// Public counterexamples for retained owner keys and offline local discovery.
// Existing authenticated hosts and bridge fixtures; no live store or provider.
use super::*;
use super::regressions::public_send;

async fn superseded_sender_retry(unknown: bool) {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let pair = qualified_pair().await;
    let central = native(&pair.central, "Central recipient");
    let sender = native(&pair.satellite, "Satellite sender");
    directory(&pair.central, &central).await;
    let target = pair.central.federation_mailbox_actor(&central).unwrap().unwrap().binding.participant_key();
    let original_sender = pair.satellite.federation_mailbox_actor(&sender).unwrap().unwrap().binding;
    if unknown {
        pair.satellite.mailbox_store.connection().unwrap().execute_batch(
            "CREATE TRIGGER lose_sender_settlement BEFORE UPDATE OF state ON federation_mailbox_intents
             WHEN NEW.state='homeCommitted' BEGIN SELECT RAISE(ABORT,'injected local settlement loss'); END;"
        ).unwrap();
    }
    let first = public_send(&pair.satellite, &sender, &target, "retained-owner-key", "original").await;
    if unknown {
        assert!(first.as_ref().unwrap_err().message.contains("central append committed"));
        pair.satellite.mailbox_store.connection().unwrap().execute_batch("DROP TRIGGER lose_sender_settlement;").unwrap();
    } else { assert!(first.is_ok(), "{first:?}"); }
    let original = pair.satellite.local_federation_intent(&sender, "retained-owner-key").unwrap();
    assert_eq!(original.state, if unknown { "appendOutcomeUnknown" } else { "homeCommitted" });
    let original_message: String = pair.central.mailbox_store.connection().unwrap().query_row(
        "SELECT id FROM mailbox_messages", [], |row| row.get(0)).unwrap();
    let (central_state, satellite_state, listeners) = pair.suspend().await;
    let settings = central_state.inner.lock().unwrap().preferences.federation.clone();
    let mut disabled = settings.clone();
    disabled.enabled = false;
    central_state.configure_federation(disabled).unwrap();
    central_state.configure_federation(settings).unwrap();
    let pair = federation_onboarding_fixture::Pair::resume(central_state, satellite_state, listeners,
        federation_onboarding_fixture::io(), federation_onboarding_fixture::io()).await;
    assert!(pair.verify().await.unwrap().termal_complete());
    let retried = public_send(&pair.satellite, &sender, &target, "retained-owner-key", "original").await;
    let current = pair.satellite.federation_mailbox_actor(&sender).unwrap().unwrap().binding;
    assert_ne!(current, original_sender, "the ordinary retry reaches automatic supersession");
    let messages: u64 = pair.central.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get(0)).unwrap();
    let intents: u64 = pair.satellite.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM federation_mailbox_intents WHERE owner_session_id=?1 AND idempotency_key='retained-owner-key'",
        [&sender], |row| row.get(0)).unwrap();
    if intents == 1 {
        let retained = pair.satellite.local_federation_intent(&sender, "retained-owner-key").unwrap();
        assert_eq!(retained.state, original.state);
        assert_eq!(retained.digest, original.digest);
    }
    pair.finish().await;
    assert_eq!(messages, 1, "a retry under a replacement route cannot append a second message: {retried:?}");
    assert_eq!(intents, 1, "one native owner/key keeps one retained intent");
    match retried {
        Ok(receipt) => {
            assert!(receipt.duplicate);
            assert_eq!(receipt.message_id, original_message);
        }
        Err(error) => assert_eq!(error.status, StatusCode::CONFLICT),
    }
}

#[tokio::test]
async fn committed_sender_key_survives_automatic_route_supersession() { superseded_sender_retry(false).await; }

#[tokio::test]
async fn unknown_sender_key_survives_automatic_route_supersession() { superseded_sender_retry(true).await; }

#[tokio::test]
async fn domain_create_refuses_bare_native_ids_before_participation() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = federation_mailboxes::central_store();
    let sender = native(&state, "Sender");
    let target = native(&state, "Target");
    let result = get_or_create_federation_mailbox(AxumPath(sender.clone()), State(state.clone()),
        Json(CreateFederationMailboxRequest { target_session_id: target.clone() })).await;
    assert!(result.is_err(), "domain-only create must not reinterpret a local id: {result:?}");
    assert!(state.federation_mailbox_actor(&sender).unwrap().is_none());
    assert!(state.federation_mailbox_actor(&target).unwrap().is_none());
}

#[tokio::test]
async fn domain_intent_refuses_bare_native_ids_before_capturing_a_delivery() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = federation_mailboxes::central_store();
    let sender = native(&state, "Sender");
    let target = native(&state, "Target");
    directory(&state, &sender).await;
    let result = prepare_federation_intent_route(AxumPath(sender), State(state.clone()),
        Json(PrepareFederationIntentRequest { target_session_id: target, message: "must refuse".into(),
            idempotency_key: "bare-intent".into(), topic: None, state_stamp: None, class: None, target_binding: None })).await;
    assert!(result.is_err(), "domain-only intent preparation must not reinterpret a local id: {result:?}");
    assert_eq!(state.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM federation_mailbox_intents", [], |row| row.get::<_, u64>(0)).unwrap(), 0);
}

#[tokio::test]
async fn empty_local_mailbox_listing_reports_unavailable_central() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let pair = qualified_pair().await;
    let sender = native(&pair.satellite, "Local sender");
    pair.satellite.federation_peer_handles.lock().unwrap().clear();
    let listing = list_mailboxes_with_availability(&sender, &pair.satellite).await;
    pair.finish().await;
    let listing = listing.expect("empty local history remains listable without the central");
    assert!(listing.mailboxes.is_empty());
    assert!(listing.unavailable_central_domain.is_some());
}

fn offline_bridge_listing(status: u16) {
    let central = Uuid::new_v4().to_string();
    let state = json!({"preferences":{"federation":{"enabled":true,"role":"satellite",
        "central":{"hostId":central,"hostGeneration":1}}},"sessions":[
        {"id":"session-parent","name":"Caller"}, {"id":"session-local","name":"Reviewer"},
        {"id":"session-child","name":"Child","parentDelegationId":"delegation-x"},
        {"id":"session-proxy","name":"Projection","remoteId":"remote-x"}],"delegations":[]});
    let (base_url, _, server) = crate::delegation_mcp_tests::spawn_test_mcp_http_server(4, move |request| {
        if request.path == "/api/state" { (200, state.clone()) }
        else {
            assert_eq!(request.path, "/api/sessions/session-parent/mailboxes/directory");
            (status, json!({"error":"central mailbox directory unavailable"}))
        }
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".into(), base_url).unwrap();
    let listing = bridge.tool_list_sessions(json!({}));
    let alias = bridge.resolve_session_reference("Reviewer");
    assert_eq!(bridge.resolve_session_reference("session-local").unwrap(), "session-local");
    server.join().unwrap();
    let listing = listing.expect("offline bridge still lists its own native roots");
    let ids = listing["sessions"].as_array().unwrap().iter()
        .filter_map(|row| row["sessionId"].as_str()).collect::<Vec<_>>();
    assert!(ids.contains(&"session-local"));
    assert!(!ids.contains(&"session-child") && !ids.contains(&"session-proxy"));
    assert!(listing["unavailableHosts"].as_array().unwrap().iter().any(|row| row["hostId"] == central));
    assert!(alias.is_err(), "incomplete global alias inventory must stay fail-closed");
}

#[test]
fn offline_bridge_lists_local_native_roots_without_guessing_global_aliases() { offline_bridge_listing(503); }

#[test]
fn unverified_bridge_lists_local_native_roots_without_guessing_global_aliases() { offline_bridge_listing(409); }
