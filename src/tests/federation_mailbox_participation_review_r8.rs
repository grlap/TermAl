// Public counterexamples for retained owner keys and offline local discovery.
// Existing authenticated hosts and bridge fixtures; no live store or provider.
use super::*;
use super::regressions::public_send;
use crate::tests::federation_native_binding_proof::DomainServer;

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
    let peer_id = pair.id.clone();
    let (central_state, satellite_state, listeners) = pair.suspend().await;
    let peer = central_state.inner.lock().unwrap().federation.peers[&peer_id].clone();
    central_state.enroll_federation_peer(peer).unwrap();
    let peer = satellite_state.inner.lock().unwrap().federation.peers.values().next().unwrap().clone();
    satellite_state.enroll_federation_peer(peer).unwrap();
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
            assert!(matches!(request.path.as_str(),
                "/api/sessions/session-parent/mailboxes/directory"
                | "/api/sessions/session-parent/mailboxes/participants?aliasBasis=true"));
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
    assert!(alias.is_err(), "incomplete global alias inventory must stay fail-closed");
    let local = listing["sessions"].as_array().unwrap().iter()
        .find(|row| row["sessionId"] == "session-local").unwrap();
    let name = local["name"].as_str().unwrap().to_owned();
    assert_eq!(name, "session-local", "an offline listed name must remain a usable native locator");
    assert_eq!(local["localName"], "Reviewer");
    let (base_url, _, server) = crate::delegation_mcp_tests::spawn_test_mcp_http_server(1, |request| {
        assert_eq!(request.path, "/api/sessions/session-parent/mailboxes/send");
        let body: Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(body["targetSessionId"], "session-local");
        assert_eq!(body["message"], "copied offline native name");
        (202, json!({"mailboxId":"mailbox-local", "messageId":"message-local", "sequence":1,
            "unreadDepth":1, "notificationDisposition":"queuedBehindActiveTurn", "duplicate":false,
            "senderProcessedThrough":1, "senderCursorAdvanced":true}))
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".into(), base_url).unwrap();
    let sent = bridge.tool_send_to_session(json!({"sessionId":name,
        "message":"copied offline native name", "idempotencyKey":"offline-listed-native"}));
    server.join().unwrap();
    assert_eq!(sent.unwrap()["sessionId"], "session-local");
}

#[test]
fn offline_bridge_lists_local_native_roots_without_guessing_global_aliases() { offline_bridge_listing(503); }

#[test]
fn unverified_bridge_lists_local_native_roots_without_guessing_global_aliases() { offline_bridge_listing(409); }

async fn exact_domain_selection_without_admission(intent: bool, missing: bool) {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = federation_mailboxes::central_store();
    let (sender, _) = federation_mailboxes::local_participant(&state, "Bound caller");
    let target = native(&state, "Unbound target");
    let host = state.inner.lock().unwrap().federation.local.as_ref().unwrap().host_id.clone();
    let address = format!("host:{host}::{}", if missing { "session-999999" } else { &target });
    assert!(state.federation_mailbox_actor(&target).unwrap().is_none());
    let before = state.inner.lock().unwrap().federation.mailbox_directory.len();
    let (route, body) = if intent {
        ("mailbox-intents", json!({"targetSessionId":address,
            "message":"exact selection must not admit", "idempotencyKey":"existing-only"}))
    } else {
        ("mailboxes/create", json!({"targetSessionId":address}))
    };
    let response = app_router(state.clone()).oneshot(Request::builder().method("POST")
        .uri(format!("/api/sessions/{sender}/{route}"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap())).unwrap()).await.unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(state.inner.lock().unwrap().federation.mailbox_directory.len(), before,
        "exact selection cannot admit the target or unrelated roots: status={status}, body={body:?}");
    assert!(state.federation_mailbox_actor(&target).unwrap().is_none());
    assert_eq!(status, StatusCode::NOT_FOUND, "{body:?}");
    let connection = state.mailbox_store.connection().unwrap();
    for table in ["mailboxes", "mailbox_messages", "federation_mailbox_intents"] {
        assert_eq!(connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [],
            |row| row.get::<_, u64>(0)).unwrap(), 0, "{table}");
    }
}

#[tokio::test]
async fn native_address_review_exact_create_does_not_admit_unbound_target() {
    exact_domain_selection_without_admission(false, false).await;
}

#[tokio::test]
async fn native_address_review_exact_intent_does_not_admit_unbound_target() {
    exact_domain_selection_without_admission(true, false).await;
}

#[tokio::test]
async fn native_address_review_missing_create_does_not_admit_unrelated_roots() {
    exact_domain_selection_without_admission(false, true).await;
}

#[tokio::test]
async fn native_address_review_missing_intent_does_not_admit_unrelated_roots() {
    exact_domain_selection_without_admission(true, true).await;
}

#[tokio::test]
async fn native_address_review_existing_exact_domain_target_stays_usable() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = federation_mailboxes::central_store();
    let (sender, _) = federation_mailboxes::local_participant(&state, "Caller");
    let (target, participant) = federation_mailboxes::local_participant(&state, "Existing recipient");
    let unrelated = native(&state, "Unrelated unbound root");
    let address = format!("host:{}::{target}", participant.binding.identity.host_id);
    let created = get_or_create_federation_mailbox(AxumPath(sender.clone()), State(state.clone()),
        Json(CreateFederationMailboxRequest { target_session_id: address.clone() })).await.unwrap().0;
    assert!(created.participants.iter().any(|row| row.session_id == participant.session_id));
    let (_, status) = prepare_federation_intent_route(AxumPath(sender), State(state.clone()),
        Json(PrepareFederationIntentRequest { target_session_id: address, message: "existing target".into(),
            idempotency_key: "existing-exact-positive".into(), topic: None, state_stamp: None,
            class: None, target_binding: None })).await.unwrap();
    assert_eq!(status.0["targetSessionId"], participant.session_id);
    assert!(state.federation_mailbox_actor(&unrelated).unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_address_review_satellite_listed_local_name_keeps_native_wake() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let pair = qualified_pair().await;
    let sender = native(&pair.satellite, "Local caller");
    let target = test_session_id(&pair.satellite, Agent::Claude);
    let runtime_id = "listed-local-native-wake";
    let (runtime, input) = test_claude_runtime_handle(runtime_id);
    {
        let mut inner = pair.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&target).unwrap();
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }
    // The bridge's first state read predates automatic admission of the caller.
    assert!(pair.satellite.federation_mailbox_actor(&sender).unwrap().is_none());
    let mut api = DomainServer::start(app_router(pair.satellite.clone())).await;
    let port = api.port;
    let recipient = target.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<(String, Value)> {
        let mut bridge = TermalDelegationMcpBridge::new(sender, format!("http://127.0.0.1:{port}"))?;
        bridge.client = reqwest::blocking::Client::builder().no_proxy()
            .redirect(reqwest::redirect::Policy::none()).timeout(phase_sync::DEADLOCK_GUARD).build()?;
        let listing = bridge.tool_list_sessions(json!({}))?;
        let row = listing["sessions"].as_array().unwrap().iter()
            .find(|row| row["localSessionId"] == recipient).unwrap();
        let name = row["name"].as_str().unwrap().to_owned();
        let sent = bridge.tool_send_to_session(json!({"sessionId":name,
            "message":"native local wake", "idempotencyKey":"listed-local-native"}))?;
        Ok((name, sent))
    }).await.unwrap();
    api.finish().await;
    pair.satellite.finish_turn_ok_if_runtime_matches(&target,
        &RuntimeToken::Claude(runtime_id.into())).unwrap();
    let command = input.try_recv();
    let satellite = pair.satellite.clone();
    pair.finish().await;
    let (name, value) = result.unwrap();
    assert_eq!(name, target, "the copied local name must select the native namespace");
    let sent: MailboxAppendReceipt = serde_json::from_value(value).unwrap();
    assert_eq!(sent.notification_disposition, "queuedBehindActiveTurn");
    let Ok(ClaudeRuntimeCommand::Prompt(command)) = command else {
        panic!("the advertised local recipient must receive its native wake");
    };
    assert!(command.text.contains(&sent.mailbox_id));
    local::acknowledge_native(&satellite, &target, &sent).await;
}

#[test]
fn native_address_review_sibling_listing_preserves_canonical_domain_target() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let state = federation_mailboxes::central_store();
    let (sender, mut caller) = federation_mailboxes::local_participant(&state, "Caller");
    let (_, central) = federation_mailboxes::local_participant(&state, "Central target");
    let (_, mut sibling) = federation_mailboxes::local_participant(&state, "Sibling target");
    caller.binding.identity.host_id = Uuid::new_v4().to_string();
    caller.session_id = caller.binding.participant_key();
    sibling.binding.identity.host_id = Uuid::new_v4().to_string();
    sibling.session_id = sibling.binding.participant_key();
    let sibling_id = sibling.session_id.clone();
    let central_name = format!("host:{}::{}", central.binding.identity.host_id, central.binding.identity.session_id);
    let view = json!({"preferences":{"federation":{"enabled":true,"role":"satellite",
        "central":caller.binding.central}},"sessions":[{"id":sender,"name":"Caller",
        "mailboxParticipantId":caller.session_id}]});
    let rows = serde_json::to_value(vec![caller, central, sibling]).unwrap();
    let posted = Arc::new(std::sync::Mutex::new(None));
    let captured = posted.clone();
    let (base_url, _, server) = crate::delegation_mcp_tests::spawn_test_mcp_http_server(3, move |request| {
        if request.path == "/api/state" { (200, view.clone()) }
        else if request.path.ends_with("/mailboxes/directory") { (200, rows.clone()) }
        else {
            *captured.lock().unwrap() = Some(serde_json::from_str::<Value>(&request.body).unwrap());
            (202, json!({"mailboxId":"mailbox-sibling", "messageId":"message-sibling", "sequence":1,
                "unreadDepth":1,"notificationDisposition":"durableButNotWoken", "duplicate":false,
                "senderProcessedThrough":1,"senderCursorAdvanced":true}))
        }
    });
    let bridge = TermalDelegationMcpBridge::new(sender, base_url).unwrap();
    let listing = bridge.tool_list_sessions(json!({})).unwrap();
    let rows = listing["sessions"].as_array().unwrap();
    let name = rows.iter().find(|row| row["sessionId"] == sibling_id).unwrap()["name"].as_str().unwrap().to_owned();
    assert!(rows.iter().any(|row| row["name"] == central_name), "the configured central keeps native relay addressing");
    let result = bridge.tool_send_to_session(json!({"sessionId":name,
        "message":"domain delivery only", "idempotencyKey":"sibling-domain"}));
    server.join().unwrap();
    assert_eq!(name, sibling_id, "a sibling without a relay pair must keep its canonical domain target");
    assert_eq!(posted.lock().unwrap().as_ref().unwrap()["targetSessionId"], sibling_id);
    assert_eq!(result.unwrap()["notificationDisposition"], "durableButNotWoken");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_address_review_verified_satellite_listed_central_name_wakes_without_activation() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let pair = qualified_pair().await;
    let sender = native(&pair.satellite, "Verified satellite caller");
    let target = test_session_id(&pair.central, Agent::Claude);
    let runtime_id = "verified-central-native-hold";
    let (runtime, input) = test_claude_runtime_handle(runtime_id);
    let host = {
        let mut inner = pair.central.inner.lock().unwrap();
        assert_eq!(inner.federation.peers[&pair.id].membership, FederationMembershipState::Verified);
        let index = inner.find_session_index(&target).unwrap();
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.federation.local.as_ref().unwrap().host_id.clone()
    };
    let mut api = DomainServer::start(app_router(pair.satellite.clone())).await;
    let port = api.port;
    let recipient = target.clone();
    let expected_name = format!("host:{host}::{target}");
    let result = tokio::task::spawn_blocking(move || -> Result<(String, bool, Value)> {
        let mut bridge = TermalDelegationMcpBridge::new(sender, format!("http://127.0.0.1:{port}"))?;
        bridge.client = reqwest::blocking::Client::builder().no_proxy()
            .redirect(reqwest::redirect::Policy::none()).timeout(phase_sync::DEADLOCK_GUARD).build()?;
        let listing = bridge.tool_list_sessions(json!({}))?;
        let row = listing["sessions"].as_array().unwrap().iter()
            .find(|row| row["hostId"] == host && row["localSessionId"] == recipient).unwrap();
        let name = row["name"].as_str().unwrap().to_owned();
        let requires_activation = row["nativeWakeRequiresActivation"] == true;
        let receipt = bridge.tool_send_to_session(json!({"sessionId":name,
            "message":"verified pair native activation hold", "idempotencyKey":"verified-central-listed-native"}))?;
        Ok((name, requires_activation, receipt))
    }).await.unwrap();
    api.finish().await;
    pair.central.run_mailbox_relay_wake_batch(0, 0).await;
    pair.central.finish_turn_ok_if_runtime_matches(&target,
        &RuntimeToken::Claude(runtime_id.into())).unwrap();
    let command = input.try_recv();
    let central = pair.central.clone();
    let membership = central.inner.lock().unwrap().federation.peers[&pair.id].membership;
    let (refusals, _) = central.mailbox_store.relay_home_wake_refusals.snapshot();
    let pending: u64 = central.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM mailbox_relay_wakes WHERE delivered=0", [], |row| row.get(0)).unwrap();
    pair.finish().await;
    let (name, requires_activation, value) = result.expect("durable native delivery remains available");
    assert_eq!(name, expected_name);
    assert_eq!(membership, FederationMembershipState::Verified, "ordinary mail must not activate the peer");
    assert!(matches!(command, Ok(ClaudeRuntimeCommand::Prompt(_))),
        "ordinary native mail from the authenticated satellite wakes the central recipient");
    assert_eq!(pending, 0);
    assert!(refusals.is_empty());
    let receipt: MailboxAppendReceipt = serde_json::from_value(value).unwrap();
    assert_eq!(receipt.notification_disposition, "durableButNotWoken");
    assert!(receipt.notification_cause.is_none(), "no activation remedy is required");
    assert!(!requires_activation, "ordinary native wakes have no activation prerequisite");
    let page = read_mailbox(AxumPath((target.clone(), receipt.mailbox_id.clone())), State(central.clone()),
        Json(ReadMailboxRequest { after_sequence: None, limit: 20, issue_receipt: true, read_key: None })).await.unwrap().0;
    assert!(page.messages.iter().any(|message| message.id == receipt.message_id));
    let principal = central.native_mailbox_relay_descriptor(&target).unwrap().principal.participant_key();
    let summary = acknowledge_mailbox(AxumPath((target, receipt.mailbox_id)), State(central),
        Json(AcknowledgeMailboxRequest { receipt: page.receipt, expected_processed_through: None,
            processed_through: None, no_continuation: false })).await.unwrap().0;
    assert!(summary.participants.iter().any(|participant|
        participant.session_id == principal && participant.processed_through == receipt.sequence));
}
