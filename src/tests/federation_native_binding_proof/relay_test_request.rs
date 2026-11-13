// Typed sends use the existing real authenticated two-host relay fixture.
// The private mapping below is fixture data for retention, not an Engram claim.
use super::*;

async fn connect_sender(fixture: &Fixture) -> (DomainServer, RelayHandleGuard) {
    let channel = fixture.channel.clone();
    let checked = channel.clone();
    let server = DomainServer::start(federation_router(
        channel,
        Arc::new(move || checked.current_authority().map(|_| ())),
    ))
    .await;
    let channel = fixture.satellite_channel.clone();
    let checked = channel.clone();
    let rpc = FederationRpcClient::new(
        server.port,
        channel,
        Arc::new(move || checked.current_authority().map(|_| ())),
    )
    .unwrap();
    let peer = fixture.satellite_channel.current_authority().unwrap();
    fixture
        .satellite
        .federation_peer_handles
        .lock()
        .unwrap()
        .insert(
            peer.peer.host_id,
            Arc::new(FederationPeerHandle {
                channel: fixture.satellite_channel.clone(),
                rpc,
            }),
        );
    (server, RelayHandleGuard(vec![fixture.satellite.clone()]))
}

fn send_request(target: String, key: &str) -> SendMailboxMessageRequest {
    SendMailboxMessageRequest {
        target_session_id: target,
        message: "test the retained committed input".into(),
        idempotency_key: key.into(),
        topic: None,
        state_stamp: None,
        class: Some("routine".into()),
    }
}

fn private_scope(fixture: &Fixture) -> MailboxTestSenderScope {
    MailboxTestSenderScope {
        project_id: "fixture-project".into(),
        store: EngramAuthorityStoreKey {
            database_path: fixture
                .satellite
                .mailbox_store
                .path
                .with_extension("private-engram-fixture"),
            project_id: "fixture-store-project".into(),
        },
    }
}

fn intent_custody(store: &MailboxStore, sender: &str, key: &str) -> (String, String) {
    store
        .connection()
        .unwrap()
        .query_row(
            "SELECT test_request_json,test_request_scope_json FROM mailbox_relay_intents
             WHERE owner_session_id=?1 AND idempotency_key=?2",
            rusqlite::params![sender, key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_relay_send_pins_private_scope_and_replays_only_original_custody() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let (mut server, handles) = connect_sender(&fixture).await;
    let sender = fixture
        .satellite
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap()
        .principal;
    let recipient = fixture
        .central
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap()
        .principal;
    let input = crate::tests::mailboxes::test_request::typed_request(MailboxTestMode::Focused);
    let request = send_request(
        format!("host:{}::{}", recipient.host.host_id, fixture.native),
        "typed-authenticated-send",
    );
    let mut custody = fixture
        .satellite
        .prepare_mailbox_test_request(&fixture.native, &request.idempotency_key, Some(&input))
        .unwrap()
        .unwrap();
    custody.sender_scope = Some(private_scope(&fixture));

    // The route rejects a replaced sender envelope before inserting an intent.
    // The store-level control below separately covers authorization rollback.
    let mut denied = custody.clone();
    denied.payload.requester.native_creation_id = Uuid::new_v4().to_string();
    let error = fixture
        .satellite
        .send_native_mailbox_relay_with_test_custody(&fixture.native, &request, Some(&denied))
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error
            .message
            .contains("sender does not match authenticated append"),
        "{error:?}"
    );
    assert!(
        fixture
            .satellite
            .mailbox_store
            .retained_mailbox_test_payload(&fixture.native, &request.idempotency_key)
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .central
            .mailbox_store
            .read_idempotent_message(&sender.participant_key(), &request.idempotency_key)
            .unwrap()
            .is_none()
    );

    let first = fixture
        .satellite
        .send_native_mailbox_relay_with_test_custody(&fixture.native, &request, Some(&custody))
        .await
        .unwrap()
        .unwrap();
    let expected = custody.for_recipient(&recipient).unwrap();
    let original = intent_custody(
        &fixture.satellite.mailbox_store,
        &fixture.native,
        &request.idempotency_key,
    );
    assert_eq!(
        serde_json::from_str::<MailboxTestPayload>(&original.0).unwrap(),
        expected.payload
    );
    assert_eq!(
        serde_json::from_str::<MailboxTestSenderScope>(&original.1).unwrap(),
        expected.sender_scope.clone().unwrap()
    );
    let home_message = fixture
        .central
        .mailbox_store
        .read_idempotent_message(&sender.participant_key(), &request.idempotency_key)
        .unwrap()
        .unwrap();
    let stored = fixture
        .central
        .mailbox_store
        .read_message(&recipient.participant_key(), &home_message.message_id)
        .unwrap();
    assert_eq!(stored.test_request, Some(expected.payload.clone()));
    let home_scope: Option<String> = fixture
        .central
        .mailbox_store
        .connection()
        .unwrap()
        .query_row(
            "SELECT test_request_scope_json FROM mailbox_messages WHERE id=?1",
            [&home_message.message_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        home_scope.is_none(),
        "sender-private mapping must not cross the wire"
    );

    let replay = fixture
        .satellite
        .prepare_mailbox_test_request(&fixture.native, &request.idempotency_key, Some(&input))
        .unwrap()
        .unwrap();
    assert!(
        replay.service.is_none() && replay.roots.is_empty(),
        "replay must not read fresh held claims"
    );
    assert_eq!(replay.payload, expected.payload);
    assert_eq!(replay.sender_scope, expected.sender_scope);
    let again = fixture
        .satellite
        .send_native_mailbox_relay_with_test_custody(&fixture.native, &request, Some(&replay))
        .await
        .unwrap()
        .unwrap();
    assert!(again.duplicate);
    assert_eq!(again.message_id, first.message_id);
    assert_eq!(again.mailbox_id, first.mailbox_id);
    assert_eq!(again.sequence, first.sequence);

    for change_scope in [true, false] {
        let mut changed = replay.clone();
        if change_scope {
            changed
                .sender_scope
                .as_mut()
                .unwrap()
                .project_id
                .push_str("-replacement");
        } else {
            changed.payload.mode = MailboxTestMode::Full;
        }
        let error = fixture
            .satellite
            .send_native_mailbox_relay_with_test_custody(&fixture.native, &request, Some(&changed))
            .await
            .unwrap_err();
        assert!(
            error.message.contains("pinned to its original payload"),
            "{error:?}"
        );
        assert_eq!(
            intent_custody(
                &fixture.satellite.mailbox_store,
                &fixture.native,
                &request.idempotency_key
            ),
            original
        );
    }
    assert_eq!(
        fixture
            .central
            .mailbox_store
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM mailbox_messages", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        1,
        "replay/refusals must not append another home message"
    );

    let reopened = MailboxStore::open(&fixture.satellite.mailbox_store.path).unwrap();
    assert_eq!(
        reopened
            .retained_mailbox_test_payload(&fixture.native, &request.idempotency_key)
            .unwrap()
            .unwrap(),
        (Some(expected.payload), expected.sender_scope)
    );
    assert_eq!(
        intent_custody(&reopened, &fixture.native, &request.idempotency_key),
        original
    );
    server.finish().await;
    drop(handles);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_relay_intent_reopen_refuses_corruption_and_ambiguous_original_lookup() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let sender = fixture
        .satellite
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap();
    let recipient = fixture
        .central
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap();
    let input = crate::tests::mailboxes::test_request::typed_request(MailboxTestMode::Full);
    let custody = fixture
        .satellite
        .prepare_mailbox_test_request(&fixture.native, "retained-intent", Some(&input))
        .unwrap()
        .unwrap()
        .for_recipient(&recipient.principal)
        .unwrap();
    let scope = private_scope(&fixture);
    let append = MailboxRelayAppend {
        conversation: MailboxRelayConversation::new(
            fixture.central.native_mailbox_relay_home().unwrap(),
            sender.principal.clone(),
            recipient.principal.clone(),
        )
        .unwrap(),
        sender: sender.principal,
        target: recipient.principal,
        requested_target: "original-remote-selector".into(),
        sender_name: sender.name,
        target_name: recipient.name,
        message: "retained but not yet sent".into(),
        idempotency_key: "retained-intent".into(),
        topic: None,
        state_stamp: None,
        test_request: Some(custody.payload.clone()),
    };
    let peer = fixture.satellite_channel.current_authority().unwrap();
    let mut authorization_checked = false;
    let denied = fixture
        .satellite
        .mailbox_store
        .pin_mailbox_relay_with_test_scope::<()>(&append, &peer, Some(&scope), || {
            authorization_checked = true;
            Err(anyhow!("fixture authorization revoked"))
        });
    assert!(
        authorization_checked,
        "the denial must reach the actual pin owner"
    );
    assert!(
        denied
            .unwrap_err()
            .to_string()
            .contains("fixture authorization revoked")
    );
    assert!(
        fixture
            .satellite
            .mailbox_store
            .retained_mailbox_test_payload(&fixture.native, &append.idempotency_key)
            .unwrap()
            .is_none(),
        "failed authorization must roll back the inserted intent and custody"
    );
    fixture
        .satellite
        .mailbox_store
        .pin_mailbox_relay_with_test_scope(&append, &peer, Some(&scope), || Ok(()))
        .unwrap();
    let original = intent_custody(
        &fixture.satellite.mailbox_store,
        &fixture.native,
        &append.idempotency_key,
    );
    let reopened = MailboxStore::open(&fixture.satellite.mailbox_store.path).unwrap();
    assert_eq!(
        reopened
            .retained_mailbox_test_payload(&fixture.native, &append.idempotency_key)
            .unwrap()
            .unwrap(),
        (Some(custody.payload), Some(scope))
    );
    drop(reopened);

    for (payload, mapping) in [
        (
            Some("{\"schemaVersion\":1,\"unknown\":true}"),
            Some(original.1.as_str()),
        ),
        (Some(original.0.as_str()), Some("{\"projectId\":\"\"}")),
        (None, Some(original.1.as_str())),
    ] {
        fixture
            .satellite
            .mailbox_store
            .connection()
            .unwrap()
            .execute(
                "UPDATE mailbox_relay_intents SET test_request_json=?1,test_request_scope_json=?2
             WHERE owner_session_id=?3 AND idempotency_key=?4",
                rusqlite::params![payload, mapping, fixture.native, append.idempotency_key],
            )
            .unwrap();
        assert!(
            MailboxStore::open(&fixture.satellite.mailbox_store.path).is_err(),
            "boot admission must refuse malformed or orphaned relay custody"
        );
        let retained: (Option<String>, Option<String>) = fixture
            .satellite
            .mailbox_store
            .connection()
            .unwrap()
            .query_row(
                "SELECT test_request_json,test_request_scope_json FROM mailbox_relay_intents
                 WHERE owner_session_id=?1 AND idempotency_key=?2",
                rusqlite::params![fixture.native, append.idempotency_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            retained,
            (payload.map(str::to_owned), mapping.map(str::to_owned)),
            "refusal must preserve the original failing row"
        );
    }
    fixture
        .satellite
        .mailbox_store
        .connection()
        .unwrap()
        .execute(
            "UPDATE mailbox_relay_intents SET test_request_json=?1,test_request_scope_json=?2
         WHERE owner_session_id=?3 AND idempotency_key=?4",
            rusqlite::params![
                original.0,
                original.1,
                fixture.native,
                append.idempotency_key
            ],
        )
        .unwrap();
    assert!(MailboxStore::open(&fixture.satellite.mailbox_store.path).is_ok());

    // Deliberately corrupt an ordinary row into the same sender/key namespace;
    // neither the local-message nor relay-intent branch may win the UNION.
    let target = test_session_id(&fixture.satellite, Agent::Codex);
    let ordinary = fixture
        .satellite
        .append_native_mailbox_message_with_test_custody(
            &fixture.native,
            send_request(target, "separate-local-send"),
            NativeMailboxNamespace::Local,
            None,
        )
        .unwrap();
    fixture
        .satellite
        .mailbox_store
        .connection()
        .unwrap()
        .execute(
            "UPDATE mailbox_messages SET idempotency_key=?1 WHERE id=?2",
            rusqlite::params![append.idempotency_key, ordinary.message_id],
        )
        .unwrap();
    let error = fixture
        .satellite
        .mailbox_store
        .retained_mailbox_test_payload(&fixture.native, &append.idempotency_key)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("ambiguous original mailbox custody")
    );
    assert!(
        fixture
            .satellite
            .prepare_mailbox_test_request(&fixture.native, &append.idempotency_key, Some(&input))
            .is_err()
    );
    assert_eq!(
        intent_custody(
            &fixture.satellite.mailbox_store,
            &fixture.native,
            &append.idempotency_key
        ),
        original
    );
    fixture.finish().await;
}
