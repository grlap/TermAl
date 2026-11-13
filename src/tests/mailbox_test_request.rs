// Public mailbox admission must not silently turn malformed test requests
// into ordinary mail. Uses the real route and coordination-store readback.
use super::*;

pub(in crate::tests) fn typed_request(mode: MailboxTestMode) -> MailboxTestRequest {
    MailboxTestRequest {
        mode,
        targets: vec![MailboxTestTarget {
            work_ref: "w-111111111111".into(),
            r#use: MailboxTestInputUse::CommittedInput {
                expected_commit: "a".repeat(40),
                purpose: MailboxTestPurpose::Verification,
            },
        }],
    }
}

fn native_fixture() -> (AppState, String, String) {
    let state = crate::tests::federation_mailboxes::central_store();
    let (sender, _) =
        crate::tests::federation_mailboxes::local_participant(&state, "Custody sender");
    let (target, _) =
        crate::tests::federation_mailboxes::local_participant(&state, "Custody runner");
    (state, sender, target)
}

#[test]
fn typed_send_captures_each_target_once_and_retains_the_original_on_retry() {
    let (state, sender, target) = native_fixture();
    let request = typed_request(MailboxTestMode::Focused);
    let custody = state
        .prepare_mailbox_test_request(&sender, "captured-send", Some(&request))
        .unwrap()
        .unwrap();
    assert!(
        custody.payload.recipient.is_none(),
        "only the route chooses the actual native recipient"
    );
    assert_eq!(custody.payload.targets[0].association, None);
    assert!(
        custody.payload.targets[0]
            .uncredited_reason
            .as_deref()
            .unwrap()
            .starts_with("producer_unsupported")
    );
    let mut input = mailbox_send_request(&target);
    input.idempotency_key = "captured-send".into();
    let first = state
        .append_native_mailbox_message_with_test_custody(
            &sender,
            input.clone(),
            NativeMailboxNamespace::Local,
            Some(custody),
        )
        .unwrap();
    let retained = state
        .mailbox_store
        .read_message(&target, &first.message_id)
        .unwrap();
    assert_eq!(
        retained
            .test_request
            .as_ref()
            .unwrap()
            .recipient
            .as_ref()
            .unwrap()
            .native_session_id,
        target
    );
    let retry = state
        .prepare_mailbox_test_request(&sender, "captured-send", Some(&request))
        .unwrap()
        .unwrap();
    assert!(
        retry.service.is_none(),
        "replay reuses the original capture, not a fresh held-claim read"
    );
    let second = state
        .append_native_mailbox_message_with_test_custody(
            &sender,
            input,
            NativeMailboxNamespace::Local,
            Some(retry),
        )
        .unwrap();
    assert_eq!(first.message_id, second.message_id);
    assert!(second.duplicate);
    assert_eq!(
        state
            .mailbox_store
            .read_range(&target, &first.mailbox_id, Some(0), 100)
            .unwrap()
            .messages
            .len(),
        1
    );
    let mut changed = request;
    changed.mode = MailboxTestMode::Full;
    assert!(
        state
            .prepare_mailbox_test_request(&sender, "captured-send", Some(&changed))
            .is_err()
    );
    let reopened = MailboxStore::open(&state.mailbox_store.path).unwrap();
    assert_eq!(
        reopened
            .read_message(&target, &first.message_id)
            .unwrap()
            .test_request,
        retained.test_request
    );
}

#[test]
fn recipient_with_original_send_key_cannot_append_as_sender_or_retarget_a_replacement() {
    let (state, sender, target) = native_fixture();
    let request = typed_request(MailboxTestMode::Full);
    let original = state
        .prepare_mailbox_test_request(&sender, "sender-only-key", Some(&request))
        .unwrap()
        .unwrap();
    let recipient = state
        .native_mailbox_relay_descriptor(&target)
        .unwrap()
        .principal;
    let captured = original.for_recipient(&recipient).unwrap();
    let mut reply = mailbox_send_request(&sender);
    reply.idempotency_key = "sender-only-key".into();
    assert!(
        state
            .append_native_mailbox_message_with_test_custody(
                &target,
                reply,
                NativeMailboxNamespace::Local,
                Some(captured.clone())
            )
            .is_err()
    );
    // Even a structurally valid internal envelope cannot make its recipient
    // act as the authenticated original sender merely by copying the key.
    let (third, _) = crate::tests::federation_mailboxes::local_participant(&state, "Third root");
    let mut forged = captured.clone();
    forged.payload.recipient = Some(
        state
            .native_mailbox_relay_descriptor(&third)
            .unwrap()
            .principal,
    );
    let mut input = mailbox_send_request(&third);
    input.idempotency_key = "sender-only-key".into();
    let error = state
        .append_native_mailbox_message_with_test_custody(
            &target,
            input,
            NativeMailboxNamespace::Local,
            Some(forged),
        )
        .unwrap_err();
    assert!(
        error.message.contains("sender identity changed"),
        "{error:?}"
    );
    let mut replacement = recipient;
    replacement.native_creation_id = Uuid::new_v4().to_string();
    assert!(captured.for_recipient(&replacement).is_err());
    assert!(!format!("{:?}", captured.payload).contains("sender-only-key"));
    assert!(
        state
            .mailbox_store
            .list_for_session(&sender)
            .unwrap()
            .is_empty()
    );
}

fn domain_read_fixture() -> (
    AppState,
    FederationMailboxAuthority,
    MailboxAppendInput,
    MailboxTestPayload,
) {
    let (state, sender, target) = native_fixture();
    let payload = state
        .prepare_mailbox_test_request(
            &sender,
            "domain-custody",
            Some(&typed_request(MailboxTestMode::Full)),
        )
        .unwrap()
        .unwrap()
        .for_recipient(
            &state
                .native_mailbox_relay_descriptor(&target)
                .unwrap()
                .principal,
        )
        .unwrap()
        .payload;
    let actor = state
        .federation_mailbox_actor(&target)
        .unwrap()
        .expect("the local participant has joined the federation mailbox")
        .binding;
    let input = MailboxAppendInput {
        sender_session_id: payload.requester.participant_key(),
        sender_name: "Sender".into(),
        target_session_id: actor.participant_key(),
        target_name: "Runner".into(),
        body: "retained typed request".into(),
        idempotency_key: "domain-custody".into(),
        topic: None,
        state_stamp: None,
    };
    let authority = FederationMailboxAuthority {
        app: state.clone(),
        actor,
        lease: None,
        target: None,
    };
    (state, authority, input, payload)
}

fn append_read_controls(
    state: &AppState,
    input: &MailboxAppendInput,
    payload: &MailboxTestPayload,
) -> (String, String) {
    let scope = MailboxTestSenderScope {
        project_id: "host-project".into(),
        store: EngramAuthorityStoreKey {
            database_path: state
                .test_temp_root
                .as_ref()
                .unwrap()
                .path()
                .join("private-destination-never-export.db"),
            project_id: "example.test/private-project".into(),
        },
    };
    let typed = state
        .mailbox_store
        .append_with_test_request_custody(
            input,
            Some(payload),
            Some(&scope),
            || Ok(()),
            |_, _| Ok(()),
        )
        .unwrap();
    let mut ordinary = input.clone();
    ordinary.idempotency_key = "legacy-null-control".into();
    let legacy = state.mailbox_store.append(&ordinary).unwrap();
    (typed.receipt.message_id, legacy.receipt.message_id)
}

fn assert_public_message(message: &MailboxMessage, payload: Option<&MailboxTestPayload>) {
    assert_eq!(message.test_request.as_ref(), payload);
    let encoded = serde_json::to_string(message).unwrap();
    assert!(!encoded.contains("private-destination-never-export"));
    assert!(!encoded.contains("example.test/private-project"));
    assert!(!encoded.contains("host-project"));
    assert!(
        !encoded.contains("idempotencyKey"),
        "ordinary send key remains skipped"
    );
}

#[test]
fn local_full_message_read_keeps_custody_and_reads_legacy_null_without_private_scope() {
    let (state, authority, input, payload) = domain_read_fixture();
    let (typed, legacy) = append_read_controls(&state, &input, &payload);
    let store = MailboxStore::open(&state.mailbox_store.path).unwrap();
    let (_, private_scope) = store
        .retained_mailbox_test_payload(&input.sender_session_id, &input.idempotency_key)
        .unwrap()
        .unwrap();
    assert!(
        private_scope
            .as_ref()
            .unwrap()
            .store
            .database_path
            .ends_with("private-destination-never-export.db")
    );
    let actor = authority.actor.participant_key();
    assert_public_message(&store.read_message(&actor, &typed).unwrap(), Some(&payload));
    assert_public_message(&store.read_message(&actor, &legacy).unwrap(), None);
    // A relay connects distinct native hosts. Keep the local-read specimen
    // above unchanged and give the wire specimen an actual remote recipient.
    let mut remote_payload = payload.clone();
    remote_payload.recipient.as_mut().unwrap().host.host_id = Uuid::new_v4().to_string();
    let relay = MailboxRelayAppend {
        test_request: Some(remote_payload.clone()),
        requested_target: "native-recipient".into(),
        conversation: MailboxRelayConversation::new(
            MailboxRelayHome {
                host: payload.requester.host.clone(),
                store_id: payload.requester.store_id.clone(),
            },
            payload.requester.clone(),
            remote_payload.recipient.clone().unwrap(),
        )
        .unwrap(),
        sender: payload.requester.clone(),
        target: remote_payload.recipient.clone().unwrap(),
        sender_name: "Sender".into(),
        target_name: "Runner".into(),
        message: input.body,
        idempotency_key: payload.request_send_key.clone(),
        topic: None,
        state_stamp: None,
    };
    let wire = serde_json::to_string(&relay).unwrap();
    assert!(!wire.contains("private-destination-never-export"));
    assert!(!wire.contains("example.test/private-project"));
    assert!(!wire.contains("host-project"));
    // The receiving/home append has only the decoded public wire, and
    // explicitly retains no sender-local mapping. It does not reconstruct a
    // private store path from project labels or public captured roots.
    let imported: MailboxRelayAppend = serde_json::from_str(&wire).unwrap();
    let mut received = imported.input().unwrap();
    received.idempotency_key = "received-public-only".into();
    let mut public_payload = imported.test_request.unwrap();
    public_payload.request_send_key = received.idempotency_key.clone();
    store
        .append_with_test_request_custody(
            &received,
            Some(&public_payload),
            None,
            || Ok(()),
            |_, _| Ok(()),
        )
        .unwrap();
    let (decoded, received_scope) = store
        .retained_mailbox_test_payload(&received.sender_session_id, &received.idempotency_key)
        .unwrap()
        .unwrap();
    assert_eq!(decoded, Some(public_payload));
    assert_eq!(received_scope, None);
}

#[test]
fn whole_store_backup_keeps_private_mapping_and_refuses_partial_scope_repairs() {
    let (state, authority, input, payload) = domain_read_fixture();
    let (typed, _) = append_read_controls(&state, &input, &payload);
    let backup = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("custody-backup.sqlite");
    state
        .mailbox_store
        .connection()
        .unwrap()
        .execute("VACUUM main INTO ?1", [backup.to_str().unwrap()])
        .unwrap();
    let restored = MailboxStore::open(&backup).unwrap();
    assert_public_message(
        &restored
            .read_message(&authority.actor.participant_key(), &typed)
            .unwrap(),
        Some(&payload),
    );
    let original = state
        .mailbox_store
        .retained_mailbox_test_payload(&input.sender_session_id, &input.idempotency_key)
        .unwrap();
    assert_eq!(
        restored
            .retained_mailbox_test_payload(&input.sender_session_id, &input.idempotency_key)
            .unwrap(),
        original
    );
    // A purported repair cannot leave a private authority mapping detached
    // from its public custody. Admission refuses it without deleting either.
    restored
        .connection()
        .unwrap()
        .execute(
            "UPDATE mailbox_messages SET test_request_json=NULL WHERE id=?1",
            [&typed],
        )
        .unwrap();
    assert!(MailboxStore::open(&backup).is_err());
    let preserved: Option<String> = rusqlite::Connection::open(&backup)
        .unwrap()
        .query_row(
            "SELECT test_request_scope_json FROM mailbox_messages WHERE id=?1",
            [&typed],
            |row| row.get(0),
        )
        .unwrap();
    assert!(preserved.is_some());
}

#[test]
fn malformed_private_mapping_is_refused_before_append_and_never_logged() {
    let (state, authority, input, payload) = domain_read_fixture();
    let scope = MailboxTestSenderScope {
        project_id: String::new(),
        store: EngramAuthorityStoreKey {
            database_path: state
                .test_temp_root
                .as_ref()
                .unwrap()
                .path()
                .join("private-invalid-mapping.db"),
            project_id: "private-invalid-project".into(),
        },
    };
    assert!(
        state
            .mailbox_store
            .append_with_test_request_custody(
                &input,
                Some(&payload),
                Some(&scope),
                || Ok(()),
                |_, _| Ok(())
            )
            .is_err()
    );
    assert!(
        state
            .mailbox_store
            .list_for_session(&authority.actor.participant_key())
            .unwrap()
            .is_empty()
    );
    let debug = format!("{scope:?}");
    assert!(!debug.contains("private-invalid-mapping"));
    assert!(!debug.contains("private-invalid-project"));
}

#[test]
fn federation_exact_message_read_keeps_custody_and_reads_legacy_null() {
    let (state, authority, input, payload) = domain_read_fixture();
    let (typed, legacy) = append_read_controls(&state, &input, &payload);
    let actor = authority.actor.participant_key();
    assert_public_message(
        &state
            .mailbox_store
            .domain_read_message(&actor, &typed, &authority)
            .unwrap(),
        Some(&payload),
    );
    assert_public_message(
        &state
            .mailbox_store
            .domain_read_message(&actor, &legacy, &authority)
            .unwrap(),
        None,
    );
}

#[test]
fn federation_full_page_keeps_custody_and_reads_legacy_null() {
    let (state, authority, input, payload) = domain_read_fixture();
    let (typed, legacy) = append_read_controls(&state, &input, &payload);
    let mailbox = state
        .mailbox_store
        .read_message(&authority.actor.participant_key(), &typed)
        .unwrap()
        .mailbox_id;
    let page = state
        .capture_federation_mailbox_page(&authority, &mailbox, Some(0), 100, None)
        .unwrap();
    assert_eq!(page.messages.len(), 2);
    assert_eq!(page.messages[0].id, typed);
    assert_public_message(&page.messages[0], Some(&payload));
    assert_eq!(page.messages[1].id, legacy);
    assert_public_message(&page.messages[1], None);
}

#[tokio::test]
async fn malformed_test_request_is_refused_without_appending_ordinary_mail() {
    let (state, sender, target) = mailbox_test_state();
    let app = app_router(state.clone());
    let endpoint = format!("/api/sessions/{sender}/mailboxes/send");
    let ordinary = json!({
        "targetSessionId": target, "message": "ordinary message",
        "idempotencyKey": "ordinary-positive-control",
    });
    let post = |value: Value| {
        Request::builder()
            .method("POST")
            .uri(&endpoint)
            .header("content-type", "application/json")
            .body(Body::from(value.to_string()))
            .unwrap()
    };
    let (status, receipt): (StatusCode, Value) = request_json(&app, post(ordinary.clone())).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{receipt}");
    let mailbox = receipt["mailboxId"].as_str().unwrap();
    let committed_input = json!({
        "kind": "committed_input", "expected_commit": "a".repeat(40), "purpose": "verification",
    });
    let cases = [
        json!({"mode": "live", "targets": [{"workRef": "work", "use": committed_input}]}),
        json!({"mode": "full", "targets": []}),
        json!({"mode": "focused", "targets": [{"workRef": "work", "use": committed_input, "binding": {"claim_fence": 999}}]}),
        json!({"mode": "full", "targets": [{"workRef": "work", "use": {"kind": "authorized_test_ref", "ref_name": "refs/heads/test/example", "expected_tree": "b".repeat(40), "purpose": "verification"}}]}),
        Value::Null,
    ];
    for (index, test_request) in cases.into_iter().enumerate() {
        let mut value = ordinary.clone();
        value["idempotencyKey"] = json!(format!("invalid-test-request-{index}"));
        value["testRequest"] = test_request;
        let response = request_response(&app, post(value)).await;
        let status = response.status();
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        assert!(
            status.is_client_error(),
            "invalid case {index} was accepted or reached execution: {status}, {}",
            String::from_utf8_lossy(&body)
        );
        let read = state
            .mailbox_store
            .read_range(&target, mailbox, Some(0), 100)
            .unwrap();
        assert_eq!(
            read.messages.len(),
            1,
            "invalid metadata must not append a second ordinary message"
        );
        assert_eq!(read.messages[0].body, "ordinary message");
    }
}
