// Owns exact-outcome replay, transient-target and satellite admission regressions.
// Does not infer delivery absence from lost replies or reconstruct old remote
// runtime authority. Exercises the reviewed implementation before its repairs.
use super::federation_mailbox_intent_lifecycle::intent_api;
use super::*;

async fn prepared(
    state: &AppState,
    sender: &str,
    target: &FederationMailboxParticipant,
    key: &str,
) -> FederationDelivery {
    state
        .prepare_federation_delivery(sender, FederationMailboxOperation::Send {
            target: target.session_id.clone(),
            key: key.to_owned(),
            message: "retained exact delivery".to_owned(),
            topic: None,
            state_stamp: None,
        })
        .await
        .unwrap()
}

fn mark_unknown(state: &AppState, delivery: &FederationDelivery) {
    state.mailbox_store.mark_federation_intent_unknown(delivery, || Ok(())).unwrap();
}

fn canonical_send(
    state: &AppState,
    sender: &str,
    delivery: &FederationDelivery,
) -> Result<Value, ApiError> {
    state.execute_federation_mailbox(federation_mailboxes::request(
        state, sender, FederationMailboxOperation::SendExact { delivery: delivery.clone() },
    ), None).map(|response| response.value)
}

fn reopened(state: AppState) -> AppState {
    let path = resolve_coordination_persistence_path(state.persistence_path.as_path());
    AppState { mailbox_store: Arc::new(MailboxStore::open(&path).unwrap()), ..state }
}

#[tokio::test]
async fn federation_delivery_review_r1_disconnected_target_does_not_become_terminal_refusal() {
    let state = federation_mailboxes::central_store();
    let (sender, _) = federation_mailboxes::local_participant(&state, "Sender");
    let (remote, _) = federation_mailbox_intent_remote::remote_actor(&state, "Remote", 48001);
    let target = state.inner.lock().unwrap().federation.mailbox_directory
        .get(&remote.binding.participant_key()).unwrap().clone();
    let delivery = prepared(&state, &sender, &target, "temporary-target-hold").await;
    let incarnation = state.inner.lock().unwrap().federation.links
        .remove(&remote.binding.identity.host_id).unwrap();
    assert!(state.send_prepared_federation_delivery(&sender, delivery.clone()).await.is_err());
    let (_, status) = intent_api(&state, &sender, "/temporary-target-hold", None).await;
    assert_eq!(status["state"], "appendOutcomeUnknown",
        "temporary target link loss must never certify a permanent refusal");
    assert_eq!(state.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM federation_mailbox_dedup WHERE idempotency_key='temporary-target-hold'",
        [], |row| row.get::<_, u64>(0)).unwrap(), 0);
    // Restore the SAME recorded witness to isolate a transient currency hold.
    // A new incarnation/placement still needs M4 original-runtime reconciliation.
    state.inner.lock().unwrap().federation.links
        .insert(remote.binding.identity.host_id.clone(), incarnation);
    let receipt = state.send_prepared_federation_delivery(&sender, delivery).await.unwrap();
    assert!(!receipt.duplicate);
    assert_eq!(state.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get::<_, u64>(0)).unwrap(), 1);
}

#[tokio::test]
async fn federation_delivery_review_r1_lost_commit_reply_recovers_after_target_retirement_and_reopen() {
    let state = federation_mailboxes::central_store();
    let (sender, _) = federation_mailboxes::local_participant(&state, "Sender");
    let (native_target, target) = federation_mailboxes::local_participant(&state, "Target");
    let delivery = prepared(&state, &sender, &target, "lost-committed").await;
    mark_unknown(&state, &delivery);
    let committed = canonical_send(&state, &sender, &delivery).unwrap();
    // Drop the successful reply before any sender settlement, then retire target.
    state.kill_session(&native_target).unwrap();
    let state = reopened(state);
    let (status, retry) = intent_api(&state, &sender, "/lost-committed/retry", Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK, "retained commit must survive target retirement: {retry}");
    assert_eq!(retry["messageId"], committed["messageId"]);
    assert_eq!(retry["sequence"], committed["sequence"]);
    assert_eq!(retry["duplicate"], true);
    assert_eq!(intent_api(&state, &sender, "/lost-committed", None).await.1["state"], "homeCommitted");
    let original_request_retry: MailboxAppendReceipt = state.federation_mailbox_operation(
        &sender, FederationMailboxOperation::Send {
            target: delivery.target.participant_key(), key: delivery.key.clone(),
            message: delivery.message.clone(), topic: delivery.topic.clone(), state_stamp: delivery.state_stamp.clone(),
        },
    ).await.unwrap();
    assert_eq!(original_request_retry.message_id, retry["messageId"].as_str().unwrap());
    assert!(original_request_retry.duplicate);
    let mut conflict = delivery;
    conflict.message.push_str(" changed");
    assert_eq!(canonical_send(&state, &sender, &conflict).unwrap_err().status, StatusCode::CONFLICT);
    assert_eq!(state.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get::<_, u64>(0)).unwrap(), 1);
}

#[tokio::test]
async fn federation_delivery_review_r1_lost_refusal_reply_settles_original_terminal_outcome() {
    let state = federation_mailboxes::central_store();
    let (sender, _) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    let delivery = prepared(&state, &sender, &target, "lost-refused").await;
    mark_unknown(&state, &delivery);
    // Retain a real canonical refusal while withholding its reply. A current
    // target must not allow a later retry to override this retained tombstone.
    state.mailbox_store.refuse_federation_delivery(&delivery, || Ok(())).unwrap();
    let state = reopened(state);
    let (status, _) = intent_api(&state, &sender, "/lost-refused/retry", Some(json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(intent_api(&state, &sender, "/lost-refused", None).await.1["state"], "refused");
    assert_eq!(state.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get::<_, u64>(0)).unwrap(), 0);
}

#[tokio::test]
async fn federation_delivery_review_r1_lost_cancellation_settlement_recovers_abandoned() {
    let state = federation_mailboxes::central_store();
    let (sender, _) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    let delivery = prepared(&state, &sender, &target, "lost-abandoned").await;
    let response = state.execute_federation_mailbox(federation_mailboxes::request(
        &state, &sender, FederationMailboxOperation::CancelExact { delivery: delivery.clone() },
    ), None).unwrap();
    assert_eq!(response.value["state"], "abandoned");
    // Canonical cancel committed, but its local settlement/reply is lost.
    let state = reopened(state);
    let (status, _) = intent_api(&state, &sender, "/lost-abandoned/retry", Some(json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(intent_api(&state, &sender, "/lost-abandoned", None).await.1["state"], "abandoned");
    assert_eq!(intent_api(&state, &sender, "/lost-abandoned/cancel", Some(json!({}))).await.0, StatusCode::OK);
    assert_eq!(state.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get::<_, u64>(0)).unwrap(), 0);
}

#[test]
fn federation_delivery_review_r1_satellite_rejects_bare_native_id_even_if_unique() {
    let state = federation_mailboxes::central_store();
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    let reference = target.binding.identity.session_id.clone();
    assert_eq!(resolve_federation_delivery_target(&[target], &reference).unwrap_err().status, StatusCode::NOT_FOUND);
}

#[test]
fn federation_delivery_review_r1_satellite_stale_canonical_id_never_falls_back_to_alias() {
    let state = federation_mailboxes::central_store();
    let stale = format!("domain-session-{}", Uuid::new_v4());
    let (_, target) = federation_mailboxes::local_participant(&state, &stale);
    assert_eq!(resolve_federation_delivery_target(&[target], &stale).unwrap_err().status, StatusCode::NOT_FOUND);
}

#[test]
fn federation_delivery_review_r1_satellite_exact_canonical_id_wins_over_alias_collision() {
    let state = federation_mailboxes::central_store();
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    let (_, collision) = federation_mailboxes::local_participant(&state, &target.session_id);
    let resolved = resolve_federation_delivery_target(&[target.clone(), collision], &target.session_id).unwrap();
    assert_eq!(resolved.binding, target.binding);
}

#[test]
fn federation_delivery_review_r1_control_satellite_qualified_aliases_keep_the_exact_binding() {
    let state = federation_mailboxes::central_store();
    let (_, target) = federation_mailboxes::local_participant(&state, "Visible target");
    for reference in [target.session_id.clone(), format!("host:{}::{}", target.host_name, target.name),
        format!("host:{}::{}", target.binding.identity.host_id, target.name)] {
        let found = resolve_federation_delivery_target(&[target.clone()], &reference).unwrap();
        assert_eq!(found.binding, target.binding);
    }
}

#[tokio::test]
async fn federation_delivery_review_r1_control_current_target_duplicate_keeps_message_identity() {
    let state = federation_mailboxes::central_store();
    let (sender, _) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    let delivery = prepared(&state, &sender, &target, "current-control").await;
    mark_unknown(&state, &delivery);
    let committed = canonical_send(&state, &sender, &delivery).unwrap();
    let receipt = state.send_prepared_federation_delivery(&sender, delivery).await.unwrap();
    assert_eq!(Some(receipt.message_id.as_str()), committed["messageId"].as_str());
    assert!(receipt.duplicate);
}

fn satellite_intent_fixture() -> (AppState, String, FederationDelivery) {
    let base = federation_registry::domain_state();
    let path = resolve_coordination_persistence_path(base.persistence_path.as_path());
    let state = AppState { mailbox_store: Arc::new(MailboxStore::open(&path).unwrap()), ..base };
    let session = test_session_id(&state, Agent::Codex);
    let central = FederationHostIdentity { host_id: Uuid::new_v4().to_string(), host_generation: 1 };
    state.configure_federation(FederationSettings {
        enabled: true, role: FederationRole::Satellite, central: Some(central.clone()),
    }).unwrap();
    let mut record = federation_registry::peer(48001);
    record.identity = central.clone();
    state.enroll_federation_peer(record).unwrap();
    let enrolled = state.federation_peer_snapshot(&central.host_id).unwrap();
    let connected = state.federation_peer_connected(&enrolled, Uuid::new_v4().to_string()).unwrap();
    assert_eq!(connected.membership, FederationMembershipState::Enrolled);
    let (sender, creation) = {
        let mut inner = state.inner.lock().unwrap();
        let local = inner.federation.local.clone().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        let creation = record.federation_native_creation_id.clone();
        let sender = FederationMailboxBinding {
            central: central.clone(),
            identity: FederationSessionIdentity { host_id: local.host_id,
                session_id: session.clone(), creation_id: creation.clone() },
            placement_generation: 1,
        };
        // Fixture-only already-attested native binding; no transport is used.
        record.federation_mailbox_binding = Some(sender.clone());
        (sender, creation)
    };
    let delivery = FederationDelivery { version: 1, sender,
        target: FederationMailboxBinding { central: central.clone(),
            identity: FederationSessionIdentity { host_id: central.host_id,
                session_id: "session-8".to_owned(), creation_id: Uuid::new_v4().to_string() },
            placement_generation: 1 },
        key: "blocked-lookup".to_owned(), message: "retained exact delivery".to_owned(), topic: None, state_stamp: None };
    delivery.validate().unwrap();
    state.mailbox_store.prepare_federation_intent(&delivery, &session, &creation, || Ok(())).unwrap();
    (state, session, delivery)
}

#[test]
fn federation_delivery_review_r1_satellite_lookup_yields_before_shared_sqlite_mutex_is_released() {
    use std::future::Future;
    let (state, sender, delivery) = satellite_intent_fixture();
    let connection = state.mailbox_store.connection().unwrap();
    let app = state.clone();
    let (yielded_tx, yielded_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            let mut request = Box::pin(app.prepare_federation_delivery(&sender, FederationMailboxOperation::Send {
                target: delivery.target.participant_key(), key: delivery.key, message: delivery.message,
                topic: None, state_stamp: None,
            }));
            let first = std::future::poll_fn(|cx| {
                let first = request.as_mut().poll(cx);
                yielded_tx.send(()).unwrap();
                std::task::Poll::Ready(first)
            }).await;
            match first {
                std::task::Poll::Ready(result) => result,
                std::task::Poll::Pending => request.await,
            }
        })
    });
    // The executor must return from its first poll while storage is STILL held.
    // Always release before asserting, so an old blocking implementation exits.
    let yielded_while_locked = yielded_rx.recv_timeout(Duration::from_secs(1)).is_ok();
    drop(connection);
    let prepared = worker.join().unwrap();
    assert!(prepared.is_ok(), "fixture/preparation must remain valid after release");
    assert!(yielded_while_locked, "satellite SQLite lookup blocked the current-thread executor");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_delivery_review_r1_satellite_prepare_api_preserves_qualified_identity_boundary() {
    use super::federation_mailbox_process::Satellite;
    let central = federation_mailboxes::central_store();
    // Leave session-1 unbound so the remote native id does not make the bare
    // central target ambiguous: this must reject even a UNIQUE native match.
    test_session_id(&central, Agent::Codex);
    let (native_target, target) = federation_mailboxes::local_participant(&central, "Target");
    federation_mailboxes::local_participant(&central, &target.session_id);
    let stale = format!("domain-session-{}", Uuid::new_v4());
    federation_mailboxes::local_participant(&central, &stale);
    let (satellite, native) = tokio::task::spawn_blocking(Satellite::spawn).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (record, channel) = federation_mailbox_native_link::channel(&central, &native, port);
    let check = channel.clone();
    let router = federation_router(channel.clone(), Arc::new(move || check.current_authority().map(|_| ())));
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let server = tokio::spawn(async move {
        axum::serve(listener, router).with_graceful_shutdown(async move {
            federation_wait_stop(&mut stopped).await
        }).await.unwrap();
    });
    let (satellite, satellite_port, control) = federation_mailbox_native_link::bind(
        &central, &channel, satellite, &native, record, port,
    ).await;
    let client = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10)).build().unwrap();
    let url = format!("http://127.0.0.1:{satellite_port}/api/sessions/{}/mailbox-intents", native.session);
    for (reference, key) in [(native_target, "bare"), (stale, "stale")] {
        let response = client.post(&url).json(&json!({"targetSessionId":reference,
            "idempotencyKey":key,"message":"must not prepare an unintended recipient"})).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND,
            "satellite prepare must reject canonical-looking references before alias fallback");
    }
    let response = client.post(&url).json(&json!({"targetSessionId":target.session_id,
        "idempotencyKey":"exact","message":"canonical id takes precedence"})).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let prepared: Value = response.json().await.unwrap();
    assert_eq!(prepared["state"], "localIntentDurable");
    let central_messages: u64 = central.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get(0)).unwrap();
    assert_eq!(central_messages, 0, "preparation is not a canonical send receipt");
    tokio::task::spawn_blocking(move || satellite.finish()).await.unwrap();
    stop.send_replace(true);
    server.await.unwrap();
    control.finish().await;
}

#[tokio::test]
async fn federation_delivery_review_r1_new_refusal_rechecks_retirement_next_to_commit() {
    let state = federation_mailboxes::central_store();
    let (sender, _) = federation_mailboxes::local_participant(&state, "Sender");
    let (remote, _) = federation_mailbox_intent_remote::remote_actor(&state, "Remote", 48001);
    let target = state.inner.lock().unwrap().federation.mailbox_directory
        .get(&remote.binding.participant_key()).unwrap().clone();
    let delivery = prepared(&state, &sender, &target, "refusal-fence").await;
    let incarnation = state.inner.lock().unwrap().federation.links
        .remove(&remote.binding.identity.host_id).unwrap();
    let restored = state.clone();
    FEDERATION_DELIVERY_TEST_CUT.with(|slot| {
        *slot.borrow_mut() = Some(("beforeCentralRefusalCommit", Box::new(move || {
            restored.inner.lock().unwrap().federation.links
                .insert(remote.binding.identity.host_id, incarnation);
        })));
    });
    // Run canonical execution on this thread so the transaction hook executes.
    let result = canonical_send(&state, &sender, &delivery);
    let hook_consumed = FEDERATION_DELIVERY_TEST_CUT.with(|slot| slot.borrow_mut().take().is_none());
    assert!(hook_consumed, "the real refusal transaction must reach the commit fence");
    assert!(result.is_err(), "restored currency is not proof of definitive retirement");
    assert_eq!(state.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM federation_mailbox_dedup WHERE idempotency_key='refusal-fence'",
        [], |row| row.get::<_, u64>(0)).unwrap(), 0);
    assert!(canonical_send(&state, &sender, &delivery).unwrap().get("messageId").is_some());
}

#[tokio::test]
async fn federation_delivery_review_r1_local_settlement_failure_reports_the_committed_append() {
    let state = federation_mailboxes::central_store();
    let (sender, _) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    let delivery = prepared(&state, &sender, &target, "local-settlement-loss").await;
    state.mailbox_store.connection().unwrap().execute_batch(
        "CREATE TRIGGER lose_sender_settlement BEFORE UPDATE OF state ON federation_mailbox_intents
         WHEN NEW.state='homeCommitted' BEGIN SELECT RAISE(ABORT,'injected local settlement failure'); END;",
    ).unwrap();
    let error = state.send_prepared_federation_delivery(&sender, delivery.clone()).await.unwrap_err();
    assert!(error.message.contains("central append committed"), "{}", error.message);
    assert!(!error.message.contains("no mailbox write was committed"));
    assert_eq!(intent_api(&state, &sender, "/local-settlement-loss", None).await.1["state"], "appendOutcomeUnknown");
    assert_eq!(state.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get::<_, u64>(0)).unwrap(), 1);
    state.mailbox_store.connection().unwrap().execute_batch("DROP TRIGGER lose_sender_settlement;").unwrap();
    let receipt = state.send_prepared_federation_delivery(&sender, delivery).await.unwrap();
    assert!(receipt.duplicate);
}

#[tokio::test]
async fn federation_delivery_review_r1_hidden_native_target_is_not_retired() {
    let state = federation_mailboxes::central_store();
    let (sender, _) = federation_mailboxes::local_participant(&state, "Sender");
    let (native, target) = federation_mailboxes::local_participant(&state, "Target");
    let delivery = prepared(&state, &sender, &target, "hidden-target").await;
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&native).unwrap();
        assert_ne!(inner.sessions[index].federation_native_creation_id, target.binding.identity.creation_id);
        assert_eq!(inner.sessions[index].federation_mailbox_binding.as_ref(), Some(&target.binding));
        inner.session_mut_by_index(index).unwrap().hidden = true;
    }
    assert!(state.send_prepared_federation_delivery(&sender, delivery.clone()).await.is_err());
    assert_eq!(intent_api(&state, &sender, "/hidden-target", None).await.1["state"], "appendOutcomeUnknown");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&native).unwrap();
        inner.session_mut_by_index(index).unwrap().hidden = false;
    }
    assert!(!state.send_prepared_federation_delivery(&sender, delivery).await.unwrap().duplicate);
}

#[tokio::test]
async fn federation_delivery_review_r1_unbound_native_target_is_not_retired() {
    let state = federation_mailboxes::central_store();
    let (sender, _) = federation_mailboxes::local_participant(&state, "Sender");
    let (native, target) = federation_mailboxes::local_participant(&state, "Target");
    let delivery = prepared(&state, &sender, &target, "unbound-target").await;
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&native).unwrap();
        inner.session_mut_by_index(index).unwrap().federation_mailbox_binding = None;
    }
    assert!(state.send_prepared_federation_delivery(&sender, delivery.clone()).await.is_err());
    assert_eq!(intent_api(&state, &sender, "/unbound-target", None).await.1["state"], "appendOutcomeUnknown");
    assert_eq!(state.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM federation_mailbox_dedup", [], |row| row.get::<_, u64>(0)).unwrap(), 0);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&native).unwrap();
        inner.session_mut_by_index(index).unwrap().federation_mailbox_binding = Some(target.binding);
    }
    assert!(!state.send_prepared_federation_delivery(&sender, delivery).await.unwrap().duplicate);
}

#[tokio::test]
async fn federation_delivery_review_r1_completed_exact_remote_placement_is_terminal_refusal() {
    let state = federation_mailboxes::central_store();
    let (sender, _) = federation_mailboxes::local_participant(&state, "Sender");
    let (remote, _) = federation_mailbox_intent_remote::remote_actor(&state, "Remote", 48001);
    let target = state.inner.lock().unwrap().federation.mailbox_directory
        .get(&remote.binding.participant_key()).unwrap().clone();
    let delivery = prepared(&state, &sender, &target, "completed-remote").await;
    {
        let mut inner = state.inner.lock().unwrap();
        let placement = inner.federation.placements.values_mut().find(|row| {
            row.identity == target.binding.identity && row.placement_generation == target.binding.placement_generation
        }).unwrap();
        // Fixture supplies the explicit terminal runtime reconciliation result,
        // unlike the link-only currency hold exercised by the RED regression.
        placement.state = FederationPlacementState::Completed;
    }
    assert!(state.send_prepared_federation_delivery(&sender, delivery).await.is_err());
    assert_eq!(intent_api(&state, &sender, "/completed-remote", None).await.1["state"], "refused");
    assert_eq!(state.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get::<_, u64>(0)).unwrap(), 0);
}
