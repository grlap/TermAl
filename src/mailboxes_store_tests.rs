//! SQLite mailbox store tests extracted verbatim from `mailboxes.rs`.
//!
//! This child module owns store-level persistence, idempotency, writer-admission,
//! dispatch-finalization, and lifecycle-transition tests. Production mailbox API
//! behavior remains in `mailboxes.rs`; keep private access through the sibling
//! `#[path]` module rather than widening visibility for tests.

use super::*;

struct MailboxTestRoot(PathBuf);

impl MailboxTestRoot {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("termal-mailbox-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).expect("mailbox test root should exist");
        Self(path)
    }

    fn database_path(&self) -> PathBuf {
        self.0.join("termal.sqlite")
    }
}

impl Drop for MailboxTestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn test_input() -> MailboxAppendInput {
    MailboxAppendInput {
        sender_session_id: "session-sender".to_owned(),
        sender_name: "Sender".to_owned(),
        target_session_id: "session-target".to_owned(),
        target_name: "Target".to_owned(),
        body: "Durable hello".to_owned(),
        idempotency_key: "send-1".to_owned(),
        topic: Some("coordination".to_owned()),
        state_stamp: Some("rev-7".to_owned()),
    }
}

#[test]
fn mailbox_api_status_uses_typed_error_kind_instead_of_message_text() {
    let internal = mailbox_api_error(anyhow!(
        "internal database lookup reported not found and exceeds retry budget"
    ));
    assert_eq!(
        internal.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal wording must never be mistaken for a client classification"
    );

    let not_found = mailbox_api_error(mailbox_store_error(
        MailboxStoreErrorKind::NotFound,
        "mailbox not found",
    ));
    assert_eq!(not_found.status, StatusCode::NOT_FOUND);
    let conflict = mailbox_api_error(mailbox_store_error(
        MailboxStoreErrorKind::Conflict,
        "mailbox cursor conflict",
    ));
    assert_eq!(conflict.status, StatusCode::CONFLICT);
    let validation = mailbox_api_error(mailbox_store_error(
        MailboxStoreErrorKind::Validation,
        "mailbox input exceeds limit",
    ));
    assert_eq!(validation.status, StatusCode::BAD_REQUEST);
    let retryable = mailbox_api_error(mailbox_sqlite_write_error(
        "beginning mailbox acknowledgement",
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("database is locked".to_owned()),
        ),
    ));
    assert_eq!(retryable.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        retryable
            .message
            .contains("no mailbox write was committed by this operation"),
        "rollback-safe SQLite errors must carry the structural MCP replay clause"
    );
}

#[test]
fn mailbox_append_and_acknowledgement_bound_same_file_writer_contention() {
    let root = MailboxTestRoot::new();
    let path = root.database_path();
    let store = MailboxStore::open_with_write_admission_timeout(&path, Duration::ZERO)
        .expect("mailbox store should open");
    let coordination_writer_lock = sqlite_state_write_lock(&path);
    assert!(
        Arc::ptr_eq(&coordination_writer_lock, &store.write_lock),
        "writers targeting the same coordination file must share one admission lock"
    );

    let coordination_writer_guard = lock_sqlite_state_writer(&coordination_writer_lock);
    let append_err = store
        .append(&test_input())
        .expect_err("append must fail within its admission deadline");
    assert!(
        append_err
            .downcast_ref::<MailboxStoreError>()
            .is_some_and(|err| err.kind == MailboxStoreErrorKind::Retryable),
        "writer admission exhaustion must be a typed retryable error: {append_err:#}"
    );
    assert!(
        append_err
            .to_string()
            .contains("no mailbox write was committed"),
        "pre-transaction rejection must make the safe retry contract explicit: {append_err:#}"
    );
    drop(coordination_writer_guard);
    let receipt = store
        .append(&test_input())
        .expect("append should succeed after writer release");

    let coordination_writer_guard = lock_sqlite_state_writer(&coordination_writer_lock);
    let acknowledge_err = store
        .acknowledge("session-target", &receipt.mailbox_id, 0, 1)
        .expect_err("acknowledgement must fail within its admission deadline");
    assert!(
        acknowledge_err
            .downcast_ref::<MailboxStoreError>()
            .is_some_and(|err| err.kind == MailboxStoreErrorKind::Retryable),
        "acknowledgement admission exhaustion must be retryable: {acknowledge_err:#}"
    );
    drop(coordination_writer_guard);
    let summary = store
        .acknowledge("session-target", &receipt.mailbox_id, 0, 1)
        .expect("acknowledgement should succeed after writer release");
    assert_eq!(summary.unread_count, 0);
}

#[test]
fn sqlite_state_writer_admission_serves_request_and_internal_tickets_in_fifo_order() {
    let admission = Arc::new(SqliteStateWriterAdmission::default());
    let first_guard = lock_sqlite_state_writer(&admission);
    let issued_with_holder = sqlite_state_writer_issued_tickets(&admission);

    let request_admission = admission.clone();
    let (request_acquired_tx, request_acquired_rx) = mpsc::channel();
    let (release_request_tx, release_request_rx) = mpsc::channel();
    let request_thread = std::thread::spawn(move || {
        let _guard = lock_sqlite_state_writer_for(
            &request_admission,
            Duration::from_secs(2),
        )
        .expect("queued request writer should acquire before its diagnostic deadline");
        request_acquired_tx
            .send(())
            .expect("request acquisition observer should remain connected");
        release_request_rx
            .recv()
            .expect("request writer should be released");
    });
    wait_for_sqlite_state_writer_issued_tickets(
        &admission,
        issued_with_holder + 1,
    );

    let internal_admission = admission.clone();
    let (internal_acquired_tx, internal_acquired_rx) = mpsc::channel();
    let internal_thread = std::thread::spawn(move || {
        let _guard = lock_sqlite_state_writer(&internal_admission);
        internal_acquired_tx
            .send(())
            .expect("internal acquisition observer should remain connected");
    });
    wait_for_sqlite_state_writer_issued_tickets(
        &admission,
        issued_with_holder + 2,
    );

    drop(first_guard);
    request_acquired_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("the first queued request ticket should acquire first");
    assert!(
        matches!(
            internal_acquired_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ),
        "the later internal ticket must not overtake the request ticket"
    );
    release_request_tx
        .send(())
        .expect("request writer should still be waiting for release");
    internal_acquired_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("internal ticket should acquire after request release");

    request_thread
        .join()
        .expect("request writer thread should join");
    internal_thread
        .join()
        .expect("internal writer thread should join");
}

#[test]
fn sqlite_state_writer_admission_skips_a_timed_out_head_ticket() {
    let admission = Arc::new(SqliteStateWriterAdmission::default());
    let first_guard = lock_sqlite_state_writer(&admission);
    let issued_with_holder = sqlite_state_writer_issued_tickets(&admission);

    assert!(
        lock_sqlite_state_writer_for(&admission, Duration::ZERO).is_none(),
        "a queued zero-deadline ticket should cancel"
    );

    let next_admission = admission.clone();
    let (next_acquired_tx, next_acquired_rx) = mpsc::channel();
    let next_thread = std::thread::spawn(move || {
        let _guard = lock_sqlite_state_writer(&next_admission);
        next_acquired_tx
            .send(())
            .expect("next-ticket observer should remain connected");
    });
    wait_for_sqlite_state_writer_issued_tickets(
        &admission,
        issued_with_holder + 2,
    );

    // The canceled ticket becomes queue head only when this holder drops.
    // Advancement must skip it immediately so the next live ticket cannot
    // stall behind a waiter that has already returned a 503.
    drop(first_guard);
    next_acquired_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("queue advancement should skip the canceled head ticket");
    next_thread
        .join()
        .expect("next writer thread should join");
}

#[test]
fn internal_lifecycle_write_waits_through_request_admission_saturation() {
    let root = MailboxTestRoot::new();
    let path = root.database_path();
    let store = Arc::new(
        MailboxStore::open_with_write_admission_timeout(&path, Duration::ZERO)
            .expect("mailbox store should open"),
    );
    let receipt = store.append(&test_input()).expect("append should succeed");
    let state_writer_lock = sqlite_state_write_lock(&path);
    let state_writer_guard = lock_sqlite_state_writer(&state_writer_lock);

    let lifecycle_store = store.clone();
    let message_id = receipt.message_id.clone();
    let lifecycle_thread = std::thread::spawn(move || {
        lifecycle_store.record_initial_dispatch_outcome(
            &message_id,
            "queuedBehindActiveTurn",
        )
    });
    store.wait_for_internal_writer_waiter();

    let mut request_input = test_input();
    request_input.idempotency_key = "request-during-saturation".to_owned();
    request_input.body = "request during saturation".to_owned();
    let request_error = store
        .append(&request_input)
        .expect_err("request writer should retain its bounded admission contract");
    assert!(
        request_error
            .downcast_ref::<MailboxStoreError>()
            .is_some_and(|err| err.kind == MailboxStoreErrorKind::Retryable)
    );

    drop(state_writer_guard);
    assert_eq!(
        lifecycle_thread
            .join()
            .expect("lifecycle writer thread should join")
            .expect("lifecycle writer should land after admission releases"),
        MailboxDispatchOutcomeRecord::Recorded {
            state_advanced: true
        }
    );
    assert_eq!(
        store
            .read_message("session-target", &receipt.message_id)
            .expect("message should read")
            .notification_state,
        "queuedBehindActiveTurn"
    );
}

#[test]
fn append_retry_after_reopen_returns_original_durable_receipt() {
    let root = MailboxTestRoot::new();
    let path = root.database_path();

    let first = {
        let store = MailboxStore::open(&path).expect("mailbox store should open");
        store.append(&test_input()).expect("append should succeed")
    };
    assert!(!first.duplicate);
    assert_eq!(first.notification_disposition, "durableButNotWoken");

    let store = MailboxStore::open(&path).expect("mailbox store should reopen");
    store
        .acknowledge("session-target", &first.mailbox_id, 0, 1)
        .expect("target cursor should advance before retry");
    let duplicate = store
        .append(&test_input())
        .expect("idempotent retry should succeed");
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.mailbox_id, first.mailbox_id);
    assert_eq!(duplicate.message_id, first.message_id);
    assert_eq!(duplicate.sequence, first.sequence);
    assert_eq!(
        duplicate.unread_depth, first.unread_depth,
        "duplicate must return the original receipt, not recompute depth from the current cursor"
    );
    assert_eq!(duplicate.notification_disposition, "durableButNotWoken");
    assert_eq!(
        store
            .read_range("session-target", &first.mailbox_id, 0, 20)
            .expect("messages should read")
            .len(),
        1
    );
}

#[test]
fn duplicate_receipt_preserves_dispatch_outcome_while_notification_state_advances() {
    let root = MailboxTestRoot::new();
    let store =
        MailboxStore::open(&root.database_path()).expect("mailbox store should open");
    let committed = store.append(&test_input()).expect("append should succeed");
    assert_eq!(
        store
            .read_message("session-target", &committed.message_id)
            .expect("committed notification state should read")
            .notification_state,
        "durableButNotWoken"
    );

    assert_eq!(
        store
        .record_initial_dispatch_outcome(&committed.message_id, "queuedBehindActiveTurn")
            .expect("initial dispatch outcome should persist"),
        MailboxDispatchOutcomeRecord::Recorded {
            state_advanced: true
        }
    );
    assert_eq!(
        store
            .read_message("session-target", &committed.message_id)
            .expect("queued notification state should read")
            .notification_state,
        "queuedBehindActiveTurn"
    );

    assert_eq!(
        store
        .mark_notifications_delivered_through(
            "session-target",
            &committed.mailbox_id,
            committed.sequence,
        )
            .expect("notification state should advance after runtime acceptance"),
        1
    );
    assert_eq!(
        store
            .read_message("session-target", &committed.message_id)
            .expect("delivered notification state should read")
            .notification_state,
        "deliveredToIdleSession"
    );

    let duplicate = store
        .append(&test_input())
        .expect("idempotent retry should succeed");
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.message_id, committed.message_id);
    assert_eq!(duplicate.sequence, committed.sequence);
    assert_eq!(duplicate.unread_depth, committed.unread_depth);
    assert_eq!(
        duplicate.notification_disposition, "queuedBehindActiveTurn",
        "duplicate receipts must retain the original dispatch outcome rather than the evolving message state"
    );
}

#[test]
fn concurrent_duplicate_waits_for_the_original_dispatch_outcome() {
    let root = MailboxTestRoot::new();
    let store = Arc::new(
        MailboxStore::open(&root.database_path()).expect("mailbox store should open"),
    );
    let committed = store.append(&test_input()).expect("append should succeed");
    let duplicate_store = store.clone();
    let duplicate_thread =
        std::thread::spawn(move || duplicate_store.append(&test_input()));

    store.wait_for_dispatch_outcome_waiter(&committed.message_id);
    let mut unrelated_input = test_input();
    unrelated_input.idempotency_key = "unrelated-while-duplicate-waits".to_owned();
    unrelated_input.body = "unrelated message".to_owned();
    let unrelated = store
        .append(&unrelated_input)
        .expect("a parked duplicate must not retain the SQLite writer boundary");
    assert_eq!(unrelated.sequence, committed.sequence + 1);
    store
        .record_initial_dispatch_outcome(
            &unrelated.message_id,
            "durableButNotWoken",
        )
        .expect("unrelated dispatch outcome should finalize");

    assert_eq!(
        store
        .record_initial_dispatch_outcome(
            &committed.message_id,
            "queuedBehindActiveTurn",
        )
            .expect("initial dispatch outcome should persist"),
        MailboxDispatchOutcomeRecord::Recorded {
            state_advanced: true
        }
    );

    let duplicate = duplicate_thread
        .join()
        .expect("duplicate append thread should join")
        .expect("duplicate append should succeed");
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.message_id, committed.message_id);
    assert_eq!(
        duplicate.notification_disposition, "queuedBehindActiveTurn",
        "an in-flight duplicate must not observe the provisional commit state"
    );
}

#[test]
fn duplicate_finalization_wait_returns_retryable_error_at_admission_deadline() {
    let root = MailboxTestRoot::new();
    let store = MailboxStore::open_with_write_admission_timeout(
        &root.database_path(),
        Duration::from_millis(20),
    )
    .expect("mailbox store should open");
    let committed = store.append(&test_input()).expect("append should succeed");

    let error = store
        .append(&test_input())
        .expect_err("an in-flight duplicate must not wait forever");
    assert!(
        error
            .downcast_ref::<MailboxStoreError>()
            .is_some_and(|error| error.kind == MailboxStoreErrorKind::Retryable),
        "duplicate finalization deadline should return the typed retryable error: {error:#}"
    );
    assert!(
        error.to_string().contains("still finalizing"),
        "duplicate error should identify the pending receipt: {error:#}"
    );
    assert!(
        error.to_string().contains(
            "the original mailbox append is durable and replaying the same idempotency key is safe"
        ),
        "duplicate finalization must carry the exact truthful bridge replay clause: {error:#}"
    );

    drop(committed);
    let recovered = store
        .append(&test_input())
        .expect("guard drop should release a later same-key retry");
    assert!(recovered.duplicate);
    assert_eq!(
        recovered.notification_disposition, "durableButNotWoken",
        "an abandoned finalizer must retain the conservative durable receipt"
    );
}

#[test]
fn dropping_dispatch_finalization_guard_releases_waiters_with_durable_fallback() {
    let root = MailboxTestRoot::new();
    let store = Arc::new(
        MailboxStore::open(&root.database_path()).expect("mailbox store should open"),
    );
    let MailboxAppendResult {
        receipt: committed,
        finalization,
    } = store.append(&test_input()).expect("append should succeed");
    let finalization = finalization.expect("fresh append should own finalization");
    let duplicate_store = store.clone();
    let duplicate_thread =
        std::thread::spawn(move || duplicate_store.append(&test_input()));

    store.wait_for_dispatch_outcome_waiter(&committed.message_id);
    drop(finalization);

    let duplicate = duplicate_thread
        .join()
        .expect("duplicate append thread should join")
        .expect("duplicate append should use the conservative fallback");
    assert!(duplicate.duplicate);
    assert_eq!(
        duplicate.notification_disposition, "durableButNotWoken",
        "guard unwinding must release same-process waiters without fabricating delivery"
    );
}

#[test]
fn initial_dispatch_outcome_cannot_regress_delivered_notification_state() {
    let root = MailboxTestRoot::new();
    let store =
        MailboxStore::open(&root.database_path()).expect("mailbox store should open");
    let committed = store.append(&test_input()).expect("append should succeed");

    assert_eq!(
        store
        .mark_notifications_delivered_through(
            "session-target",
            &committed.mailbox_id,
            committed.sequence,
        )
            .expect("runtime acceptance should mark the notification delivered"),
        1
    );
    assert_eq!(
        store
        .record_initial_dispatch_outcome(
            &committed.message_id,
            "queuedBehindActiveTurn",
        )
            .expect("initial dispatch outcome should persist after runtime acceptance"),
        MailboxDispatchOutcomeRecord::Recorded {
            state_advanced: false
        },
        "delivered state must reject the stale initial transition"
    );

    let message = store
        .read_message("session-target", &committed.message_id)
        .expect("message should read");
    assert_eq!(message.notification_state, "deliveredToIdleSession");
    let duplicate = store
        .append(&test_input())
        .expect("duplicate receipt should read");
    assert_eq!(
        duplicate.notification_disposition, "queuedBehindActiveTurn",
        "immutable receipt outcome remains distinct from delivered lifecycle state"
    );
}

#[test]
fn initial_dispatch_outcome_is_immutable_after_finalization() {
    let root = MailboxTestRoot::new();
    let store =
        MailboxStore::open(&root.database_path()).expect("mailbox store should open");
    let committed = store.append(&test_input()).expect("append should succeed");

    assert_eq!(
        store
            .record_initial_dispatch_outcome(
                &committed.message_id,
                "queuedBehindActiveTurn",
            )
            .expect("first dispatch finalization should persist"),
        MailboxDispatchOutcomeRecord::Recorded {
            state_advanced: true
        }
    );
    assert_eq!(
        store
            .record_initial_dispatch_outcome(
                &committed.message_id,
                "deliveredToIdleSession",
            )
            .expect("duplicate dispatch finalization should be a no-op"),
        MailboxDispatchOutcomeRecord::AlreadyFinalized {
            dispatch_outcome: "queuedBehindActiveTurn".to_owned()
        }
    );

    let duplicate = store
        .append(&test_input())
        .expect("duplicate receipt should read");
    assert_eq!(
        duplicate.notification_disposition, "queuedBehindActiveTurn",
        "a later finalizer must not rewrite the immutable receipt outcome"
    );
    assert_eq!(
        store
            .read_message("session-target", &committed.message_id)
            .expect("message should read")
            .notification_state,
        "queuedBehindActiveTurn",
        "a later finalizer must not advance mutable state using a rejected outcome"
    );
}

#[test]
fn recovery_cannot_regress_delivered_notification_state() {
    let root = MailboxTestRoot::new();
    let store =
        MailboxStore::open(&root.database_path()).expect("mailbox store should open");
    let committed = store.append(&test_input()).expect("append should succeed");
    assert_eq!(
        store
        .record_initial_dispatch_outcome(
            &committed.message_id,
            "deliveredToIdleSession",
        )
            .expect("delivered dispatch outcome should persist"),
        MailboxDispatchOutcomeRecord::Recorded {
            state_advanced: true
        }
    );
    assert_eq!(
        store
        .mark_notifications_recovered_through(
            "session-target",
            &committed.mailbox_id,
            committed.sequence,
        )
            .expect("recovery bookkeeping should succeed"),
        0,
        "recovery must be a guarded no-op after delivery"
    );

    assert_eq!(
        store
            .read_message("session-target", &committed.message_id)
            .expect("message should read")
            .notification_state,
        "deliveredToIdleSession"
    );
}

#[test]
fn idempotent_retry_ignores_mutable_participant_display_names() {
    let root = MailboxTestRoot::new();
    let path = root.database_path();
    let store = MailboxStore::open(&path).expect("mailbox store should open");
    let first = store.append(&test_input()).expect("append should succeed");
    store
        .record_initial_dispatch_outcome(
            &first.message_id,
            "durableButNotWoken",
        )
        .expect("original receipt should finalize before retry");

    let mut renamed = test_input();
    renamed.sender_name = "Renamed Sender".to_owned();
    renamed.target_name = "Renamed Target".to_owned();
    let duplicate = store
        .append(&renamed)
        .expect("renaming either participant must not change message intent");
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.message_id, first.message_id);

    let stored = store
        .read_message("session-target", &first.message_id)
        .expect("original durable message should read");
    assert_eq!(stored.sender_name, "Sender");
    assert_eq!(stored.target_name, "Target");
}

#[test]
fn reused_idempotency_key_with_different_intent_is_rejected() {
    let root = MailboxTestRoot::new();
    let path = root.database_path();
    let store = MailboxStore::open(&path).expect("mailbox store should open");
    store.append(&test_input()).expect("append should succeed");
    let mut conflicting = test_input();
    conflicting.body = "Different message".to_owned();
    let error = store
        .append(&conflicting)
        .expect_err("conflicting retry should fail");
    assert!(error.to_string().contains("different mailbox message"));
}

#[test]
fn acknowledgement_is_forward_only_compare_and_swap() {
    let root = MailboxTestRoot::new();
    let path = root.database_path();
    let store = MailboxStore::open(&path).expect("mailbox store should open");
    let receipt = store.append(&test_input()).expect("append should succeed");

    let summary = store
        .acknowledge("session-target", &receipt.mailbox_id, 0, 1)
        .expect("matching cursor should advance");
    assert_eq!(summary.unread_count, 0);
    assert_eq!(summary.latest_sequence, 1);
    assert_eq!(
        summary
            .participants
            .iter()
            .find(|participant| participant.session_id == "session-target")
            .expect("target participant should be present")
            .processed_through,
        1,
        "the response prepared inside the transaction must reflect the committed cursor"
    );
    let duplicate = store
        .acknowledge("session-target", &receipt.mailbox_id, 0, 1)
        .expect("an identical retry whose outcome is already satisfied should succeed");
    assert_eq!(duplicate, summary);

    let mut second_input = test_input();
    second_input.idempotency_key = "second-message".to_owned();
    store
        .append(&second_input)
        .expect("second message should append");
    let error = store
        .acknowledge("session-target", &receipt.mailbox_id, 0, 2)
        .expect_err("a stale cursor that requests new progress should conflict");
    assert!(error.to_string().contains("conflict"));
}

#[test]
fn unread_count_includes_only_inbound_messages_above_the_cursor() {
    let root = MailboxTestRoot::new();
    let path = root.database_path();
    let store = MailboxStore::open(&path).expect("mailbox store should open");
    let first = store.append(&test_input()).expect("append should succeed");

    let mut reply = test_input();
    reply.sender_session_id = "session-target".to_owned();
    reply.sender_name = "Target".to_owned();
    reply.target_session_id = "session-sender".to_owned();
    reply.target_name = "Sender".to_owned();
    reply.idempotency_key = "reply-1".to_owned();
    reply.body = "Outbound from the original target".to_owned();
    store.append(&reply).expect("reply should append");

    let target_summary = store
        .list_for_session("session-target")
        .expect("target summary should read")
        .into_iter()
        .find(|summary| summary.id == first.mailbox_id)
        .expect("target mailbox should exist");
    assert_eq!(
        target_summary.unread_count, 1,
        "the target's own outbound reply must not inflate inbound unread"
    );
    assert_eq!(
        store
            .unread_wakeups_for_session("session-target")
            .expect("wake state should read")[0]
            .unread_count,
        1
    );
}

#[test]
fn mailbox_append_caps_body_and_optional_metadata() {
    let mutations: [fn(&mut MailboxAppendInput); 3] = [
        |input: &mut MailboxAppendInput| {
            input.body = "x".repeat(MAX_MAILBOX_BODY_BYTES + 1);
        },
        |input: &mut MailboxAppendInput| {
            input.topic = Some("x".repeat(MAX_MAILBOX_METADATA_BYTES + 1));
        },
        |input: &mut MailboxAppendInput| {
            input.state_stamp = Some("x".repeat(MAX_MAILBOX_METADATA_BYTES + 1));
        },
    ];
    for mutate in mutations {
        let mut input = test_input();
        mutate(&mut input);
        assert!(
            validate_mailbox_append_input(&input)
                .expect_err("oversized mailbox input should fail")
                .to_string()
                .contains("exceeds")
        );
    }
}

#[test]
fn concurrent_appends_allocate_one_dense_mailbox_sequence() {
    let root = MailboxTestRoot::new();
    let path = root.database_path();
    let store = Arc::new(MailboxStore::open(&path).expect("mailbox store should open"));
    let barrier = Arc::new(std::sync::Barrier::new(5));
    let mut handles = Vec::new();
    for index in 0..4 {
        let store = store.clone();
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move || {
            let mut input = test_input();
            input.idempotency_key = format!("send-{index}");
            input.body = format!("message {index}");
            barrier.wait();
            store.append(&input).expect("concurrent append should succeed")
        }));
    }
    barrier.wait();
    let mut receipts = handles
        .into_iter()
        .map(|handle| handle.join().expect("append thread should join"))
        .collect::<Vec<_>>();
    receipts.sort_by_key(|receipt| receipt.sequence);
    assert_eq!(
        receipts
            .iter()
            .map(|receipt| receipt.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    assert!(
        receipts
            .windows(2)
            .all(|pair| pair[0].mailbox_id == pair[1].mailbox_id)
    );
}

#[test]
fn appending_again_does_not_resurrect_a_departed_participant() {
    let root = MailboxTestRoot::new();
    let path = root.database_path();
    let store = MailboxStore::open(&path).expect("mailbox store should open");
    let first = store.append(&test_input()).expect("append should succeed");
    store
        .mark_session_left("session-target")
        .expect("participant should be marked left");

    let mut second_input = test_input();
    second_input.idempotency_key = "send-2".to_owned();
    second_input.body = "second body".to_owned();
    let error = store
        .append(&second_input)
        .expect_err("append to a departed participant should be rejected");
    assert!(error.to_string().contains("departed mailbox participant"));

    assert!(
        store
            .list_for_session("session-target")
            .expect("departed participant list should read")
            .is_empty(),
        "append upsert must not clear a deletion's left marker"
    );
    let sender_summary = store
        .list_for_session("session-sender")
        .expect("sender mailbox list should read")
        .into_iter()
        .find(|summary| summary.id == first.mailbox_id)
        .expect("sender should retain mailbox history");
    assert!(sender_summary
        .participants
        .iter()
        .find(|participant| participant.session_id == "session-target")
        .expect("target snapshot should remain")
        .left_at
        .is_some());
}
