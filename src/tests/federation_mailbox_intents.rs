// Owns sender-intent durability, cross-target key scope, and aggregate bounds.
// Does not simulate successful transport or serve mailbox reads from intents.
// New integration coverage over the existing mailbox facade and real SQLite.
use super::*;

fn send(target: &str, key: &str, body: &str) -> FederationMailboxOperation {
    FederationMailboxOperation::Send {
        target: target.to_owned(),
        message: body.to_owned(),
        key: key.to_owned(),
        topic: None,
        state_stamp: None,
    }
}

fn interrupt_central_append(state: &AppState) {
    state
        .mailbox_store
        .connection()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER interrupt_central_append BEFORE INSERT ON mailbox_messages
         BEGIN SELECT RAISE(ABORT, 'injected central write failure'); END;",
        )
        .unwrap();
}

fn assert_not_committed(result: Result<Value, ApiError>) {
    match result {
        Ok(value) => {
            assert!(
                value.get("messageId").is_none(),
                "pending is not an append receipt: {value}"
            );
            assert!(
                value.get("sequence").is_none(),
                "pending has no canonical sequence: {value}"
            );
            assert!(
                matches!(
                    value["state"].as_str(),
                    Some("localIntentDurable" | "appendOutcomeUnknown")
                ),
                "a queued result must expose its distinct intent state: {value}"
            );
        }
        Err(error) => assert!(
            error.status.is_server_error(),
            "unexpected refusal: {:?}",
            error.status
        ),
    }
}

#[tokio::test]
async fn federation_delivery_intent_home_commit_retains_key_digest_target_and_original_receipt() {
    let state = federation_mailboxes::central_store();
    let (sender, actor) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    let first: MailboxAppendReceipt = state
        .federation_mailbox_operation(
            &sender,
            send(&target.session_id, "retained", "original body"),
        )
        .await
        .unwrap();
    let path = resolve_coordination_persistence_path(state.persistence_path.as_path());
    let reopened = MailboxStore::open(&path).unwrap();
    let connection = reopened.connection().unwrap();
    let row: (String, String, String, String, String, u64) = connection.query_row(
        "SELECT sender_session_id, target_session_id, canonical_digest, state, message_id, sequence
         FROM federation_mailbox_intents WHERE sender_session_id=?1 AND idempotency_key='retained'",
        [&actor.session_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
    ).expect("a confirmed send must retain its durable idempotency outcome across reopen");
    assert_eq!(row.0, actor.session_id);
    assert_eq!(row.1, target.session_id);
    assert_eq!(row.2.len(), 64);
    assert!(row.2.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(row.3, "homeCommitted");
    assert_eq!(row.4, first.message_id);
    assert_eq!(row.5, first.sequence);
}

#[tokio::test]
async fn federation_delivery_intent_local_durability_failure_prevents_canonical_append() {
    let state = federation_mailboxes::central_store();
    let (sender, _) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    state
        .mailbox_store
        .connection()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER fail_intent_disk_write BEFORE INSERT ON federation_mailbox_intents
         BEGIN SELECT RAISE(ABORT, 'injected local durability failure'); END;",
        )
        .expect("sender intent storage must exist before sending");
    assert!(
        state
            .federation_mailbox_operation::<Value>(
                &sender,
                send(&target.session_id, "not-durable", "must not leave sender"),
            )
            .await
            .is_err()
    );
    let connection = state.mailbox_store.connection().unwrap();
    for table in [
        "mailbox_messages",
        "federation_mailbox_dedup",
        "federation_mailbox_wake_obligations",
    ] {
        let count: u64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            count, 0,
            "local intent durability failure cannot create {table}"
        );
    }
}

#[tokio::test]
async fn federation_delivery_intent_unknown_append_is_durable_and_never_mailbox_content() {
    let state = federation_mailboxes::central_store();
    let (sender, actor) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    interrupt_central_append(&state);
    assert_not_committed(
        state
            .federation_mailbox_operation::<Value>(
                &sender,
                send(&target.session_id, "unknown", "retry-only body"),
            )
            .await,
    );
    state
        .mailbox_store
        .connection()
        .unwrap()
        .execute_batch("DROP TRIGGER interrupt_central_append")
        .unwrap();
    let path = resolve_coordination_persistence_path(state.persistence_path.as_path());
    let reopened = MailboxStore::open(&path).unwrap();
    let connection = reopened.connection().unwrap();
    let row: (String, Option<String>, Option<u64>) = connection
        .query_row(
            "SELECT state, message_id, sequence FROM federation_mailbox_intents
         WHERE sender_session_id=?1 AND idempotency_key='unknown'",
            [&actor.session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("an attempted but unconfirmed append must survive reopen as an intent");
    assert_eq!(row.0, "appendOutcomeUnknown");
    assert_eq!(row.1, None);
    assert_eq!(row.2, None);
    drop(connection);
    assert!(
        reopened
            .list_for_session(&actor.session_id)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn federation_delivery_intent_key_conflicts_across_targets_before_another_append() {
    let state = federation_mailboxes::central_store();
    let (sender, actor) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, left) = federation_mailboxes::local_participant(&state, "Left");
    let (_, right) = federation_mailboxes::local_participant(&state, "Right");
    let first: MailboxAppendReceipt = state
        .federation_mailbox_operation(&sender, send(&left.session_id, "one-key", "same body"))
        .await
        .unwrap();
    for operation in [
        send(&right.session_id, "one-key", "same body"),
        send(&left.session_id, "one-key", "changed body"),
        FederationMailboxOperation::Send {
            target: left.session_id.clone(),
            message: "same body".to_owned(),
            key: "one-key".to_owned(),
            topic: Some(String::new()),
            state_stamp: None,
        },
    ] {
        assert_eq!(
            state
                .federation_mailbox_operation::<Value>(&sender, operation)
                .await
                .unwrap_err()
                .status,
            StatusCode::CONFLICT
        );
    }
    let duplicate: MailboxAppendReceipt = state
        .federation_mailbox_operation(&sender, send(&left.session_id, "one-key", "same body"))
        .await
        .unwrap();
    assert_eq!(duplicate.message_id, first.message_id);
    assert!(duplicate.duplicate);
    let connection = state.mailbox_store.connection().unwrap();
    let intents: u64 = connection
        .query_row(
            "SELECT COUNT(*) FROM federation_mailbox_intents WHERE sender_session_id=?1",
            [&actor.session_id],
            |row| row.get(0),
        )
        .expect("all targets must share one sender intent key namespace");
    assert_eq!(intents, 1);
}

#[tokio::test]
async fn federation_delivery_intent_pending_count_is_global_across_two_targets() {
    let state = federation_mailboxes::central_store();
    let (sender, actor) = federation_mailboxes::local_participant(&state, "Sender");
    let (other_sender, _) = federation_mailboxes::local_participant(&state, "Other sender");
    let (_, left) = federation_mailboxes::local_participant(&state, "Left");
    let (_, right) = federation_mailboxes::local_participant(&state, "Right");
    interrupt_central_append(&state);
    for index in 0..1000 {
        let local_sender = if index % 2 == 0 {
            &sender
        } else {
            &other_sender
        };
        let target = if index % 2 == 0 {
            &left.session_id
        } else {
            &right.session_id
        };
        assert_not_committed(
            state
                .federation_mailbox_operation::<Value>(
                    local_sender,
                    send(target, &format!("queued-{index}"), "small pending body"),
                )
                .await,
        );
        if index == 0 {
            let count: u64 = state
                .mailbox_store
                .connection()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM federation_mailbox_intents WHERE sender_session_id=?1
                 AND state IN ('localIntentDurable','appendOutcomeUnknown')",
                    [&actor.session_id],
                    |row| row.get(0),
                )
                .expect("local durability must precede even the first failed home append");
            assert_eq!(count, 1);
        }
    }
    let refusal = state
        .federation_mailbox_operation::<Value>(
            &sender,
            send(&right.session_id, "over-global-bound", "refused visibly"),
        )
        .await
        .unwrap_err();
    assert_eq!(refusal.status, StatusCode::TOO_MANY_REQUESTS);
    let connection = state.mailbox_store.connection().unwrap();
    let counts: (u64, u64, u64) = connection
        .query_row(
            "SELECT COUNT(*), COUNT(DISTINCT target_session_id), COUNT(DISTINCT sender_session_id)
             FROM federation_mailbox_intents WHERE state IN ('localIntentDurable','appendOutcomeUnknown')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(counts, (1000, 2, 2));
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM mailbox_messages", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn federation_delivery_real_sqlite_full_never_reports_local_durability_or_append_success() {
    let state = federation_mailboxes::central_store();
    let (sender, _) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    {
        let connection = state.mailbox_store.connection().unwrap();
        let count: u64 = connection
            .query_row(
                "SELECT COUNT(*) FROM federation_mailbox_intents",
                [],
                |row| row.get(0),
            )
            .expect("real disk-full control requires the durable sender schema");
        assert_eq!(count, 0);
        let pages: u64 = connection
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .unwrap();
        let cap: u64 = connection
            .query_row(&format!("PRAGMA max_page_count={pages}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(cap, pages);
    }
    // A maximal allowed body must grow its empty payload table. The private
    // SQLite file cannot allocate another page; this is not a fabricated errno.
    let result = state
        .federation_mailbox_operation::<Value>(
            &sender,
            send(
                &target.session_id,
                "actual-sqlite-full",
                &"x".repeat(MAX_MAILBOX_BODY_BYTES),
            ),
        )
        .await;
    assert!(
        result.is_err(),
        "a failed local durable write cannot return a queued or committed result"
    );
    let connection = state.mailbox_store.connection().unwrap();
    for table in [
        "federation_mailbox_intents",
        "mailbox_messages",
        "federation_mailbox_dedup",
        "federation_mailbox_wake_obligations",
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
async fn federation_delivery_intent_pending_bytes_refuse_visibly_before_count_bound() {
    let state = federation_mailboxes::central_store();
    let (sender, actor) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, left) = federation_mailboxes::local_participant(&state, "Left");
    let (_, right) = federation_mailboxes::local_participant(&state, "Right");
    interrupt_central_append(&state);
    let body = "x".repeat(MAX_MAILBOX_BODY_BYTES);
    let mut refused_at = None;
    for index in 0..300 {
        let target = if index % 2 == 0 {
            &left.session_id
        } else {
            &right.session_id
        };
        let result = state
            .federation_mailbox_operation::<Value>(
                &sender,
                send(target, &format!("bytes-{index}"), &body),
            )
            .await;
        if result
            .as_ref()
            .err()
            .is_some_and(|error| error.status == StatusCode::TOO_MANY_REQUESTS)
        {
            refused_at = Some(index);
            break;
        }
        assert_not_committed(result);
        if index == 0 {
            state
                .mailbox_store
                .connection()
                .unwrap()
                .query_row(
                    "SELECT payload_bytes FROM federation_mailbox_intents
                 WHERE sender_session_id=?1 AND idempotency_key='bytes-0'",
                    [&actor.session_id],
                    |row| row.get::<_, u64>(0),
                )
                .expect("pending-byte accounting must be durable, not a process-local estimate");
        }
    }
    let refused_at =
        refused_at.expect("64 MiB must refuse before 300 maximal-body pending intents");
    assert!(refused_at > 0 && refused_at < 1000);
    let connection = state.mailbox_store.connection().unwrap();
    let row: (u64, u64, u64) = connection
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(payload_bytes),0), COUNT(DISTINCT target_session_id)
         FROM federation_mailbox_intents WHERE sender_session_id=?1
         AND state IN ('localIntentDurable','appendOutcomeUnknown')",
            [&actor.session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        row.0, refused_at as u64,
        "refusal must not silently evict old ambiguous intents"
    );
    assert!(row.1 <= 64 * 1024 * 1024);
    assert_eq!(row.2, 2);
}
