// Owns central mailbox routing, opposite-first-send, and namespace fencing tests.
// Does not provision SSH or claim sender-intent/wake-journal guarantees. New
// M2 integration coverage over real isolated SQLite and the public API facade.
use super::*;

pub(super) fn central_store() -> AppState {
    let base = federation_registry::central();
    let path = resolve_coordination_persistence_path(base.persistence_path.as_path());
    AppState {
        mailbox_store: Arc::new(MailboxStore::open(&path).unwrap()),
        ..base
    }
}

pub(super) fn local_participant(
    state: &AppState,
    name: &str,
) -> (String, FederationMailboxParticipant) {
    let id = test_session_id(state, Agent::Codex);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        inner.session_mut_by_index(index).unwrap().session.name = name.to_owned();
        inner.session_mut_by_index(index).unwrap().session.status = SessionStatus::Active;
    }
    let participant = state.join_federation_mailbox_session(&id).unwrap();
    (id, participant)
}

pub(super) fn request(
    state: &AppState,
    local: &str,
    operation: FederationMailboxOperation,
) -> FederationMailboxRequest {
    let central = state
        .inner
        .lock()
        .unwrap()
        .federation
        .local
        .clone()
        .unwrap();
    FederationMailboxRequest {
        protocol_version: 1,
        schema_version: "1".to_owned(),
        central,
        actor: state.federation_mailbox_actor(local).unwrap(),
        operation,
    }
}

fn send(target: &str, key: &str) -> FederationMailboxOperation {
    FederationMailboxOperation::Send {
        target: target.to_owned(),
        message: "durable central body".to_owned(),
        key: key.to_owned(),
        topic: None,
        state_stamp: None,
    }
}

#[tokio::test]
async fn explicit_empty_mailbox_creation_is_idempotent_and_visible_before_first_send() {
    let state = central_store();
    let (a, pa) = local_participant(&state, "A");
    let (b, pb) = local_participant(&state, "B");
    let mut created = Vec::new();
    for (caller, target) in [(&a, &pb.session_id), (&b, &pa.session_id)] {
        let mailbox: MailboxSummary = state
            .federation_mailbox_operation(
                caller,
                FederationMailboxOperation::GetOrCreate {
                    target: target.clone(),
                },
            )
            .await
            .unwrap();
        assert_eq!(mailbox.latest_sequence, 0);
        assert_eq!(mailbox.unread_count, 0);
        assert_eq!(mailbox.participants.len(), 2);
        assert!(mailbox.latest_message_preview.is_none());
        assert!(mailbox.latest_message_at.is_none());
        let listed: Vec<MailboxSummary> = state
            .federation_mailbox_operation(caller, FederationMailboxOperation::List)
            .await
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, mailbox.id);
        created.push(mailbox.id);
    }
    assert_eq!(created[0], created[1]);
    let connection = state.mailbox_store.connection().unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM mailbox_messages", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
    // Standalone callers retain the existing populated-mailbox-only view.
    assert!(mailbox_summaries_for_session(&connection, &pa.session_id)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn opposite_first_sends_and_creation_use_one_mailbox_and_one_pair_of_memberships() {
    let state = central_store();
    let (a, pa) = local_participant(&state, "A");
    let (b, pb) = local_participant(&state, "B");
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let mut threads = Vec::new();
    for (caller, target, key) in [
        (a.clone(), pb.session_id.clone(), "a"),
        (b.clone(), pa.session_id.clone(), "b"),
    ] {
        let app = state.clone();
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            let req = request(&app, &caller, send(&target, key));
            barrier.wait();
            app.execute_federation_mailbox(req, None).unwrap().value
        }));
    }
    barrier.wait();
    let first = threads.remove(0).join().unwrap();
    let second = threads.remove(0).join().unwrap();
    assert_eq!(first["mailboxId"], second["mailboxId"]);
    let conn = state.mailbox_store.connection().unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM mailboxes", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM mailbox_participants", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(DISTINCT participant_key) FROM mailboxes",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        1
    );
    drop(conn);
    let mailbox: MailboxSummary = state
        .federation_mailbox_operation(
            &a,
            FederationMailboxOperation::GetOrCreate {
                target: pb.session_id,
            },
        )
        .await
        .unwrap();
    assert_eq!(mailbox.id, first["mailboxId"].as_str().unwrap());
    assert_eq!(mailbox.latest_sequence, 2);
    let inner = state.inner.lock().unwrap();
    assert_eq!(
        inner.sessions[inner.find_session_index(&a).unwrap()]
            .queued_prompts
            .len(),
        1
    );
    assert_eq!(
        inner.sessions[inner.find_session_index(&b).unwrap()]
            .queued_prompts
            .len(),
        1
    );
}

#[tokio::test]
async fn domain_read_issues_receipts_and_exact_retry_keeps_the_canonical_append() {
    let state = central_store();
    let (a, pa) = local_participant(&state, "A");
    let (b, pb) = local_participant(&state, "B");
    let first = state
        .execute_federation_mailbox(request(&state, &a, send(&pb.session_id, "same")), None)
        .unwrap()
        .value;
    let duplicate = state
        .execute_federation_mailbox(request(&state, &a, send(&pb.session_id, "same")), None)
        .unwrap()
        .value;
    assert_eq!(first["messageId"], duplicate["messageId"]);
    assert_eq!(duplicate["duplicate"], true);
    let mailbox = first["mailboxId"].as_str().unwrap().to_owned();
    let page: MailboxReadResponse = state
        .federation_mailbox_operation(
            &b,
            FederationMailboxOperation::Read {
                mailbox_id: mailbox.clone(),
                after_sequence: None,
                limit: 50,
                issue_receipt: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.reader_session_id, Some(pb.session_id.clone()));
    assert_eq!(page.messages[0].sender_session_id, pa.session_id);
    let exact: MailboxMessage = state
        .federation_mailbox_operation(
            &b,
            FederationMailboxOperation::ReadMessage {
                message_id: first["messageId"].as_str().unwrap().to_owned(),
            },
        )
        .await
        .unwrap();
    assert_eq!(exact.body, "durable central body");
    let receipt = page.receipt.unwrap();
    let summary: MailboxSummary = state
        .federation_mailbox_operation(
            &b,
            FederationMailboxOperation::Acknowledge {
                mailbox_id: mailbox.clone(),
                receipt: receipt.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(summary.unread_count, 0);
    assert_eq!(
        state
            .federation_mailbox_operation::<MailboxSummary>(
                &a,
                FederationMailboxOperation::Acknowledge {
                    mailbox_id: mailbox,
                    receipt
                }
            )
            .await
            .unwrap_err()
            .status,
        StatusCode::BAD_REQUEST
    );
    assert!(
        state
            .inner
            .lock()
            .unwrap()
            .sessions
            .iter()
            .find(|r| r.session.id == b)
            .unwrap()
            .queued_prompts[0]
            .pending_prompt
            .text
            .contains("already acknowledged")
    );
}

#[tokio::test]
async fn numeric_ack_is_rejected_for_domain_members_but_standalone_behavior_remains() {
    let state = central_store();
    let (a, _) = local_participant(&state, "A");
    let response = app_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{a}/mailboxes/unknown/acknowledge"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"expectedProcessedThrough":0,"processedThrough":1}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let sender = test_session_id(&state, Agent::Codex);
    let target = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().unwrap();
        for id in [&sender, &target] {
            let index = inner.find_session_index(id).unwrap();
            inner.session_mut_by_index(index).unwrap().session.status = SessionStatus::Active;
        }
    }
    let sent = state.append_mailbox_message_and_notify(&sender, SendMailboxMessageRequest {
        target_session_id: target.clone(), message: "standalone numeric ACK".to_owned(),
        idempotency_key: "numeric".to_owned(), topic: None, state_stamp: None, class: None,
    }).unwrap();
    state.mailbox_store.read_range_with_issuance(&target, &sent.mailbox_id, None, 50, true).unwrap();
    let response = app_router(state).oneshot(Request::builder().method("POST")
        .uri(format!("/api/sessions/{target}/mailboxes/{}/acknowledge", sent.mailbox_id))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"expectedProcessedThrough":0,"processedThrough":1}"#)).unwrap())
        .await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn central_domain_receipt_cannot_silently_accept_a_local_coverage_declaration() {
    let _temp = TestTempRootDirectoryScope::repository_local();
    let state = central_store();
    let (sender, _) = local_participant(&state, "Sender");
    let (target, participant) = local_participant(&state, "Target");
    let sent: MailboxAppendReceipt = state.federation_mailbox_operation(
        &sender, send(&participant.session_id, "coverage-declaration"),
    ).await.unwrap();
    let page: MailboxReadResponse = state.federation_mailbox_operation(
        &target, FederationMailboxOperation::Read {
            mailbox_id: sent.mailbox_id.clone(), after_sequence: None,
            limit: 10, issue_receipt: true,
        },
    ).await.unwrap();
    let receipt = page.receipt.unwrap();
    let error = acknowledge_mailbox(
        AxumPath((target.clone(), sent.mailbox_id.clone())), State(state.clone()),
        Json(AcknowledgeMailboxRequest {
            receipt: Some(receipt.clone()), expected_processed_through: None,
            processed_through: None, no_continuation: true,
        }),
    ).await.unwrap_err();
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("noContinuation is unavailable for central-domain"));
    assert_eq!(mailbox_processed_through(
        &state.mailbox_store.connection().unwrap(), &sent.mailbox_id, &participant.session_id,
    ).unwrap(), 0);
    let summary = acknowledge_mailbox(
        AxumPath((target, sent.mailbox_id)), State(state.clone()),
        Json(AcknowledgeMailboxRequest {
            receipt: Some(receipt), expected_processed_through: None,
            processed_through: None, no_continuation: false,
        }),
    ).await.unwrap().0;
    assert_eq!(summary.unread_count, 0);
    assert_eq!(state.mailbox_store.connection().unwrap().query_row(
        "SELECT count(*) FROM mailbox_continuation_declarations", [], |r| r.get::<_, u64>(0),
    ).unwrap(), 0);
}

#[test]
fn recreated_local_id_and_name_inherit_no_directory_access_or_receipts() {
    let state = central_store();
    let (id, old) = local_participant(&state, "Name");
    let stale = request(&state, &id, FederationMailboxOperation::List);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&id).unwrap();
        inner.remove_session_at(index).unwrap();
        inner.next_session_number = id.trim_start_matches("session-").parse().unwrap();
    }
    let (replacement, new) = local_participant(&state, "Name");
    assert_eq!(replacement, id);
    assert_ne!(
        new.binding.identity.creation_id,
        old.binding.identity.creation_id
    );
    assert_ne!(new.session_id, old.session_id);
    assert_eq!(
        state
            .execute_federation_mailbox(stale, None)
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    assert!(
        state
            .mailbox_store
            .list_for_session(&new.session_id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn domain_protocol_schema_and_existing_size_limits_are_enforced() {
    let state = central_store();
    let (a, _) = local_participant(&state, "A");
    let (_, pb) = local_participant(&state, "B");
    for (version, schema) in [(2, "1"), (1, "future")] {
        let mut req = request(&state, &a, FederationMailboxOperation::Protocol);
        req.protocol_version = version;
        req.schema_version = schema.to_owned();
        assert_eq!(
            state
                .execute_federation_mailbox(req, None)
                .unwrap_err()
                .status,
            StatusCode::BAD_REQUEST
        );
    }
    let operation = FederationMailboxOperation::Send {
        target: pb.session_id,
        message: "x".repeat(MAX_MAILBOX_BODY_BYTES + 1),
        key: "large".to_owned(),
        topic: None,
        state_stamp: None,
    };
    assert_eq!(
        state
            .execute_federation_mailbox(request(&state, &a, operation), None)
            .unwrap_err()
            .status,
        StatusCode::BAD_REQUEST
    );
    assert!(
        state
            .mailbox_store
            .list_for_session(
                &state
                    .federation_mailbox_actor(&a)
                    .unwrap()
                    .unwrap()
                    .binding
                    .participant_key()
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn alias_collision_is_an_error_and_qualified_ids_are_unambiguous() {
    let state = central_store();
    let (_, a) = local_participant(&state, "Same");
    let (_, b) = local_participant(&state, "Same");
    let inner = state.inner.lock().unwrap();
    assert_eq!(
        resolve_federation_mailbox_participant_locked(&inner, "Same")
            .unwrap_err()
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        resolve_federation_mailbox_participant_locked(&inner, &a.session_id).unwrap(),
        a
    );
    assert_ne!(a.session_id, b.session_id);
}

#[test]
fn central_native_renames_change_discovery_without_changing_identity() {
    let state = central_store();
    let (id, participant) = local_participant(&state, "Before");
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&id).unwrap();
    inner.session_mut_by_index(index).unwrap().session.name = "After".to_owned();
    let renamed = resolve_federation_mailbox_participant_locked(&inner, "After").unwrap();
    assert_eq!(renamed.session_id, participant.session_id);
    assert_eq!(renamed.name, "After");
    assert!(resolve_federation_mailbox_participant_locked(&inner, "Before").is_err());
}
