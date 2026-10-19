// Owns queued-intent status, cancellation, payload GC, and namespace controls.
// Does not grant mailbox authority from a pending row or infer never-sent
// from a negative home lookup. New facade tests beside the sender store tests.
use super::*;

pub(super) async fn intent_api(
    state: &AppState,
    sender: &str,
    suffix: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(if body.is_some() { "POST" } else { "GET" })
        .uri(format!("/api/sessions/{sender}/mailbox-intents{suffix}"));
    let body = if let Some(body) = body {
        request = request.header("content-type", "application/json");
        Body::from(serde_json::to_vec(&body).unwrap())
    } else {
        Body::empty()
    };
    let response = app_router(state.clone())
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

pub(super) fn queued_body(target: &str, key: &str) -> Value {
    json!({"targetSessionId":target,"idempotencyKey":key,"message":"exact retry body"})
}

#[tokio::test]
async fn federation_delivery_queued_status_has_no_canonical_receipt_or_mailbox_cursor() {
    let state = federation_mailboxes::central_store();
    let (sender, actor) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    let (status, queued) = intent_api(
        &state,
        &sender,
        "",
        Some(queued_body(&target.session_id, "queued")),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(queued["state"], "localIntentDurable");
    assert!(queued.get("messageId").is_none());
    assert!(queued.get("sequence").is_none());
    let (status, retained) = intent_api(&state, &sender, "/queued", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(retained["state"], "localIntentDurable");
    assert_eq!(retained["canonicalDigest"], queued["canonicalDigest"]);
    assert!(
        state
            .mailbox_store
            .list_for_session(&actor.session_id)
            .unwrap()
            .is_empty()
    );
    let connection = state.mailbox_store.connection().unwrap();
    for table in [
        "mailbox_messages",
        "mailbox_read_pages",
        "mailbox_participants",
    ] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, u64>(0))
                .unwrap(),
            0
        );
    }
}

#[tokio::test]
async fn federation_delivery_cancel_then_payload_gc_fences_a_delayed_first_append() {
    let state = federation_mailboxes::central_store();
    let (sender, actor) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    let delayed = federation_mailboxes::request(
        &state,
        &sender,
        FederationMailboxOperation::Send {
            target: target.session_id.clone(),
            message: "exact retry body".to_owned(),
            key: "cancelled".to_owned(),
            topic: None,
            state_stamp: None,
        },
    );
    assert_eq!(
        intent_api(
            &state,
            &sender,
            "",
            Some(queued_body(&target.session_id, "cancelled"))
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    let (status, cancelled) =
        intent_api(&state, &sender, "/cancelled/cancel", Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cancelled["state"], "abandoned");
    assert_eq!(
        intent_api(&state, &sender, "/cancelled/gc", Some(json!({})))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        state
            .execute_federation_mailbox(delayed, None)
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    let path = resolve_coordination_persistence_path(state.persistence_path.as_path());
    let reopened = MailboxStore::open(&path).unwrap();
    let connection = reopened.connection().unwrap();
    let retained: (String, String, String, Option<String>) = connection.query_row(
        "SELECT target_session_id, canonical_digest, state, payload FROM federation_mailbox_intents
         WHERE sender_session_id=?1 AND idempotency_key='cancelled'",
        [&actor.session_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).unwrap();
    assert_eq!(retained.0, target.session_id);
    assert_eq!(retained.1.len(), 64);
    assert_eq!(retained.2, "abandoned");
    assert_eq!(retained.3, None);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM mailbox_messages", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(connection.query_row(
        "SELECT state FROM federation_mailbox_dedup WHERE sender_session_id=?1 AND idempotency_key='cancelled'",
        [&actor.session_id], |row| row.get::<_, String>(0),
    ).unwrap(), "abandoned");
}

#[tokio::test]
async fn federation_delivery_unknown_cannot_be_cancelled_or_gc_from_a_negative_lookup() {
    let state = federation_mailboxes::central_store();
    let (sender, actor) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    state
        .mailbox_store
        .connection()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER lose_home_outcome BEFORE INSERT ON mailbox_messages
         BEGIN SELECT RAISE(ABORT, 'injected unknown outcome'); END;",
        )
        .unwrap();
    let result = state
        .federation_mailbox_operation::<Value>(
            &sender,
            FederationMailboxOperation::Send {
                target: target.session_id,
                message: "possibly sent".to_owned(),
                key: "uncertain".to_owned(),
                topic: None,
                state_stamp: None,
            },
        )
        .await;
    assert!(result.is_err() || result.as_ref().unwrap().get("messageId").is_none());
    assert_eq!(
        intent_api(&state, &sender, "/uncertain/cancel", Some(json!({})))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        intent_api(&state, &sender, "/uncertain/gc", Some(json!({})))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let connection = state.mailbox_store.connection().unwrap();
    let retained: (String, Option<String>) = connection.query_row(
        "SELECT state, payload FROM federation_mailbox_intents WHERE sender_session_id=?1 AND idempotency_key='uncertain'",
        [&actor.session_id], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(retained.0, "appendOutcomeUnknown");
    assert!(retained.1.is_some());
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM mailbox_messages", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn federation_delivery_committed_payload_gc_keeps_exact_retry_outcome_after_reopen() {
    let state = federation_mailboxes::central_store();
    let (sender, actor) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    let operation = || FederationMailboxOperation::Send {
        target: target.session_id.clone(),
        message: "exact retry body".to_owned(),
        key: "committed-gc".to_owned(),
        topic: None,
        state_stamp: None,
    };
    let original: MailboxAppendReceipt = state
        .federation_mailbox_operation(&sender, operation())
        .await
        .unwrap();
    assert_eq!(
        intent_api(&state, &sender, "/committed-gc/gc", Some(json!({})))
            .await
            .0,
        StatusCode::OK
    );
    let path = resolve_coordination_persistence_path(state.persistence_path.as_path());
    let reopened = AppState {
        mailbox_store: Arc::new(MailboxStore::open(&path).unwrap()),
        ..state.clone()
    };
    let retry: MailboxAppendReceipt = reopened
        .federation_mailbox_operation(&sender, operation())
        .await
        .unwrap();
    assert_eq!(retry.message_id, original.message_id);
    assert_eq!(retry.sequence, original.sequence);
    assert!(retry.duplicate);
    let connection = reopened.mailbox_store.connection().unwrap();
    let row: (String, Option<String>, String) = connection
        .query_row(
            "SELECT state, payload, canonical_digest FROM federation_mailbox_intents
         WHERE sender_session_id=?1 AND idempotency_key='committed-gc'",
            [&actor.session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(row.0, "homeCommitted");
    assert_eq!(row.1, None);
    assert_eq!(row.2.len(), 64);
}

#[tokio::test]
async fn federation_delivery_retry_never_retargets_a_recreated_recipient_alias() {
    let state = federation_mailboxes::central_store();
    let (sender, actor) = federation_mailboxes::local_participant(&state, "Sender");
    let (old_id, old_target) = federation_mailboxes::local_participant(&state, "Recipient");
    assert_eq!(
        intent_api(
            &state,
            &sender,
            "",
            Some(queued_body("Recipient", "captured"))
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    state.kill_session(&old_id).unwrap();
    state.inner.lock().unwrap().next_session_number =
        old_id.trim_start_matches("session-").parse().unwrap();
    let (new_id, new_target) = federation_mailboxes::local_participant(&state, "Recipient");
    assert_eq!(new_id, old_id);
    assert_ne!(new_target.session_id, old_target.session_id);
    assert_eq!(
        intent_api(&state, &sender, "/captured/retry", Some(json!({})))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let connection = state.mailbox_store.connection().unwrap();
    assert_eq!(connection.query_row(
        "SELECT target_session_id FROM federation_mailbox_intents WHERE sender_session_id=?1 AND idempotency_key='captured'",
        [&actor.session_id], |row| row.get::<_, String>(0),
    ).unwrap(), old_target.session_id);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM mailbox_messages", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn federation_delivery_sender_retirement_permanently_fences_only_old_creation_namespace() {
    let state = federation_mailboxes::central_store();
    let (sender, old_actor) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    assert_eq!(
        intent_api(
            &state,
            &sender,
            "",
            Some(queued_body(&target.session_id, "reuse"))
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    let delayed = federation_mailboxes::request(
        &state,
        &sender,
        FederationMailboxOperation::Send {
            target: target.session_id.clone(),
            message: "exact retry body".to_owned(),
            key: "reuse".to_owned(),
            topic: None,
            state_stamp: None,
        },
    );
    state.kill_session(&sender).unwrap();
    state.inner.lock().unwrap().next_session_number =
        sender.trim_start_matches("session-").parse().unwrap();
    let (new_sender, new_actor) = federation_mailboxes::local_participant(&state, "Sender");
    assert_eq!(new_sender, sender);
    assert_ne!(new_actor.session_id, old_actor.session_id);
    assert_eq!(
        state
            .execute_federation_mailbox(delayed, None)
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    let fresh: MailboxAppendReceipt = state
        .federation_mailbox_operation(
            &new_sender,
            FederationMailboxOperation::Send {
                target: target.session_id,
                message: "fresh namespace".to_owned(),
                key: "reuse".to_owned(),
                topic: None,
                state_stamp: None,
            },
        )
        .await
        .unwrap();
    assert!(!fresh.duplicate);
    let connection = state.mailbox_store.connection().unwrap();
    assert_eq!(connection.query_row(
        "SELECT COUNT(*) FROM federation_mailbox_retired_namespaces WHERE sender_session_id=?1",
        [&old_actor.session_id], |row| row.get::<_, u64>(0),
    ).unwrap(), 1);
    assert_eq!(connection.query_row(
        "SELECT COUNT(*) FROM federation_mailbox_retired_namespaces WHERE sender_session_id=?1",
        [&new_actor.session_id], |row| row.get::<_, u64>(0),
    ).unwrap(), 0);
}

#[tokio::test]
async fn federation_delivery_pending_status_remains_local_under_hold_and_rejects_recreated_native_owner()
 {
    let state = federation_mailboxes::central_store();
    let (sender, _) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    assert_eq!(
        intent_api(
            &state,
            &sender,
            "",
            Some(queued_body(&target.session_id, "held"))
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    state.inner.lock().unwrap().federation_durable = false;
    let (status, value) = intent_api(&state, &sender, "/held", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["state"], "localIntentDurable");
    assert_eq!(
        intent_api(&state, &sender, "/held/retry", Some(json!({})))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        state
            .mailbox_store
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM mailbox_messages", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
    // The native creation, rather than a mutable C binding, owns local status.
    let index = state
        .inner
        .lock()
        .unwrap()
        .find_session_index(&sender)
        .unwrap();
    state
        .inner
        .lock()
        .unwrap()
        .session_mut_by_index(index)
        .unwrap()
        .federation_native_creation_id = Uuid::new_v4().to_string();
    assert_eq!(
        intent_api(&state, &sender, "/held", None).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn federation_delivery_authoritative_refusal_survives_gc_without_retargeting() {
    let state = federation_mailboxes::central_store();
    let (sender, actor) = federation_mailboxes::local_participant(&state, "Sender");
    let (target_id, target) = federation_mailboxes::local_participant(&state, "Target");
    assert_eq!(
        intent_api(
            &state,
            &sender,
            "",
            Some(queued_body(&target.session_id, "refused"))
        )
        .await
        .0,
        StatusCode::ACCEPTED
    );
    state.kill_session(&target_id).unwrap();
    assert_eq!(
        intent_api(&state, &sender, "/refused/retry", Some(json!({})))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let (status, value) = intent_api(&state, &sender, "/refused", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["state"], "refused");
    assert_eq!(
        intent_api(&state, &sender, "/refused/gc", Some(json!({})))
            .await
            .0,
        StatusCode::OK
    );
    let path = resolve_coordination_persistence_path(state.persistence_path.as_path());
    let reopened = MailboxStore::open(&path).unwrap();
    let connection = reopened.connection().unwrap();
    assert_eq!(connection.query_row(
        "SELECT state,payload FROM federation_mailbox_intents WHERE sender_session_id=?1 AND idempotency_key='refused'",
        [&actor.session_id],|row|Ok((row.get::<_,String>(0)?,row.get::<_,Option<String>>(1)?))).unwrap(),("refused".into(),None));
    assert_eq!(connection.query_row("SELECT state FROM federation_mailbox_dedup WHERE sender_session_id=?1 AND idempotency_key='refused'",
        [&actor.session_id],|row|row.get::<_,String>(0)).unwrap(),"refused");
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM mailbox_messages", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
}
