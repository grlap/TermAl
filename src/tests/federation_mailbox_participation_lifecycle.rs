// Public routing and current-authority transition counterexamples.
// The disposable authenticated hosts never contact providers or live stores.
use super::*;

#[tokio::test]
async fn aliases_include_new_eligible_roots_before_send_or_get_or_create() {
    for target in ["Inbox", "host:Central::Inbox"] {
        let state = federation_mailboxes::central_store();
        let from = native(&state, "Sender");
        native(&state, "Inbox");
        directory(&state, &from).await;
        native(&state, "Inbox");
        let sent = public_send(&state, &from, target, "ambiguous", "must not append").await;
        let created: Result<MailboxSummary, ApiError> = state.federation_mailbox_operation(
            &from, FederationMailboxOperation::GetOrCreate { target: target.into() }).await;
        assert!(sent.is_err(), "new eligible names must participate in ambiguity: {sent:?}");
        assert!(created.is_err(), "GetOrCreate must reject the same ambiguity");
        assert_eq!(state.mailbox_store.connection().unwrap().query_row(
            "SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get::<_, u64>(0)).unwrap(), 0);
    }
}

async fn routing_after_authority_change(reenroll: bool) {
    let pair = qualified_pair().await;
    let c = native(&pair.central, "C");
    let s = native(&pair.satellite, "S");
    directory(&pair.central, &c).await;
    let central_target = pair.central.federation_mailbox_actor(&c).unwrap().unwrap().binding.participant_key();
    let prior = pair.satellite.federation_mailbox_actor(&s).unwrap().unwrap().binding;
    let historical = send(&pair.central, &c, "S", "prior-unacknowledged-domain-mail").await;
    let issued = read_mailbox(AxumPath((s.clone(), historical.mailbox_id.clone())),
        State(pair.satellite.clone()), Json(ReadMailboxRequest {
            read_key: None, issue_receipt: true, after_sequence: None, limit: 20,
        })).await.unwrap().0;
    let retained_receipt = issued.receipt.expect("prior domain page must issue a receipt");
    let id = pair.id.clone();
    let (central, satellite, listeners) = pair.suspend().await;
    if reenroll {
        let peer = central.inner.lock().unwrap().federation.peers[&id].clone();
        central.enroll_federation_peer(peer).unwrap();
        let reverse = satellite.inner.lock().unwrap().federation.peers.values().next().unwrap().clone();
        satellite.enroll_federation_peer(reverse).unwrap();
    } else {
        let settings = central.inner.lock().unwrap().preferences.federation.clone();
        let mut disabled = settings.clone();
        disabled.enabled = false;
        central.configure_federation(disabled).unwrap();
        central.configure_federation(settings).unwrap();
    }
    let pair = federation_onboarding_fixture::Pair::resume(central, satellite, listeners,
        federation_onboarding_fixture::io(), federation_onboarding_fixture::io()).await;
    // No participant-directory operation is allowed to repair this first send.
    let sent = public_send(&pair.satellite, &s, &central_target, "new-authority-first-send", "current route").await;
    if let Err(error) = sent {
        pair.finish().await;
        panic!("ordinary send must re-admit automatic routing: {error:?}");
    }
    let listed = list_mailboxes_with_availability(&s, &pair.satellite).await.unwrap();
    assert!(!listed.mailboxes.is_empty());
    let availability = serde_json::to_value(&listed).unwrap();
    let prior_visible = availability["unavailablePriorDomains"].as_array().is_some_and(|rows|
        rows.iter().any(|row| row["participantId"] == prior.participant_key()
            && row["reason"].as_str().is_some_and(|reason| !reason.is_empty())));
    let current = pair.satellite.federation_mailbox_actor(&s).unwrap().unwrap().binding;
    if reenroll {
        assert_ne!(current, prior, "new enrollment cannot borrow the retired authority");
        assert!(current.placement_generation > prior.placement_generation);
    } else {
        assert_eq!(current, prior, "a settings toggle cannot replace the enrolled mailbox identity");
    }
    assert!(pair.satellite.inner.lock().unwrap().sessions.iter().find(|row|
        row.session.id == s).unwrap().federation_mailbox_binding.is_none());
    let restored = {
        let inner = pair.satellite.inner.lock().unwrap();
        let root = inner.sessions.iter().find(|row| row.session.id == s).unwrap();
        let saved = PersistedSessionRecord::from_record(root).clone_without_messages();
        serde_json::from_value::<PersistedSessionRecord>(serde_json::to_value(saved).unwrap()).unwrap()
            .into_record().unwrap()
    };
    assert_eq!(restored.mailbox_binding(), Some(&current));
    assert!(restored.federation_mailbox_binding.is_none());
    assert_eq!(restored.federation_retired_automatic_mailbox_bindings.contains(&prior), reenroll);
    let original_page: (String, String) = pair.central.mailbox_store.connection().unwrap().query_row(
        "SELECT mailbox_id,session_id FROM mailbox_read_pages WHERE receipt=?1", [&retained_receipt],
        |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(original_page, (historical.mailbox_id.clone(), prior.participant_key()),
        "the original receipt is neither deleted nor reassigned to the replacement");
    pair.finish().await;
    assert_eq!(prior_visible, reenroll,
        "only a replaced enrollment retires unread domain history and its receipt ({retained_receipt})");
}

#[tokio::test]
async fn disconnected_qualified_peer_remains_in_alias_uniqueness() {
    let pair = qualified_pair().await;
    let sender = native(&pair.central, "Sender");
    let local = native(&pair.central, "Reviewer");
    native(&pair.satellite, "Reviewer");
    directory(&pair.central, &sender).await;
    let before = public_send(&pair.central, &sender, "Reviewer", "ambiguous-before", "must not append").await;
    assert!(before.is_err());
    let current = pair.central.federation_peer_snapshot(&pair.id).unwrap();
    assert!(pair.central.federation_peer_disconnected(&current, &pair.lease.incarnation));
    let snapshot = pair.central.mailbox_directory_snapshot(&sender).await.unwrap();
    let after = public_send(&pair.central, &sender, "Reviewer", "ambiguous-after", "must not choose local").await;
    let healthy = public_send(&pair.central, &sender, &local, "exact-local-after", "healthy exact route").await;
    let unavailable = snapshot.unavailable_hosts.iter().any(|host| host.host_id == pair.id);
    pair.finish().await;
    assert!(unavailable, "disconnected qualified peer cannot silently vanish from uniqueness");
    assert!(after.is_err(), "a disconnected namesake must not turn an ambiguous name into a delivery");
    assert!(healthy.is_ok(), "exact local native routing remains independent of peer discovery");
}

#[tokio::test]
async fn reenrollment_first_ordinary_send_readmits_automatic_routing() {
    routing_after_authority_change(true).await;
}

#[tokio::test]
async fn settings_toggle_first_send_preserves_automatic_admission_without_verification() {
    routing_after_authority_change(false).await;
}

#[tokio::test]
async fn explicit_upgrade_does_not_revive_acknowledged_mailbox_only_wakes() {
    let pair = qualified_pair().await;
    let c = native(&pair.central, "C");
    let s = native(&pair.satellite, "S");
    let sent = send(&pair.central, &c, "S", "mail-before-grant").await;
    read_ack(&pair.satellite, &s, &sent).await;
    let unread = send(&pair.central, &c, "S", "unread-before-grant").await;
    for receipt in [&sent, &unread] {
        assert_eq!(pair.central.mailbox_store.federation_executor_wake_state(&receipt.message_id).unwrap(),
            "mailbox delivery settled", "routing-only delivery has no receiver outcome");
    }
    let outcomes: u64 = pair.central.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM federation_executor_outcomes", [], |row| row.get(0)).unwrap();
    assert_eq!(outcomes, 0, "routing-only settlement must not seed a receiver-outcome poll");
    let _ = activate_federation_satellite_route(AxumPath(pair.id.clone()), State(pair.central.clone())).await.unwrap();
    let _ = reserve_federation_mailbox_placement_route(AxumPath(pair.id.clone()), State(pair.central.clone()),
        Json(FederationPlacementRequest { native_session_id: s, admission_key: "explicit-after-mail".into() }))
        .await.unwrap();
    let executable = send(&pair.central, &c, "S", "mail-after-grant").await;
    pair.central.refresh_federation_executor_reservation_domain().unwrap();
    let wakes = pair.central.mailbox_store.reserve_federation_executor_wakes().unwrap();
    let revived = wakes.iter().any(|wake| wake.wake.message_id == sent.message_id || wake.wake.message_id == unread.message_id);
    let authorized = wakes.iter().any(|wake| wake.wake.message_id == executable.message_id);
    for receipt in [&sent, &unread] {
        assert_eq!(pair.central.mailbox_store.federation_executor_wake_state(&receipt.message_id).unwrap(),
            "mailbox delivery settled", "a later grant cannot invent a historical receiver outcome");
    }
    pair.finish().await;
    assert!(!revived, "an explicit grant cannot revive pre-grant mail as executable work");
    assert!(authorized, "new mail after explicit admission must retain executor wake eligibility");
}

#[tokio::test]
async fn unavailable_unrelated_peer_does_not_hide_the_central_directory() {
    let pair = qualified_pair().await;
    let c = native(&pair.central, "C");
    let local = native(&pair.central, "Local");
    let remote = native(&pair.satellite, "Remote");
    directory(&pair.central, &c).await;
    pair.central.federation_peer_handles.lock().unwrap().clear();
    let unavailable_host = pair.id.clone();
    let result = list_federation_mailbox_directory(AxumPath(c.clone()), State(pair.central.clone())).await;
    assert!(result.is_ok(), "unrelated peer must be unavailable, not erase healthy roots: {result:?}");
    let result = result.unwrap().0;
    let default_wire = serde_json::to_value(&result).unwrap();
    assert!(default_wire["unavailableHosts"].is_array());
    assert!(default_wire.get("aliasBasis").is_none(), "the existing directory envelope stays unchanged");
    assert!(result.participants.iter().any(|row| row.binding.identity.session_id == local
        && row.binding.identity.host_id != unavailable_host));
    assert!(!result.participants.iter().any(|row| row.binding.identity.session_id == remote
        && row.binding.identity.host_id == unavailable_host));
    assert!(result.unavailable_hosts.iter().any(|host| host.host_id == unavailable_host && !host.reason.is_empty()));
    assert!(!federation_directory_alias_available("Remote", &result.unavailable_hosts, result.alias_basis.as_ref()));
    assert!(federation_directory_alias_available("host:Central::Local", &result.unavailable_hosts, result.alias_basis.as_ref()));
    let opted_in = list_federation_mailbox_participants_with_basis(AxumPath(c.clone()), State(pair.central.clone()),
        Query(FederationMailboxDirectoryQuery { alias_basis: true })).await.unwrap();
    let bytes = axum::body::to_bytes(opted_in.into_body(), usize::MAX).await.unwrap();
    let opted_in: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(opted_in["aliasBasis"]["complete"], false);
    assert!(opted_in["unavailableHosts"].as_array().unwrap().iter()
        .any(|host| host["hostId"] == unavailable_host));
    let local_binding = pair.central.federation_mailbox_actor(&local).unwrap().unwrap().binding;
    let exact = format!("host:{}::{}", local_binding.identity.host_id, local);
    for denied in ["name:Local".to_owned(), "Central::Local".to_owned(),
        format!("host:{unavailable_host}::{remote}")] {
        assert!(public_send(&pair.central, &c, &denied, &format!("denied-{denied}"), "must not append")
            .await.is_err(), "unavailable inventory cannot reinterpret {denied}");
    }
    let sent = public_send(&pair.central, &c, &exact, "healthy-exact-pair", "exact local target").await.unwrap();
    let stored_target: String = pair.central.mailbox_store.connection().unwrap().query_row(
        "SELECT target_session_id FROM mailbox_messages WHERE id=?1", [&sent.message_id], |row| row.get(0)).unwrap();
    assert_eq!(stored_target, local_binding.participant_key());
    let message_count: u64 = pair.central.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get(0)).unwrap();
    assert_eq!(message_count, 1, "only the exact healthy target may append");
    pair.finish().await;
}

#[test]
fn incomplete_directories_never_choose_a_global_or_unavailable_host_alias() {
    let unavailable = vec![FederationMailboxUnavailableHost { host_id: "peer".into(),
        host_name: "Laptop".into(), reason: "unsupported endpoint".into(), status: 409 }];
    for alias in ["Inbox", "name:Inbox", "Laptop::Inbox", "peer::Inbox", "host:Laptop::Inbox", "host:peer::Inbox"] {
        assert!(!federation_directory_alias_available(alias, &unavailable, None));
    }
    for exact in ["domain-session-current", "session-local", "host:Central::Inbox"] {
        assert!(federation_directory_alias_available(exact, &unavailable, None));
    }
    assert!(!federation_directory_alias_available("host:Central::Inbox", &[federation_directory_incomplete()], None));
}

#[test]
fn automatic_directory_availability_uses_the_shared_typed_grammar() {
    let healthy = Uuid::new_v4();
    let offline = Uuid::new_v4();
    let basis = FederationMailboxAliasBasis { complete: false, hosts: vec![
        FederationMailboxAliasHost { id: healthy.to_string(), name: "Lab::Mac".into() },
        FederationMailboxAliasHost { id: offline.to_string(), name: "Offline".into() },
    ] };
    let unavailable = vec![FederationMailboxUnavailableHost { host_id: offline.to_string(),
        host_name: "Offline".into(), reason: "disconnected".into(), status: 503 }];
    let exact = format!("host:{healthy}::session-build");
    assert!(federation_directory_alias_available(&exact, &unavailable, Some(&basis)));
    for reference in ["name:Lab::Mac::Build".to_owned(), "Lab::Mac::Build".to_owned(),
        "host:Lab::session-build".to_owned(), format!("host:{offline}::session-build")] {
        assert!(!federation_directory_alias_available(&reference, &unavailable, Some(&basis)), "{reference}");
    }
    assert!(!federation_directory_alias_available(&exact, &[federation_directory_incomplete()], Some(&basis)));
}

#[tokio::test]
async fn mailbox_only_binding_cannot_authorize_the_central_executor_mirror() {
    let pair = qualified_pair().await;
    let c = native(&pair.central, "C");
    let s = native(&pair.satellite, "S");
    directory(&pair.central, &c).await;
    let binding = pair.satellite.federation_mailbox_actor(&s).unwrap().unwrap().binding;
    let operation = FederationExecutorOperation {
        central_host_id: binding.central.host_id.clone(), central_epoch: binding.central.host_generation,
        operation_key: "routing-is-not-execution".into(), target_host_id: binding.identity.host_id.clone(),
        enrollment_generation: pair.lease.channel.snapshot.enrollment_generation,
        session_id: s, creation_id: binding.identity.creation_id.clone(),
        placement_generation: binding.placement_generation, command_kind: "wake".into(), payload: "{}".into(),
    };
    let authority = FederationExecutorMirrorAuthority { app: &pair.central, operation: &operation, lease: &pair.lease };
    let refused = authority.lock().is_err();
    pair.finish().await;
    assert!(refused, "central mirror must enforce explicit execution authority independently");
}

fn historical_delegation(index: usize) -> DelegationRecord {
    DelegationRecord {
        id: format!("historical-{index}"), parent_session_id: "parent".into(), child_session_id: format!("child-{index}"),
        mode: DelegationMode::Reviewer, status: DelegationStatus::Completed, title: "Historical".into(),
        prompt: "review".into(), cwd: ".".into(), agent: Agent::Codex, model: None,
        write_policy: DelegationWritePolicy::ReadOnly, created_at: stamp_now(), started_at: None,
        completed_at: Some(stamp_now()), result: None, submitted_review_result: None,
        post_submission_transport_error: None, base_revision: None, review_result_recovery_probe_attempt: None,
        review_result_recovery_error: None, review_result_schema_version: None, queued_followup_prompt_id: None,
        review_result_submission_attempt: 0, acceptance_evaluation: None, attempt: DelegationAttemptState::default(),
    }
}

#[test]
fn routine_delegation_updates_do_not_abort_native_catalog_enumeration() {
    let state = federation_mailboxes::central_store();
    let root = native(&state, "Root");
    let mut inner = state.inner.lock().unwrap();
    for index in 0..130 { inner.delegations.push(historical_delegation(index)); }
    let central = inner.federation.local.clone().unwrap();
    let first = federation_native_catalog_locked(&inner, None, central.clone(), central.clone(), String::new()).unwrap();
    let mut seen: HashSet<_> = first.roots.into_iter().map(|row| row.session_id).collect();
    let mut after = first.next;
    inner.mark_delegation_mutated(0).unwrap();
    while let Some(cursor) = after {
        let page = federation_native_catalog_locked(&inner, Some(&cursor), central.clone(), central.clone(), String::new());
        assert!(page.is_ok(), "routine status updates cannot invalidate discovery: {:?}", page.as_ref().err());
        let page = page.unwrap();
        seen.extend(page.roots.into_iter().map(|row| row.session_id));
        after = page.next;
    }
    assert!(seen.contains(&root));
}

#[test]
fn catalog_child_index_preserves_exclusion_across_relationship_changes_and_deletion() {
    let state = federation_mailboxes::central_store();
    let roots: Vec<_> = (0..130).map(|index| native(&state, &format!("Root {index}"))).collect();
    let mut inner = state.inner.lock().unwrap();
    for index in 0..130 {
        let mut record = historical_delegation(index);
        record.child_session_id = roots[index].clone();
        inner.delegations.push(record);
    }
    let central = inner.federation.local.clone().unwrap();
    let mut after = None;
    let mut pages = 0;
    loop {
        let page = federation_native_catalog_locked(&inner, after.as_deref(), central.clone(), central.clone(), String::new()).unwrap();
        assert!(page.roots.is_empty(), "delegation children are never native directory candidates");
        pages += 1;
        after = page.next;
        if after.is_none() { break; }
        assert!(pages < 10, "cold child indexing plus bounded session scan must finish");
    }
    // An ordinary mutation of another field cannot clear a child's exclusion.
    inner.mark_delegation_mutated(0).unwrap();
    let removed = inner.delegations.remove(0);
    inner.record_removed_delegation(removed.id);
    inner.sessions.remove(0);
    let mut found = HashSet::new();
    loop {
        let page = federation_native_catalog_locked(&inner, after.as_deref(), central.clone(), central.clone(), String::new()).unwrap();
        assert!(page.roots.len() <= FEDERATION_CATALOG_PAGE);
        found.extend(page.roots.into_iter().map(|root| root.session_id));
        after = page.next;
        if after.is_none() { break; }
    }
    // The first root was deleted; every surviving root remains a child.
    assert!(found.is_empty());
    // Changing a delegation's child updates both sides of the exclusion index.
    inner.delegations[0].child_session_id = "not-a-native-root".into();
    inner.mark_delegation_mutated(0).unwrap();
    let page = federation_native_catalog_locked(&inner, None, central.clone(), central, String::new()).unwrap();
    assert!(page.roots.iter().any(|root| root.session_id == roots[1]));
}

#[test]
fn cold_catalog_advances_during_structural_delegation_churn() {
    let state = federation_mailboxes::central_store();
    let root = native(&state, "Root");
    let mut inner = state.inner.lock().unwrap();
    for index in 0..512 { inner.delegations.push(historical_delegation(index)); }
    let central = inner.federation.local.clone().unwrap();
    let first = federation_native_catalog_locked(&inner, None, central.clone(), central.clone(), String::new()).unwrap();
    let mut after = first.next;
    let mut seen = HashSet::new();
    for step in 0..20 {
        let Some(cursor) = after.take() else { break; };
        if step < 6 {
            let index = inner.delegations.len();
            inner.delegations.push(historical_delegation(1000 + step));
            inner.mark_delegation_mutated(index).unwrap();
            inner.remove_delegation_at(0);
            // Alternate an additional insertion so both length and position churn.
            if step % 2 == 0 {
                let index = inner.delegations.len();
                inner.delegations.push(historical_delegation(2000 + step));
                inner.mark_delegation_mutated(index).unwrap();
            }
        }
        let page = federation_native_catalog_locked(&inner, Some(&cursor), central.clone(), central.clone(), String::new()).unwrap();
        assert!(page.next.as_ref() != Some(&cursor), "structural churn must not repeat a cold-build cursor");
        seen.extend(page.roots.into_iter().map(|row| row.session_id));
        after = page.next;
    }
    assert!(after.is_none(), "finite churn must not continually discard cold-build progress");
    assert!(seen.contains(&root));
}

#[test]
fn current_central_discovery_joins_do_not_rescan_delegation_history() {
    let state = federation_mailboxes::central_store();
    let roots: Vec<_> = (0..32).map(|index| native(&state, &format!("Root {index}"))).collect();
    for root in &roots { state.join_federation_mailbox_session_with_history(root, true).unwrap(); }
    {
        let mut inner = state.inner.lock().unwrap();
        for index in 0..512 { inner.delegations.push(historical_delegation(index)); }
        let central = inner.federation.local.clone().unwrap();
        let mut after = None;
        loop {
            let page = federation_native_catalog_locked(&inner, after.as_deref(), central.clone(), central.clone(), String::new()).unwrap();
            after = page.next;
            if after.is_none() { break; }
        }
    }
    TEST_DELEGATION_CHILD_SCAN_COUNT.with(|count| count.set(0));
    for root in roots { state.join_federation_mailbox_session_with_history(&root, true).unwrap(); }
    let scans = TEST_DELEGATION_CHILD_SCAN_COUNT.with(|count| count.get());
    assert_eq!(scans, 0, "a current indexed page cannot scan delegation history again per root");
}

#[tokio::test]
async fn interrupted_automatic_install_reuses_its_reservation_after_requalification() {
    let mut pair = qualified_pair().await;
    let c = native(&pair.central, "C");
    let s = native(&pair.satellite, "S");
    let target = pair.central.join_federation_mailbox_session_with_history(&c, true).unwrap().session_id;
    let handle = pair.central.federation_peer_handles.lock().unwrap()[&pair.id].clone();
    let mut channel = handle.channel.clone();
    let cut_channel = handle.channel.clone();
    let cut = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = cut.clone();
    channel.control.observed = Arc::new(move |phase, _| {
        if phase == "mailboxParticipationReserved" && !observed.swap(true, Ordering::SeqCst) {
            cut_channel.invalidate();
        }
    });
    let controlled = FederationPeerHandle { channel, rpc: handle.rpc.clone() };
    let lease = controlled.channel.current_lease().unwrap();
    let described: FederationParticipationCatalog = controlled.rpc.request(FederationRpcRoute::Participation(
        lease.clone(), FederationParticipationCommand::Describe { native_session_id: s.clone() })).await.unwrap();
    // Drive the actual admission/INSTALL path with the observed lease. The
    // incoming Ensure route has its own lease, so replacing an outgoing
    // handle alone cannot inject this precise commit-adjacent transport cut.
    let first = pair.central.admit_automatic_mailbox_root(&controlled, &lease, described.roots.into_iter().next().unwrap()).await;
    assert!(first.is_err());
    assert!(cut.load(Ordering::SeqCst), "the transport cut must follow the actual reserve commit");
    assert!(pair.satellite.federation_mailbox_actor(&s).unwrap().is_none());
    let reserved = {
        let inner = pair.central.inner.lock().unwrap();
        assert_eq!(inner.federation.placements.len(), 1);
        let placement = inner.federation.placements.values().next().unwrap();
        assert_eq!(placement.state, FederationPlacementState::Reserved);
        inner.federation.binding_install_intents[&placement.admission_key].binding.clone()
    };
    pair.reconnect().await;
    pair.rpc.health(&pair.lease).await.unwrap();
    let retry = public_send(&pair.satellite, &s, &target, "interrupted-auto-install", "retained admission").await;
    let installed = pair.satellite.inner.lock().unwrap().sessions.iter().find(|row| row.session.id == s)
        .unwrap().federation_automatic_mailbox_binding.clone();
    let reservations = pair.central.inner.lock().unwrap().federation.placements.len();
    pair.finish().await;
    assert!(retry.is_ok(), "ordinary retry must finish the retained admission: {retry:?}");
    assert_eq!(installed, Some(reserved), "requalification cannot allocate a replacement principal");
    assert_eq!(reservations, 1);
}

#[tokio::test]
async fn revoked_peer_no_longer_blocks_healthy_global_aliases() {
    let pair = qualified_pair().await;
    let sender = native(&pair.central, "Sender");
    let target = native(&pair.central, "Reviewer");
    native(&pair.satellite, "Reviewer");
    directory(&pair.central, &sender).await;
    assert!(public_send(&pair.central, &sender, "Reviewer", "ambiguous-before-revoke", "must not append").await.is_err());
    let current = pair.central.federation_peer_snapshot(&pair.id).unwrap();
    pair.central.transition_federation_peer(&current, FederationMembershipState::Revoked).unwrap();
    let snapshot = pair.central.mailbox_directory_snapshot(&sender).await.unwrap();
    let sent = public_send(&pair.central, &sender, "Reviewer", "healthy-after-revoke", "only eligible reviewer").await;
    let still_unavailable = snapshot.unavailable_hosts.iter().any(|host| host.host_id == pair.id);
    let expected = pair.central.federation_mailbox_actor(&target).unwrap().unwrap().binding.participant_key();
    let resolved: Option<String> = sent.as_ref().ok().map(|receipt| pair.central.mailbox_store.connection().unwrap()
        .query_row("SELECT target_session_id FROM mailbox_messages WHERE id=?1", [&receipt.message_id], |row| row.get(0)).unwrap());
    pair.finish().await;
    assert!(!still_unavailable, "terminal revocation removes a host from alias eligibility");
    assert!(sent.is_ok(), "a revoked namesake cannot permanently deny a healthy alias: {sent:?}");
    assert_eq!(resolved, Some(expected));
}

#[tokio::test]
async fn routing_only_duplicate_receipt_retains_a_non_executable_notification_decision() {
    let pair = qualified_pair().await;
    let c = native(&pair.central, "C");
    let s = native(&pair.satellite, "S");
    let first = send(&pair.central, &c, "S", "routing-only-retry-cause").await;
    let target = pair.satellite.federation_mailbox_actor(&s).unwrap().unwrap().binding.participant_key();
    // Public retries may replay their local intent receipt before reaching C.
    // An incoming duplicate Send exercises C's append/dedup boundary itself.
    let request = federation_mailboxes::request(&pair.central, &c, FederationMailboxOperation::Send {
        target, message: "routing-only-retry-cause".into(), key: "routing-only-retry-cause".into(),
        topic: None, state_stamp: None,
    });
    let retry: MailboxAppendReceipt = serde_json::from_value(pair.central.execute_federation_mailbox(request, None).unwrap().value).unwrap();
    pair.finish().await;
    assert!(retry.duplicate);
    assert_eq!(first.message_id, retry.message_id);
    assert_ne!(retry.notification_cause.as_deref(), Some(FEDERATION_REMOTE_WAKE_CAUSE),
        "a duplicate cannot invent an executor wake after a routing-only append");
}
