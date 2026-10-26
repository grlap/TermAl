// Ordinary sends preserve the caller's explicit host/native namespace.
// Disposable authenticated hosts; no live hosts or provider turns are claimed.
use super::*;

fn address(state: &AppState, session: &str) -> String {
    let inner = state.inner.lock().unwrap();
    let host = &inner.federation.local.as_ref().unwrap().host_id;
    let target = format!("host:{host}::{session}");
    assert!(matches!(FederationMailboxAlias::parse(&target, None).unwrap(),
        FederationMailboxAlias::HostNative { .. }));
    target
}

async fn ordinary(state: &AppState, sender: &str, target: &str, key: &str)
    -> Result<MailboxAppendReceipt, ApiError>
{
    send_mailbox_message(AxumPath(sender.to_owned()), State(state.clone()),
        Json(SendMailboxApiRequest { message: SendMailboxMessageRequest {
            target_session_id: target.to_owned(), message: key.to_owned(),
            idempotency_key: key.to_owned(), topic: None, state_stamp: None,
            class: Some("routine".into()),
        }, _target_binding: () })).await.map(|(_, receipt)| receipt.0)
}

async fn receive_and_ack(state: &AppState, recipient: &str, sent: &MailboxAppendReceipt)
    -> Result<(), ApiError>
{
    let page = read_mailbox(AxumPath((recipient.to_owned(), sent.mailbox_id.clone())),
        State(state.clone()), Json(ReadMailboxRequest {
            after_sequence: None, limit: 20, issue_receipt: true, read_key: None,
        })).await?.0;
    let message = page.messages.iter().find(|message| message.id == sent.message_id)
        .expect("the intended native recipient sees the exact committed message");
    assert_eq!(message.sequence, sent.sequence);
    let target = message.target_session_id.clone();
    let summary = acknowledge_mailbox(AxumPath((recipient.to_owned(), sent.mailbox_id.clone())),
        State(state.clone()), Json(AcknowledgeMailboxRequest {
            receipt: Some(page.receipt.expect("the recipient receives its own ACK receipt")),
            expected_processed_through: None, processed_through: None, no_continuation: false,
        })).await?.0;
    assert!(summary.participants.iter().any(|participant|
        participant.session_id == target && participant.processed_through >= sent.sequence));
    Ok(())
}

fn messages(state: &AppState) -> u64 {
    state.mailbox_store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get(0)).unwrap()
}

#[tokio::test]
async fn explicit_native_pair_exchanges_mail_without_proxy_or_manual_join() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let pair = qualified_pair().await;
    let central = native(&pair.central, "Ordinary central sender");
    let central_namesake = native(&pair.central, "Local namesake");
    let satellite_namesake = native(&pair.satellite, "Satellite namesake");
    let satellite = native(&pair.satellite, "Ordinary satellite recipient");
    assert_eq!(central, satellite_namesake);
    assert_eq!(satellite, central_namesake);
    for state in [&pair.central, &pair.satellite] {
        let inner = state.inner.lock().unwrap();
        assert!(inner.sessions.iter().all(|record|
            record.mailbox_binding().is_none() && record.remote_id.is_none()));
    }
    let result = async {
        let sent = ordinary(&pair.central, &central, &address(&pair.satellite, &satellite),
            "explicit-native-outbound").await?;
        let locator = MailboxRelayLocator::decode(&sent.mailbox_id, false).unwrap().unwrap();
        let recipient = pair.satellite.native_mailbox_relay_descriptor(&satellite)?.principal;
        assert!(pair.satellite.mailbox_store.mailbox_relay_association(&recipient, &locator.id).unwrap().is_none(),
            "the first recipient has not received a worker wake or learned an association");
        let stranger = native(&pair.satellite, "Unrelated root");
        assert!(read_mailbox(AxumPath((stranger.clone(), sent.mailbox_id.clone())),
            State(pair.satellite.clone()), Json(ReadMailboxRequest {
                after_sequence: None, limit: 20, issue_receipt: true, read_key: None,
            })).await.is_err(), "the locator cannot grant another native root membership");
        let stranger = pair.satellite.native_mailbox_relay_descriptor(&stranger)?.principal;
        assert!(pair.satellite.mailbox_store.mailbox_relay_association(&stranger, &locator.id).unwrap().is_none());
        receive_and_ack(&pair.satellite, &satellite, &sent).await?;
        assert!(pair.satellite.mailbox_store.mailbox_relay_association(&recipient, &locator.id).unwrap().is_some());
        assert!(read_mailbox_message(AxumPath((central_namesake, sent.message_id.clone())),
            State(pair.central.clone())).await.is_err(), "the same local id is a different recipient");
        let reply = ordinary(&pair.satellite, &satellite, &address(&pair.central, &central),
            "explicit-native-return").await?;
        receive_and_ack(&pair.central, &central, &reply).await?;
        assert_eq!(sent.mailbox_id, reply.mailbox_id);
        let duplicate = ordinary(&pair.central, &central, &address(&pair.satellite, &satellite),
            "explicit-native-outbound").await?;
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.message_id, sent.message_id);
        Ok::<_, ApiError>(())
    }.await;
    pair.finish().await;
    result.expect("both ordinary qualified sends and their receipt retries must work");
}

#[tokio::test]
async fn bare_native_ids_stay_local_when_the_same_ids_exist_on_the_peer() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let pair = qualified_pair().await;
    let central_sender = native(&pair.central, "Central sender");
    let central_target = native(&pair.central, "Central target");
    let satellite_sender = native(&pair.satellite, "Satellite sender");
    let satellite_target = native(&pair.satellite, "Satellite target");
    assert_eq!(central_target, satellite_target);
    let result = async {
        for (state, sender, target, other) in [
            (&pair.central, &central_sender, &central_target, &pair.satellite),
            (&pair.satellite, &satellite_sender, &satellite_target, &pair.central),
        ] {
            let sent = ordinary(state, sender, target, "bare-stays-local").await?;
            receive_and_ack(state, target, &sent).await?;
            assert!(read_mailbox_message(AxumPath((target.clone(), sent.message_id)),
                State(other.clone())).await.is_err(), "a bare id cannot deliver on the other host");
        }
        Ok::<_, ApiError>(())
    }.await;
    pair.finish().await;
    result.expect("bare native ids retain local-only semantics in each direction");
}

#[tokio::test]
async fn absent_bare_native_refuses_with_explicit_remote_address_guidance() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let pair = qualified_pair().await;
    let sender = native(&pair.central, "Local sender");
    native(&pair.satellite, "Other host sender");
    let remote_only = native(&pair.satellite, "Remote root absent locally");
    let sent = ordinary(&pair.central, &sender, &remote_only, "missing-local-root").await;
    let stored = messages(&pair.central) + messages(&pair.satellite);
    pair.finish().await;
    let error = sent.expect_err("a missing bare id never searches other hosts");
    assert_eq!(stored, 0);
    assert!(error.message.contains("host:") && error.message.to_ascii_lowercase().contains("native"),
        "the refusal must explain the explicit host/native form: {}", error.message);
}

#[tokio::test]
async fn bare_central_proxy_refuses_a_new_remote_send_with_its_native_address() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let pair = qualified_pair().await;
    let sender = native(&pair.central, "Ordinary sender");
    let proxy = native(&pair.central, "UI projection only");
    let remote = native(&pair.satellite, "Remote native root");
    let expected = address(&pair.satellite, &remote);
    let peer = pair.central.federation_peer_snapshot(&pair.id).unwrap();
    {
        let mut inner = pair.central.inner.lock().unwrap();
        inner.preferences.remotes.push(RemoteConfig {
            id: "addressing-test-peer".into(), name: "Test peer".into(), enabled: true,
            transport: RemoteTransport::Ssh, host: Some(peer.endpoint.ssh_host.clone()),
            port: Some(peer.endpoint.ssh_port), user: Some(peer.endpoint.ssh_user.clone()),
        });
        let index = inner.find_session_index(&proxy).unwrap();
        inner.sessions[index].remote_id = Some("addressing-test-peer".into());
        inner.sessions[index].remote_session_id = Some(remote);
    }
    let sent = ordinary(&pair.central, &sender, &proxy, "fresh-bare-proxy").await;
    let stored = messages(&pair.central) + messages(&pair.satellite);
    pair.finish().await;
    let error = sent.expect_err("a projection id cannot initiate a new remote send");
    assert_eq!(stored, 0, "a refusal does not append anywhere");
    assert!(error.message.contains(&expected), "give the explicit native address: {}", error.message);
}

#[tokio::test]
async fn missing_exact_native_pair_never_falls_back_to_a_display_name() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let pair = qualified_pair().await;
    let sender = native(&pair.central, "Sender");
    let missing = address(&pair.satellite, "session-99999");
    native(&pair.central, &missing);
    native(&pair.satellite, "Recipient decoy");
    let sent = ordinary(&pair.central, &sender, &missing, "exact-does-not-name-fallback").await;
    let stored = messages(&pair.central) + messages(&pair.satellite);
    pair.finish().await;
    assert!(sent.is_err(), "an absent native id cannot fall back to the local literal namesake");
    assert_eq!(stored, 0);
}

#[tokio::test]
async fn local_child_refusal_never_searches_the_peer_for_the_same_bare_id() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let pair = qualified_pair().await;
    let sender = native(&pair.central, "Sender");
    let child = native(&pair.central, "Local child");
    native(&pair.satellite, "Remote sender");
    let remote = native(&pair.satellite, "Remote root with same id");
    assert_eq!(child, remote);
    {
        let mut inner = pair.central.inner.lock().unwrap();
        let index = inner.find_session_index(&child).unwrap();
        inner.sessions[index].session.parent_delegation_id = Some("addressing-child".into());
    }
    let sent = ordinary(&pair.central, &sender, &child, "bare-child-no-fallback").await;
    let stored = messages(&pair.central) + messages(&pair.satellite);
    pair.finish().await;
    assert!(sent.is_err(), "a local child cannot redirect a bare id to the remote root");
    assert_eq!(stored, 0);
}

#[tokio::test]
async fn unenrolled_host_uuid_never_resolves_to_a_local_literal_namesake() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let pair = qualified_pair().await;
    let sender = native(&pair.central, "Sender");
    let target = format!("host:{}::session-2", Uuid::new_v4());
    native(&pair.central, &target);
    let sent = ordinary(&pair.central, &sender, &target, "unknown-host-no-fallback").await;
    let stored = messages(&pair.central) + messages(&pair.satellite);
    pair.finish().await;
    assert!(sent.is_err(), "an exact UUID cannot fall back to a display-name match");
    assert_eq!(stored, 0);
}

async fn refused_peer_case(condition: &str) {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let pair = federation_onboarding_fixture::Pair::with_enrolled_profile(
        federation_mailboxes::central_store(), federation_onboarding_fixture::io(),
        federation_onboarding_fixture::io(), federation_onboarding_fixture::profile(),
    ).await;
    let sender = native(&pair.central, "Sender");
    let target = native(&pair.satellite, "Remote root");
    if condition != "unverified" {
        assert!(pair.verify().await.unwrap().termal_complete());
    }
    if condition == "revoked" {
        let peer = pair.central.federation_peer_snapshot(&pair.id).unwrap();
        pair.central.transition_federation_peer(&peer, FederationMembershipState::Revoked).unwrap();
    } else if condition == "stale" {
        pair.lease.channel.invalidate();
    }
    let sent = ordinary(&pair.central, &sender, &address(&pair.satellite, &target), condition).await;
    let stored = messages(&pair.central) + messages(&pair.satellite);
    pair.finish().await;
    let error = sent.expect_err("explicit addressing cannot grant host qualification");
    assert!(!error.message.is_empty());
    assert_eq!(stored, 0);
}

#[tokio::test]
async fn explicit_native_target_refuses_unverified_peer() { refused_peer_case("unverified").await; }

#[tokio::test]
async fn explicit_native_target_refuses_revoked_peer() { refused_peer_case("revoked").await; }

#[tokio::test]
async fn explicit_native_target_refuses_stale_link() { refused_peer_case("stale").await; }

#[tokio::test]
async fn ordinary_local_to_local_send_keeps_the_existing_public_path() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let (state, sender, target) = mailboxes::mailbox_test_state();
    let sent = ordinary(&state, &sender, &target, "ordinary-local-control").await.unwrap();
    receive_and_ack(&state, &target, &sent).await.unwrap();
    let duplicate = ordinary(&state, &sender, &target, "ordinary-local-control").await.unwrap();
    assert_eq!(duplicate.message_id, sent.message_id);
    assert!(duplicate.duplicate);
}

#[test]
fn mcp_send_preserves_the_explicit_native_locator_without_directory_resolution() {
    let target = format!("host:{}::session-2", Uuid::new_v4());
    let expected = target.clone();
    let (base_url, requests, server) = crate::delegation_mcp_tests::spawn_test_mcp_http_server(1, move |request| {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/sessions/session-parent/mailboxes/send");
        let body: Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(body["targetSessionId"], expected);
        assert_eq!(body["idempotencyKey"], "explicit-mcp-native");
        (202, json!({"mailboxId":"mailbox-native", "messageId":"mailbox-message-native",
            "sequence":1, "unreadDepth":1, "notificationDisposition":"queuedBehindActiveTurn",
            "duplicate":false, "senderProcessedThrough":1, "senderCursorAdvanced":true}))
    });
    let bridge = TermalDelegationMcpBridge::new("session-parent".into(), base_url).unwrap();
    let sent = bridge.tool_send_to_session(json!({"sessionId":target,
        "message":"ordinary", "idempotencyKey":"explicit-mcp-native"}));
    server.join().unwrap();
    assert_eq!(sent.unwrap()["sessionId"], target);
    assert_eq!(requests.lock().unwrap().len(), 1, "the backend owns exact native admission");
}

#[tokio::test]
async fn retained_domain_send_keeps_its_receipt_when_the_native_relay_becomes_available() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let pair = qualified_pair().await;
    let sender = native(&pair.central, "Sender");
    let target = native(&pair.satellite, "Recipient");
    directory(&pair.central, &sender).await;
    let requested = address(&pair.satellite, &target);
    let delivery = pair.central.prepare_federation_delivery(&sender, FederationMailboxOperation::Send {
        target: requested.clone(), message: "retained-domain".into(), key: "retained-domain".into(),
        topic: None, state_stamp: None,
    }).await.unwrap();
    let original = pair.central.send_prepared_federation_delivery(&sender, delivery).await.unwrap();
    let retry = ordinary(&pair.central, &sender, &requested, "retained-domain").await.unwrap();
    assert!(retry.duplicate);
    assert_eq!((retry.mailbox_id, retry.message_id), (original.mailbox_id, original.message_id));
    let connection = pair.central.mailbox_store.connection().unwrap();
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM mailbox_messages", [], |row| row.get::<_, u64>(0)).unwrap(), 1);
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM mailbox_relay_intents", [], |row| row.get::<_, u64>(0)).unwrap(), 0);
    drop(connection);
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_domain_and_relay_sends_share_one_native_sender_key() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let pair = qualified_pair().await;
    let sender = native(&pair.central, "Sender");
    let local = native(&pair.central, "Local recipient");
    let remote = native(&pair.satellite, "Remote recipient");
    directory(&pair.central, &sender).await;
    let qualified = address(&pair.satellite, &remote);
    let (local_result, domain_result, relay_result) = tokio::join!(
        super::regressions::public_send(&pair.central, &sender, &local, "one-native-key", "local"),
        super::regressions::public_send(&pair.central, &sender, "Local recipient", "one-native-key", "domain"),
        super::regressions::public_send(&pair.central, &sender, &qualified, "one-native-key", "relay"));
    assert_eq!([local_result.is_ok(), domain_result.is_ok(), relay_result.is_ok()]
        .into_iter().filter(|success| *success).count(), 1,
        "local={local_result:?}, domain={domain_result:?}, relay={relay_result:?}");
    assert_eq!(messages(&pair.central) + messages(&pair.satellite), 1);
    let connection = pair.central.mailbox_store.connection().unwrap();
    let intents: u64 = connection.query_row(
        "SELECT (SELECT COUNT(*) FROM federation_mailbox_intents)+(SELECT COUNT(*) FROM mailbox_relay_intents)",
        [], |row| row.get(0)).unwrap();
    assert_eq!(intents, u64::from(!local_result.is_ok()));
    drop(connection);
    pair.finish().await;
}
