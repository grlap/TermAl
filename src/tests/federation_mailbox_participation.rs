// Exercises ordinary native mailbox participation without operator joins.
// Uses disposable stores and authenticated loopback hosts, never live hosts.
use super::*;

#[path = "federation_mailbox_participation_regressions.rs"]
mod regressions;

#[path = "federation_mailbox_participation_wakes.rs"]
mod wakes;

#[path = "federation_mailbox_participation_local.rs"]
mod local;

#[path = "federation_mailbox_participation_review_r8.rs"]
mod review_r8;

#[path = "federation_mailbox_native_addressing.rs"]
mod native_addressing;

#[path = "federation_mailbox_routing_reconnect.rs"]
mod routing_reconnect;

pub(super) async fn directory(state: &AppState, native: &str) -> Vec<FederationMailboxParticipant> {
    list_federation_mailbox_participants(AxumPath(native.to_owned()), State(state.clone()))
        .await
        .expect("qualified native roots participate without an explicit join")
        .0
}

#[tokio::test]
async fn central_native_directory_discovers_existing_roots_without_join() {
    let state = federation_mailboxes::central_store();
    let a = test_session_id(&state, Agent::Codex);
    let b = test_session_id(&state, Agent::Codex);
    assert!(state.federation_mailbox_actor(&a).unwrap().is_none());
    let rows = directory(&state, &a).await;
    for native in [&a, &b] {
        assert!(rows.iter().any(|row| row.binding.identity.session_id == *native));
    }
    assert!(state.inner.lock().unwrap().federation.placements.is_empty(),
        "central mailbox discovery must not reserve remote execution capacity");
}

#[tokio::test]
async fn automatic_participation_preserves_standalone_mail_and_issued_receipt() {
    let state = federation_mailboxes::central_store();
    let a = test_session_id(&state, Agent::Codex);
    let b = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().unwrap();
        for native in [&a, &b] {
            let index = inner.find_session_index(native).unwrap();
            inner.session_mut_by_index(index).unwrap().session.status = SessionStatus::Active;
        }
    }
    let sent = state.append_mailbox_message_and_notify(&a, SendMailboxMessageRequest {
        target_session_id: b.clone(), message: "existing local mail".to_owned(),
        idempotency_key: "local-before-participation".to_owned(), topic: None,
        state_stamp: None, class: Some("routine".to_owned()),
    }).unwrap();
    let page = read_mailbox(AxumPath((b.clone(), sent.mailbox_id.clone())),
        State(state.clone()), Json(ReadMailboxRequest {
            read_key: None, issue_receipt: true, after_sequence: None, limit: 20,
        })).await.unwrap().0;
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].id, sent.message_id);
    let receipt = page.receipt.expect("local page must issue a receipt");

    let rows = directory(&state, &b).await;
    assert!(rows.iter().any(|row| row.binding.identity.session_id == b));
    let listed = list_mailboxes_with_availability(&b, &state).await.unwrap();
    assert!(listed.mailboxes.iter().any(|row| row.id == sent.mailbox_id));
    let message = read_mailbox_message(AxumPath((b.clone(), sent.message_id.clone())),
        State(state.clone())).await.unwrap().0;
    assert_eq!(message.body, "existing local mail");
    assert_eq!(message.id, sent.message_id);
    let acknowledged = acknowledge_mailbox(AxumPath((b.clone(), sent.mailbox_id.clone())),
        State(state.clone()), Json(AcknowledgeMailboxRequest {
            receipt: Some(receipt), expected_processed_through: None,
            processed_through: None, no_continuation: false,
        })).await.unwrap().0;
    assert!(acknowledged.participants.iter().any(|row|
        row.session_id == b && row.processed_through == sent.sequence));
}

#[tokio::test]
async fn verified_peer_roots_are_discovered_without_activation_or_placement() {
    let pair = federation_onboarding_fixture::Pair::with_enrolled_profile(
        federation_mailboxes::central_store(), federation_onboarding_fixture::io(),
        federation_onboarding_fixture::io(), federation_onboarding_fixture::profile(),
    ).await;
    let central_native = test_session_id(&pair.central, Agent::Codex);
    let satellite_native = test_session_id(&pair.satellite, Agent::Codex);
    assert!(pair.verify().await.unwrap().termal_complete());
    {
        let inner = pair.central.inner.lock().unwrap();
        assert_eq!(inner.federation.peers[&pair.id].membership, FederationMembershipState::Verified);
        assert!(inner.federation.placements.is_empty());
    }
    let rows = directory(&pair.central, &central_native).await;
    assert!(rows.iter().any(|row| row.binding.identity.host_id == pair.id
        && row.binding.identity.session_id == satellite_native));
    let remote_rows = directory(&pair.satellite, &satellite_native).await;
    assert!(remote_rows.iter().any(|row| row.binding.identity.session_id == central_native
        && row.binding.identity.host_id != pair.id));
    assert_eq!(pair.central.inner.lock().unwrap().federation.peers[&pair.id].membership,
        FederationMembershipState::Verified,
        "mailbox participation cannot promote execution authority");
    pair.finish().await;
}

#[tokio::test]
async fn enrolled_authenticated_satellite_joins_mail_without_execution_authority() {
    let pair = federation_onboarding_fixture::Pair::with_enrolled_profile(
        federation_mailboxes::central_store(), federation_onboarding_fixture::io(),
        federation_onboarding_fixture::io(), federation_onboarding_fixture::profile(),
    ).await;
    let satellite_native = test_session_id(&pair.satellite, Agent::Codex);
    let result = list_federation_mailbox_participants(AxumPath(satellite_native.clone()),
        State(pair.satellite.clone())).await;
    assert!(result.is_ok(), "the enrolled authenticated pair suffices for ordinary mail");
    assert!(pair.satellite.federation_mailbox_actor(&satellite_native).unwrap().is_some());
    let inner = pair.central.inner.lock().unwrap();
    assert_eq!(inner.federation.peers[&pair.id].membership, FederationMembershipState::Enrolled);
    assert!(inner.federation.peers[&pair.id].verification.is_none());
    assert!(inner.federation.placements.values().all(|placement| placement.mailbox_only
        && !federation_placement_authority(&inner.federation, &inner.preferences.federation, placement)));
    drop(inner);
    pair.finish().await;
}

pub(super) async fn qualified_pair() -> federation_onboarding_fixture::Pair {
    let pair = federation_onboarding_fixture::Pair::with_enrolled_profile(
        federation_mailboxes::central_store(), federation_onboarding_fixture::io(),
        federation_onboarding_fixture::io(), federation_onboarding_fixture::profile(),
    ).await;
    assert!(pair.verify().await.unwrap().termal_complete());
    pair
}

pub(super) fn native(state: &AppState, name: &str) -> String {
    let native = test_session_id(state, Agent::Codex);
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&native).unwrap();
    let record = inner.session_mut_by_index(index).unwrap();
    record.session.name = name.to_owned();
    record.session.status = SessionStatus::Active;
    native
}

async fn send(state: &AppState, from: &str, to: &str, key: &str) -> MailboxAppendReceipt {
    send_mailbox_message(AxumPath(from.to_owned()), State(state.clone()),
        Json(serde_json::from_value(json!({"targetSessionId":to,"message":key,
            "idempotencyKey":key})).unwrap())).await.unwrap().1.0
}

async fn read_ack(state: &AppState, native: &str, sent: &MailboxAppendReceipt) {
    let page = read_mailbox(AxumPath((native.to_owned(), sent.mailbox_id.clone())), State(state.clone()),
        Json(ReadMailboxRequest { after_sequence: None, limit: 20, issue_receipt: true, read_key: None })).await.unwrap().0;
    assert!(page.messages.iter().any(|message| message.id == sent.message_id));
    let exact = read_mailbox_message(AxumPath((native.to_owned(), sent.message_id.clone())), State(state.clone())).await.unwrap().0;
    assert_eq!(exact.id, sent.message_id);
    let receipt = page.receipt.unwrap();
    let summary = acknowledge_mailbox(AxumPath((native.to_owned(), sent.mailbox_id.clone())), State(state.clone()),
        Json(AcknowledgeMailboxRequest { receipt: Some(receipt), expected_processed_through: None,
            processed_through: None, no_continuation: false })).await.unwrap().0;
    let actor = state.federation_mailbox_actor(native).unwrap().unwrap().binding.participant_key();
    assert!(summary.participants.iter().any(|row| row.session_id == actor && row.processed_through == sent.sequence));
}

#[tokio::test]
async fn qualified_existing_roots_exchange_read_and_ack_without_join_or_activation() {
    let pair = qualified_pair().await;
    let c = native(&pair.central, "Central inbox");
    let s = native(&pair.satellite, "Satellite inbox");
    let old_peer = native(&pair.satellite, "Existing local peer");
    let historical = pair.satellite.append_mailbox_message_and_notify(&old_peer,
        SendMailboxMessageRequest { target_session_id: s.clone(), message: "original local history".into(),
            idempotency_key: "original-local-history".into(), topic: None, state_stamp: None, class: None }).unwrap();
    let old_page = read_mailbox(AxumPath((s.clone(), historical.mailbox_id.clone())), State(pair.satellite.clone()),
        Json(ReadMailboxRequest { after_sequence: None, limit: 20, issue_receipt: true, read_key: None })).await.unwrap().0;
    // A first send by name discovers the peer's previously unused native root.
    let outbound = send(&pair.central, &c, "Satellite inbox", "automatic-outbound").await;
    read_ack(&pair.satellite, &s, &outbound).await;
    let inbound = send(&pair.satellite, &s, "Central inbox", "automatic-inbound").await;
    read_ack(&pair.central, &c, &inbound).await;
    assert_eq!(outbound.mailbox_id, inbound.mailbox_id);
    let again = send(&pair.central, &c, "Satellite inbox", "automatic-outbound").await;
    assert_eq!(again.message_id, outbound.message_id);
    let listed = list_mailboxes_with_availability(&s, &pair.satellite).await.unwrap();
    assert!(listed.mailboxes.iter().any(|row| row.id == historical.mailbox_id));
    let old = read_mailbox_message(AxumPath((s.clone(), historical.message_id.clone())), State(pair.satellite.clone())).await.unwrap().0;
    assert_eq!(old.body, "original local history");
    let acknowledged = acknowledge_mailbox(AxumPath((s.clone(), historical.mailbox_id.clone())), State(pair.satellite.clone()),
        Json(AcknowledgeMailboxRequest { receipt: old_page.receipt, expected_processed_through: None,
            processed_through: None, no_continuation: false })).await.unwrap().0;
    assert!(acknowledged.participants.iter().any(|row| row.session_id == s && row.processed_through == historical.sequence));
    let view = pair.central.federation_registry_view().unwrap();
    assert_eq!(view["peers"][0]["allocatedSessions"], 0);
    assert_eq!(view["peers"][0]["allocatedBuildSlots"], 0);
    assert_eq!(view["peers"][0]["allocatedGateSlots"], 0);
    let inner = pair.central.inner.lock().unwrap();
    assert_eq!(inner.federation.peers[&pair.id].membership, FederationMembershipState::Verified);
    for placement in inner.federation.placements.values() {
        assert!(placement.mailbox_only);
        assert!(!federation_placement_authority(&inner.federation, &inner.preferences.federation, placement));
    }
    drop(inner);
    pair.finish().await;
}

#[tokio::test]
async fn central_bare_native_target_stays_local_after_automatic_participation() {
    let state = federation_mailboxes::central_store();
    let a = native(&state, "A");
    let b = native(&state, "B");
    directory(&state, &a).await;
    {
        let inner = state.inner.lock().unwrap();
        for native in [&a, &b] {
            let root = &inner.sessions[inner.find_session_index(native).unwrap()];
            assert!(root.federation_automatic_mailbox_binding.is_some());
        }
    }
    let sent = send(&state, &a, &b, "automatic-local").await;
    local::acknowledge_native(&state, &b, &sent).await;
    assert_eq!(state.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM federation_mailbox_intents", [], |row| row.get::<_, u64>(0)).unwrap(), 0);
}

#[tokio::test]
async fn explicit_active_placement_upgrades_only_mailbox_authority_without_readdressing() {
    let pair = qualified_pair().await;
    let c = native(&pair.central, "C");
    let s = native(&pair.satellite, "S");
    directory(&pair.central, &c).await;
    let before = pair.satellite.federation_mailbox_actor(&s).unwrap().unwrap().binding;
    let request = || Json(FederationPlacementRequest { native_session_id: s.clone(), admission_key: "explicit-execution".into() });
    assert!(reserve_federation_mailbox_placement_route(AxumPath(pair.id.clone()), State(pair.central.clone()), request()).await.is_err());
    let _ = activate_federation_satellite_route(AxumPath(pair.id.clone()), State(pair.central.clone())).await.unwrap();
    let (status, result) = reserve_federation_mailbox_placement_route(AxumPath(pair.id.clone()), State(pair.central.clone()), request()).await.unwrap();
    assert_eq!(status, StatusCode::OK, "{}", result.0);
    let placement: FederationPlacement = serde_json::from_value(result.0["placement"].clone()).unwrap();
    assert!(!placement.mailbox_only);
    assert_eq!(placement.identity, before.identity);
    assert_eq!(placement.placement_generation, before.placement_generation);
    assert_eq!(pair.satellite.federation_mailbox_actor(&s).unwrap().unwrap().binding, before);
    pair.central.ensure_federation_placement_current(&placement).unwrap();
    assert_eq!(pair.central.federation_registry_view().unwrap()["peers"][0]["allocatedSessions"], 1);
    pair.finish().await;
}

#[tokio::test]
async fn recreated_native_cannot_reuse_an_automatic_binding_or_reservation() {
    let pair = qualified_pair().await;
    let c = native(&pair.central, "C");
    let s = native(&pair.satellite, "S");
    directory(&pair.central, &c).await;
    let old = pair.satellite.federation_mailbox_actor(&s).unwrap().unwrap().binding;
    {
        let mut inner = pair.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&s).unwrap();
        let root = inner.session_mut_by_index(index).unwrap();
        root.federation_native_creation_id = Uuid::new_v4().to_string();
        root.federation_mailbox_binding = None;
        root.federation_automatic_mailbox_binding = None;
    }
    pair.satellite.ensure_automatic_mailbox_participation(&s, true).await.unwrap();
    let replacement = pair.satellite.federation_mailbox_actor(&s).unwrap().unwrap().binding;
    assert_ne!(replacement.identity, old.identity);
    assert!(replacement.placement_generation > old.placement_generation);
    assert!(pair.central.inner.lock().unwrap().federation.placements.values().any(|placement|
        placement.identity == old.identity && placement.state == FederationPlacementState::Completed));
    pair.finish().await;
}

#[tokio::test]
async fn revoked_and_retired_links_refuse_new_automatic_participants() {
    let pair = qualified_pair().await;
    let s = native(&pair.satellite, "S");
    pair.central.mutate_federation(|domain, _| {
        domain.peers.get_mut(&pair.id).unwrap().membership = FederationMembershipState::Revoked;
        Ok(())
    }).unwrap();
    assert!(pair.satellite.ensure_automatic_mailbox_participation(&s, true).await.is_err());
    assert!(pair.satellite.federation_mailbox_actor(&s).unwrap().is_none());
    assert!(pair.central.inner.lock().unwrap().federation.placements.is_empty());
    pair.finish().await;
    let pair = qualified_pair().await;
    let s = native(&pair.satellite, "S");
    pair.satellite_channel.invalidate();
    assert!(pair.satellite.ensure_automatic_mailbox_participation(&s, true).await.is_err());
    assert!(pair.satellite.federation_mailbox_actor(&s).unwrap().is_none());
    pair.finish().await;
}

#[test]
fn bridge_uses_domain_discovery_before_a_native_root_has_a_binding() {
    let mut state = json!({"preferences":{"federation":{"enabled":true,"role":"satellite"}},
        "sessions":[{"id":"session-1"}], "delegations":[]});
    assert!(automatic_domain_mailbox_discovery(&state, "session-1"));
    state["sessions"][0]["remoteId"] = json!("ssh-peer");
    assert!(!automatic_domain_mailbox_discovery(&state, "session-1"));
    state["sessions"][0]["remoteId"] = Value::Null;
    state["sessions"][0]["parentDelegationId"] = json!("delegation-1");
    assert!(!automatic_domain_mailbox_discovery(&state, "session-1"));
    state["sessions"][0]["parentDelegationId"] = Value::Null;
    state["preferences"]["federation"]["enabled"] = json!(false);
    assert!(!automatic_domain_mailbox_discovery(&state, "session-1"));
}

#[tokio::test]
async fn authenticated_session_assertion_cannot_supply_a_forged_native_creation() {
    let pair = qualified_pair().await;
    let s = native(&pair.satellite, "S");
    let central = pair.satellite.inner.lock().unwrap().preferences.federation.central.clone().unwrap();
    let handle = pair.satellite.federation_peer_handle(&central.host_id).unwrap();
    let lease = handle.channel.current_lease().unwrap();
    let result = handle.rpc.request::<FederationMailboxParticipant>(FederationRpcRoute::Participation(
        lease, FederationParticipationCommand::Ensure { native_session_id: s.clone(),
            native_creation_id: Uuid::new_v4().to_string(), directory: false },
    )).await;
    assert_eq!(result.unwrap_err().status, StatusCode::CONFLICT);
    assert!(pair.central.inner.lock().unwrap().federation.placements.is_empty());
    assert!(pair.satellite.federation_mailbox_actor(&s).unwrap().is_none());
    pair.finish().await;
}
