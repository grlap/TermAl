// Public local sends keep their native owner independently of domain routing.
// Disposable hosts only; provider execution and live delivery are not exercised.
use super::*;
use super::regressions::public_send;

#[tokio::test]
async fn durability_history_keeps_transitions_across_idle_worker_ticks() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for condition in ["disabled", "standalone", "satellite", "durability"] {
        let state = federation_mailboxes::central_store();
        let (_, participant) = federation_mailboxes::local_participant(&state, "Idle observer");
        let before = {
            let mut inner = state.inner.lock().unwrap();
            inner.set_federation_durable(false, "test_hold");
            inner.set_federation_durable(true, "test_release");
            match condition {
                "disabled" => inner.preferences.federation.enabled = false,
                "standalone" => inner.preferences.federation.role = FederationRole::Standalone,
                "satellite" => inner.preferences.federation.role = FederationRole::Satellite,
                "durability" => inner.set_federation_durable(false, "test_hold"),
                _ => unreachable!(),
            }
            inner.federation_durability_diagnostics.snapshot()
        };
        let mut drain = FederationExecutorWakeDrain::default();
        let mut wakes = FederationMailboxWakeWorker::default();
        let mut relay = MailboxRelayWakeWorker::default();
        for _ in 0..70 {
            state.federation_executor_wake_signal.kick();
            drain.tick(&state);
            // Join the actual outcome poll for this tick, without timers.
            if let Some(poll) = drain.outcome_poll.take() {
                poll.await.unwrap();
            }
            wakes.tick(&state, true).await;
            relay.tick(&state, true).await;
            if let Some(task) = relay.task.take() {
                task.await.unwrap();
            }
            assert_eq!(state.run_native_mailbox_wake_batch(0), (0, false));
            assert!(state.native_mailbox_relay_home_checked(false).is_err());
            let inner = state.inner.lock().unwrap();
            assert!(!federation_mailbox_participant_current_locked(&inner, &participant));
        }
        drain.shutdown().await;
        wakes.shutdown().await;
        relay.shutdown().await;
        assert_eq!(state.inner.lock().unwrap().federation_durability_diagnostics.snapshot(), before,
            "idle gates and predicates must preserve real transition history: {condition}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durability_diagnostics_answers_while_state_is_locked() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = federation_mailboxes::central_store();
    let observer = state.inner.lock().unwrap().federation_durability_diagnostics.snapshot();
    let (held_tx, held_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let holder_state = state.clone();
    let holder = std::thread::spawn(move || {
        let _held = holder_state.inner.lock().unwrap();
        held_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    held_rx.await.unwrap();
    let request_state = state.clone();
    let mut request = tokio::spawn(async move {
        get_mailbox_relay_wake_diagnostics(State(request_state)).await
    });
    // A broken blocking handler is released before the assertion and teardown.
    let result = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, &mut request).await;
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    let response = result.expect("diagnostics must answer before the state holder releases")
        .unwrap().unwrap().0;
    assert_eq!(response["federationDurability"]["available"], false);
    assert_eq!(response["federationDurability"]["reason"], "state_lock_busy");
    assert_eq!(response["serverInstanceId"], state.server_instance_id);
    assert_eq!(response["pendingHomeWakes"], 0);
    assert_eq!(response["homeRpcFailures"], json!([]));
    assert_eq!(response["refusedHomeWakes"], json!([]));
    let recovered = get_mailbox_relay_wake_diagnostics(State(state.clone())).await.unwrap().0;
    assert_eq!(recovered["federationDurability"], observer);
}

#[tokio::test]
async fn durability_history_resets_on_persisted_reconstruction() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = federation_mailboxes::central_store();
    let (persisted, previous) = {
        let mut inner = state.inner.lock().unwrap();
        inner.set_federation_durable(false, "test_hold");
        (serde_json::to_vec(&PersistedState::from_inner(&inner)).unwrap(),
            inner.federation_durability_diagnostics.snapshot())
    };
    let mut restored = serde_json::from_slice::<PersistedState>(&persisted).unwrap()
        .into_inner().unwrap();
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    bind_loaded_federation(&mut restored, &connection, state.persistence_path.as_path()).unwrap();
    let fresh = restored.federation_durability_diagnostics.snapshot();
    assert_ne!(fresh["observerId"], previous["observerId"]);
    assert_eq!(fresh["evicted"], 0);
    let records = fresh["records"].as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["kind"], "initial");
    assert_eq!(records[0]["site"], "state_restored");
    assert_eq!(records[0]["durable"], true);
    assert!(records[0]["windowId"].is_null());
    assert!(records[0]["falseForMs"].is_null());
    *state.inner.lock().unwrap() = restored;
    let response = get_mailbox_relay_wake_diagnostics(State(state.clone())).await.unwrap().0;
    assert_eq!(response["federationDurability"], fresh);
    assert_eq!(response["serverInstanceId"], state.server_instance_id);
}

#[tokio::test]
async fn bound_directory_refusal_recovers_after_exact_persistence_reconciliation() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = federation_mailboxes::central_store();
    let (caller, participant) = federation_mailboxes::local_participant(&state, "Directory caller");
    let instance = state.server_instance_id.clone();
    let initial = list_federation_mailbox_directory(AxumPath(caller.clone()), State(state.clone()))
        .await.unwrap().0;
    assert!(initial.participants.iter().any(|row| row.binding == participant.binding));
    let diagnostic_before = state.inner.lock().unwrap().federation_durability_diagnostics.snapshot();
    list_federation_mailbox_directory(AxumPath(caller.clone()), State(state.clone())).await.unwrap();
    assert_eq!(state.inner.lock().unwrap().federation_durability_diagnostics.snapshot(), diagnostic_before,
        "healthy discovery of current participants emits no transition or refusal");
    let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_directory_state BEFORE INSERT ON state_metadata_units BEGIN SELECT RAISE(ABORT,'fixture persistence failure'); END;").unwrap();
    let settings = state.inner.lock().unwrap().preferences.federation.clone();
    assert!(state.configure_federation(settings).is_err());
    assert!(!state.inner.lock().unwrap().federation_durable);
    let refusal = list_federation_mailbox_directory(AxumPath(caller.clone()), State(state.clone()))
        .await.err().expect("unsettled authority must refuse bound discovery");
    assert_eq!(refusal.status, StatusCode::CONFLICT);
    assert_eq!(refusal.kind, Some(ApiErrorKind::FederationMailboxCentralDurability));
    assert!(refusal.message.contains("central mailbox authority is not durable"));
    assert!(refusal.message.contains(&caller));
    assert!(refusal.message.contains(&participant.session_id));
    assert!(refusal.message.contains("persistence reconciliation"));
    assert!(!refusal.message.contains("binding is stale"));
    let held = get_mailbox_relay_wake_diagnostics(State(state.clone())).await.unwrap().0;
    let records = held["federationDurability"]["records"].as_array().unwrap();
    let refused = records.iter().rev().find(|row| row["kind"] == "refusal").unwrap();
    let transition = records.iter().rev().find(|row| row["kind"] == "transition" && row["durable"] == false).unwrap();
    assert_eq!(refused["status"], 409);
    assert_eq!(refused["stage"], "central_authority_durability");
    assert_eq!(refused["windowId"], transition["windowId"]);
    connection.execute_batch("DROP TRIGGER reject_directory_state;").unwrap();
    drop(connection);
    state.reconcile_federation_durability().unwrap();
    let recovered = list_federation_mailbox_directory(AxumPath(caller.clone()), State(state.clone()))
        .await.unwrap().0;
    assert!(recovered.participants.iter().any(|row| row.binding == participant.binding));
    let recovered_diagnostic = state.inner.lock().unwrap().federation_durability_diagnostics.snapshot();
    let finished = recovered_diagnostic["records"].as_array().unwrap().iter().rev()
        .find(|row| row["kind"] == "transition").unwrap();
    assert_eq!(finished["durable"], true);
    assert_eq!(finished["windowId"], refused["windowId"]);
    assert_eq!(state.federation_mailbox_actor(&caller).unwrap().unwrap().binding, participant.binding);
    assert_eq!(state.server_instance_id, instance, "recovery must use the same running host");
    assert!(state.inner.lock().unwrap().federation.placements.is_empty());
}

#[tokio::test]
async fn bound_directory_missing_entry_names_context_without_rebinding_the_caller() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = federation_mailboxes::central_store();
    let (caller, participant) = federation_mailboxes::local_participant(&state, "Missing directory entry");
    // Session creation persists state, so prepare the other root before
    // injecting an inconsistent same-version directory image.
    let replacement = native(&state, "Current directory caller");
    assert!(state.federation_mailbox_actor(&replacement).unwrap().is_none());
    let instance = state.server_instance_id.clone();
    state.inner.lock().unwrap().federation.mailbox_directory.remove(&participant.session_id);
    let refusal = list_federation_mailbox_directory(AxumPath(caller.clone()), State(state.clone()))
        .await.err().expect("a bound caller cannot replace its absent directory identity");
    assert_eq!(refusal.status, StatusCode::CONFLICT);
    assert!(refusal.message.contains(&participant.session_id));
    assert!(refusal.message.contains("observed directory entry is missing or differs"));
    assert!(refusal.message.contains("another current root session on this host"));
    let recovered = list_federation_mailbox_directory(AxumPath(replacement.clone()), State(state.clone()))
        .await.unwrap().0;
    assert!(recovered.participants.iter().any(|row| row.binding.identity.session_id == replacement));
    assert!(!recovered.participants.iter().any(|row| row.session_id == participant.session_id));
    let inner = state.inner.lock().unwrap();
    assert_eq!(federation_mailbox_root_locked(&inner, &caller).unwrap().mailbox_binding(), Some(&participant.binding));
    assert!(!inner.federation.mailbox_directory.contains_key(&participant.session_id));
    assert!(inner.federation.placements.is_empty());
    assert_eq!(state.server_instance_id, instance);
}

#[test]
fn directory_conflict_after_central_recovery_does_not_label_a_healthy_binding_stale() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = federation_mailboxes::central_store();
    let (caller, participant) = federation_mailboxes::local_participant(&state, "Race directory caller");
    let instance = state.server_instance_id.clone();
    let authority = FederationMailboxAuthority {
        app: state.clone(), actor: participant.binding.clone(), lease: None, target: None,
    };
    state.inner.lock().unwrap().federation_durable = false;
    // The commit-adjacent gate intentionally erases the earlier central cause.
    // Settle that real condition before the directory boundary observes it.
    let refusal = mailbox_api_error(authority.lock().err().expect("the authority hold must refuse"));
    assert_eq!(refusal.status, StatusCode::CONFLICT);
    assert_eq!(refusal.kind, None);
    state.reconcile_federation_durability().unwrap();
    let refusal = state.federation_mailbox_directory_refusal(&caller, refusal);
    assert!(refusal.message.contains(&participant.session_id));
    assert!(refusal.message.contains("central mailbox authority is currently available"));
    assert!(refusal.message.contains("directory/binding authority changed or is unresolved"));
    assert!(!refusal.message.contains("binding is stale"));
    assert!(!refusal.message.contains("another current root"));
    assert!(!refusal.message.contains("wait for the existing federation persistence reconciliation"));
    assert!(authority.lock().is_ok(), "the original binding's rights remain current");
    assert_eq!(state.federation_mailbox_actor(&caller).unwrap().unwrap().binding, participant.binding);
    assert_eq!(state.server_instance_id, instance);
    let unrelated = state.federation_mailbox_directory_refusal(&caller, ApiError::not_found("fixture missing resource"));
    assert_eq!(unrelated.status, StatusCode::NOT_FOUND);
    assert_eq!(unrelated.message, "fixture missing resource");
}

#[tokio::test]
async fn unbound_central_native_sends_bypass_unavailable_domain_authority() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for unavailable in ["durability", "continuity", "quarantine"] {
        let state = federation_mailboxes::central_store();
        let sender = native(&state, "Local sender");
        let target = native(&state, "Local recipient");
        let host = state.inner.lock().unwrap().federation.local.clone().unwrap();
        {
            let mut inner = state.inner.lock().unwrap();
            match unavailable {
                "durability" => inner.federation_durable = false,
                "continuity" => inner.federation_continuity_lost = true,
                "quarantine" => inner.federation.admission_quarantine = Some(FederationQuarantineReason::DeclaredRestore),
                _ => unreachable!(),
            }
        }
        // A display alias selects domain discovery even when both roots are
        // local and unbound. Explicit native locators require no domain join.
        let refused = public_send(&state, &sender, "Local recipient", "alias-attempt", "local report")
            .await.unwrap_err();
        assert_eq!(refused.status, StatusCode::CONFLICT, "{unavailable}");
        assert!(refused.message.contains(FEDERATION_STALE_AUTHORITY), "{unavailable}: {}", refused.message);
        let expected_reason = match unavailable {
            "durability" => "not durable",
            "continuity" => "continuity is lost",
            "quarantine" => "quarantined",
            _ => unreachable!(),
        };
        assert!(refused.message.contains(expected_reason), "{unavailable}: {}", refused.message);
        let sent = public_send(&state, &sender, &target, "alias-attempt", "local report")
            .await.unwrap_or_else(|error| panic!("{unavailable}: {}", error.message));
        assert!(!sent.duplicate);
        let replay = public_send(&state, &sender, &target, "alias-attempt", "local report").await.unwrap();
        assert!(replay.duplicate);
        assert_eq!(replay.message_id, sent.message_id);
        let qualified = format!("host:{}::{target}", host.host_id);
        let second = public_send(&state, &sender, &qualified, "qualified-local", "another report").await.unwrap();
        assert_ne!(second.message_id, sent.message_id);
        let inner = state.inner.lock().unwrap();
        for id in [&sender, &target] {
            assert!(federation_mailbox_root_locked(&inner, id).unwrap().mailbox_binding().is_none(), "{unavailable}");
        }
        drop(inner);
        let connection = state.mailbox_store.connection().unwrap();
        assert_eq!(connection.query_row("SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get::<_, u64>(0)).unwrap(), 2);
        assert_eq!(connection.query_row("SELECT COUNT(*) FROM federation_mailbox_intents", [], |row| row.get::<_, u64>(0)).unwrap(), 0);
    }
}

pub(super) async fn acknowledge_native(state: &AppState, recipient: &str, sent: &MailboxAppendReceipt) {
    let page = read_mailbox(AxumPath((recipient.into(), sent.mailbox_id.clone())), State(state.clone()),
        Json(ReadMailboxRequest { after_sequence: None, limit: 20, issue_receipt: true, read_key: None }))
        .await.unwrap().0;
    assert!(page.messages.iter().any(|message| message.id == sent.message_id));
    let exact = read_mailbox_message(AxumPath((recipient.into(), sent.message_id.clone())),
        State(state.clone())).await.unwrap().0;
    assert_eq!(exact.id, sent.message_id);
    let ack = acknowledge_mailbox(AxumPath((recipient.into(), sent.mailbox_id.clone())), State(state.clone()),
        Json(AcknowledgeMailboxRequest { receipt: page.receipt, expected_processed_through: None,
            processed_through: None, no_continuation: false })).await.unwrap().0;
    assert!(ack.participants.iter().any(|row| row.session_id == recipient && row.processed_through == sent.sequence));
}

async fn unavailable_satellite_local_send(bound: bool) {
    let pair = qualified_pair().await;
    let sender = native(&pair.satellite, "Local sender");
    let target = native(&pair.satellite, "Local recipient");
    let remote = native(&pair.central, "Domain recipient");
    if bound { directory(&pair.satellite, &sender).await; }
    let remote_binding = if bound {
        Some(pair.central.federation_mailbox_actor(&remote).unwrap().unwrap().binding.participant_key())
    } else { None };
    let diagnostic = pair.satellite.federation_mailbox_directory_refusal(
        &sender, ApiError::conflict(FEDERATION_STALE_AUTHORITY));
    let central = pair.central.inner.lock().unwrap().federation.local.clone().unwrap();
    assert!(diagnostic.message.contains(&central.host_id));
    assert!(diagnostic.message.contains("condition is not observed locally on this host"));
    assert!(!diagnostic.message.contains("this host is not the central mailbox authority"));
    assert!(!diagnostic.message.contains("another current root"));
    pair.satellite.federation_peer_handles.lock().unwrap().clear();
    let before = pair.satellite.inner.lock().unwrap().sessions.iter()
        .map(|row| row.mailbox_binding().cloned()).collect::<Vec<_>>();
    let sent = public_send(&pair.satellite, &sender, &target, "local-offline", "local mail").await.unwrap();
    acknowledge_native(&pair.satellite, &target, &sent).await;
    let retry = public_send(&pair.satellite, &sender, &target, "local-offline", "local mail").await.unwrap();
    assert!(retry.duplicate);
    assert_eq!(retry.message_id, sent.message_id);
    let domain = public_send(&pair.satellite, &sender,
        remote_binding.as_deref().unwrap_or("Domain recipient"), "domain-offline", "must refuse").await;
    assert!(domain.is_err(), "domain targets still require automatic participation");
    let connection = pair.satellite.mailbox_store.connection().unwrap();
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get::<_, u64>(0)).unwrap(), 1);
    drop(connection);
    assert_eq!(before, pair.satellite.inner.lock().unwrap().sessions.iter()
        .map(|row| row.mailbox_binding().cloned()).collect::<Vec<_>>(), "local sends cannot admit or change domain bindings");
    pair.finish().await;
}

#[tokio::test]
async fn unbound_satellite_local_send_does_not_require_a_central_handle() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    unavailable_satellite_local_send(false).await;
}

#[tokio::test]
async fn automatically_bound_satellite_local_send_does_not_require_a_central_handle() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    unavailable_satellite_local_send(true).await;
}

#[tokio::test]
async fn local_native_send_does_not_need_host_verification() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let pair = federation_onboarding_fixture::Pair::with_enrolled_profile(
        federation_mailboxes::central_store(), federation_onboarding_fixture::io(),
        federation_onboarding_fixture::io(), federation_onboarding_fixture::profile(),
    ).await;
    let sender = native(&pair.satellite, "Local sender");
    let target = native(&pair.satellite, "Local recipient");
    let sent = public_send(&pair.satellite, &sender, &target, "unverified-local", "local").await.unwrap();
    acknowledge_native(&pair.satellite, &target, &sent).await;
    assert!(pair.satellite.federation_mailbox_actor(&sender).unwrap().is_none());
    assert!(public_send(&pair.satellite, &sender, "Remote", "unverified-domain", "refuse").await.is_err());
    assert!(federation_mailbox_root_locked(&pair.satellite.inner.lock().unwrap(), &sender)
        .unwrap().federation_mailbox_binding.is_none());
    pair.finish().await;
}

#[tokio::test]
async fn explicitly_bound_native_roots_keep_public_bare_sends_local() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = federation_mailboxes::central_store();
    let (sender, _) = federation_mailboxes::local_participant(&state, "Explicit sender");
    let (target, _) = federation_mailboxes::local_participant(&state, "Explicit recipient");
    let sent = public_send(&state, &sender, &target, "explicit-local", "local").await.unwrap();
    acknowledge_native(&state, &target, &sent).await;
    assert_eq!(state.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM federation_mailbox_intents", [], |row| row.get::<_, u64>(0)).unwrap(), 0);
}

#[tokio::test]
async fn local_and_domain_retries_keep_their_original_namespace_and_receipt() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = federation_mailboxes::central_store();
    let sender = native(&state, "Sender");
    let target = native(&state, "Target");
    let domain = public_send(&state, &sender, "Target", "domain-key", "domain").await.unwrap();
    let retry = public_send(&state, &sender, &target, "domain-key", "domain").await.unwrap();
    assert!(retry.duplicate);
    assert_eq!(retry.message_id, domain.message_id);
    let local = public_send(&state, &sender, &target, "local-key", "local").await.unwrap();
    assert_ne!(local.mailbox_id, domain.mailbox_id);
    acknowledge_native(&state, &target, &local).await;
    assert!(public_send(&state, &sender, "Target", "local-key", "local").await.is_err());
    let retry = public_send(&state, &sender, &target, "local-key", "local").await.unwrap();
    assert_eq!(retry.message_id, local.message_id);
    let connection = state.mailbox_store.connection().unwrap();
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get::<_, u64>(0)).unwrap(), 2);
}

#[tokio::test]
async fn concurrent_local_and_domain_sends_cannot_claim_the_same_sender_key() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = federation_mailboxes::central_store();
    let sender = native(&state, "Sender");
    let target = native(&state, "Target");
    directory(&state, &sender).await;
    let (local, domain) = tokio::join!(
        public_send(&state, &sender, &target, "same-key", "local"),
        public_send(&state, &sender, "Target", "same-key", "domain"));
    assert_ne!(local.is_ok(), domain.is_ok(), "local={local:?}, domain={domain:?}");
    let connection = state.mailbox_store.connection().unwrap();
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get::<_, u64>(0)).unwrap(), 1);
}
