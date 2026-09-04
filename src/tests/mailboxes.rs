//! Durable neutral mailbox integration coverage.
//!
//! These tests pin commit-before-notify, metadata-only wake-up, retry
//! idempotency, and persist-worker independence without timing or subprocesses.

use super::*;

pub(super) fn mailbox_test_state() -> (AppState, String, String) {
    let base = test_app_state();
    let coordination_path = resolve_coordination_persistence_path(base.persistence_path.as_ref());
    let state = AppState {
        mailbox_store: Arc::new(
            MailboxStore::open(&coordination_path).expect("mailbox test store should open"),
        ),
        ..base
    };
    let sender_id = test_session_id(&state, Agent::Codex);
    let target_id = test_session_id(&state, Agent::Claude);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let sender_index = inner
            .find_session_index(&sender_id)
            .expect("sender should exist");
        inner.sessions[sender_index].session.name = "Sol".to_owned();
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        inner.sessions[target_index].session.name = "Fable".to_owned();
        // A busy target deterministically queues the compact wake-up prompt and
        // never starts a real agent runtime.
        inner.sessions[target_index].session.status = SessionStatus::Active;
    }
    (state, sender_id, target_id)
}

fn mailbox_send_request(target_session_id: &str) -> SendMailboxMessageRequest {
    SendMailboxMessageRequest {
        target_session_id: target_session_id.to_owned(),
        message: "The durable body must never enter the target prompt.".to_owned(),
        idempotency_key: "sol-send-1".to_owned(),
        topic: Some("architecture".to_owned()),
        state_stamp: Some("rev-9".to_owned()),
        class: Some("routine".to_owned()),
    }
}

#[test]
fn lightweight_test_state_does_not_hold_a_mailbox_database_descriptor() {
    let state = test_app_state();
    assert!(
        state.mailbox_store.connection_if_enabled().is_none(),
        "ordinary test fixtures must opt into mailbox SQLite explicitly so retained AppStates cannot exhaust the suite's fd budget"
    );
}

#[test]
fn mailbox_backend_rejects_exact_delegation_child_target_before_append() {
    let (state, sender_id, target_id) = mailbox_test_state();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        inner.sessions[target_index].session.parent_delegation_id =
            Some("delegation-child-boundary".to_owned());
    }

    let err = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect_err("delegation children must not be mailbox peers");
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert_eq!(err.message, "target must be a local root session");
    assert!(
        state
            .mailbox_store
            .list_for_session(&sender_id)
            .expect("sender mailboxes should list")
            .is_empty(),
        "backend eligibility validation must happen before durable append"
    );
}

#[tokio::test]
async fn mailbox_read_routes_reject_delegation_children_as_non_peers() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let receipt = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("root peers should establish a mailbox");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        inner.sessions[target_index].session.parent_delegation_id =
            Some("delegation-child-boundary".to_owned());
    }

    let list_err = list_mailboxes(AxumPath(target_id.clone()), State(state.clone()))
        .await
        .expect_err("delegation children must not list root-peer mailboxes");
    assert_eq!(list_err.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        list_err.message,
        "mailbox participant must be a local root session"
    );

    let read_err = read_mailbox(
        AxumPath((target_id.clone(), receipt.mailbox_id.clone())),
        State(state.clone()),
        Json(ReadMailboxRequest {
            after_sequence: 0,
            limit: 20,
        }),
    )
    .await
    .expect_err("delegation children must not read root-peer mailboxes");
    assert_eq!(read_err.status, StatusCode::BAD_REQUEST);

    let exact_err = read_mailbox_message(
        AxumPath((target_id.clone(), receipt.message_id.clone())),
        State(state.clone()),
    )
    .await
    .expect_err("delegation children must not read exact root-peer messages");
    assert_eq!(exact_err.status, StatusCode::BAD_REQUEST);

    let acknowledge_err = acknowledge_mailbox(
        AxumPath((target_id, receipt.mailbox_id)),
        State(state),
        Json(AcknowledgeMailboxRequest {
            expected_processed_through: 0,
            processed_through: receipt.sequence,
        }),
    )
    .await
    .expect_err("delegation children must not acknowledge root-peer mailboxes");
    assert_eq!(acknowledge_err.status, StatusCode::BAD_REQUEST);
}

#[test]
fn mailbox_send_commits_body_before_metadata_only_wake_and_retry_does_not_rewake() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let first = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("mailbox send should succeed");
    assert!(!first.duplicate);
    assert_eq!(first.notification_disposition, "queuedBehindActiveTurn");

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let target = inner
            .sessions
            .iter()
            .find(|record| record.session.id == target_id)
            .expect("target should exist");
        assert_eq!(target.queued_prompts.len(), 1);
        let pending = &target.queued_prompts[0].pending_prompt;
        assert!(pending.text.contains(&first.mailbox_id));
        assert!(pending.text.contains("termal_list_mailboxes"));
        assert!(pending.text.contains("termal_read_mailbox"));
        assert!(pending.text.contains("expectedProcessedThrough"));
        assert!(pending.text.contains("TERMAL_CLI"));
        assert!(pending.text.contains("TERMAL_SESSION_ID"));
        assert!(pending.text.contains("mailbox acknowledge --expected"));
        assert!(
            !pending.text.contains("durable body"),
            "wake-up prompt must contain metadata only"
        );
        let source = pending
            .source
            .as_ref()
            .expect("mailbox wake-up should carry structured source");
        assert!(source.is_mailbox());
        assert_eq!(
            source
                .mailbox
                .as_ref()
                .expect("mailbox source metadata should exist")
                .message_id,
            first.message_id
        );
    }

    let stored = state
        .mailbox_store
        .read_range(&target_id, &first.mailbox_id, 0, 20)
        .expect("durable body should be readable");
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].body,
        "The durable body must never enter the target prompt."
    );

    let mut second_request = mailbox_send_request(&target_id);
    second_request.idempotency_key = "sol-send-2".to_owned();
    second_request.message = "A second independently durable message.".to_owned();
    let second = state
        .append_mailbox_message_and_notify(&sender_id, second_request)
        .expect("second mailbox send should succeed");
    assert_eq!(second.mailbox_id, first.mailbox_id);
    assert_eq!(second.sequence, 2);
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let target = inner
            .sessions
            .iter()
            .find(|record| record.session.id == target_id)
            .expect("target should exist");
        assert_eq!(
            target.queued_prompts.len(),
            1,
            "busy receivers should retain one metadata wake-up per mailbox"
        );
        let latest_source = target.queued_prompts[0]
            .pending_prompt
            .source
            .as_ref()
            .and_then(|source| source.mailbox.as_ref())
            .expect("coalesced wake-up should retain mailbox metadata");
        assert_eq!(latest_source.message_id, second.message_id);
        assert_eq!(latest_source.unread_count, 2);
    }
    assert_eq!(
        state
            .mailbox_store
            .read_range(&target_id, &first.mailbox_id, 0, 20)
            .expect("both messages should remain durable")
            .len(),
        2
    );

    let duplicate = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("idempotent retry should return the original receipt");
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.message_id, first.message_id);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should exist");
    assert_eq!(
        target.queued_prompts.len(),
        1,
        "duplicate retry must not wake the receiver twice"
    );
}

#[test]
fn live_busy_mailbox_wake_is_visible_and_drains_at_turn_completion() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let (runtime, input_rx) = test_claude_runtime_handle("live-mailbox-wake-runtime");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        inner.sessions[target_index].runtime = SessionRuntime::Claude(runtime);
    }
    let runtime_token = RuntimeToken::Claude("live-mailbox-wake-runtime".to_owned());

    let receipt = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("live mailbox send should queue behind the active turn");
    assert_eq!(receipt.notification_disposition, "queuedBehindActiveTurn");

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let target = inner
            .sessions
            .iter()
            .find(|record| record.session.id == target_id)
            .expect("target should exist");
        assert_eq!(target.session.pending_prompts.len(), 1);
        let pending = &target.session.pending_prompts[0];
        assert!(
            pending.text.contains(&receipt.mailbox_id),
            "the metadata-only wake should be visible through the queued-card wire field"
        );
        assert!(
            pending
                .source
                .as_ref()
                .is_some_and(MessageSource::is_mailbox),
            "the visible queued card must retain mailbox provenance"
        );
    }

    state
        .finish_turn_ok_if_runtime_matches(&target_id, &runtime_token)
        .expect("finishing the active turn should drain the queued mailbox wake");
    assert!(matches!(
        input_rx.recv_timeout(Duration::from_secs(1)),
        Ok(ClaudeRuntimeCommand::Prompt(command)) if command.text.contains(&receipt.mailbox_id)
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should exist");
    assert!(
        target.session.pending_prompts.is_empty(),
        "the queued card should disappear after the wake starts"
    );
    assert_eq!(target.session.status, SessionStatus::Active);
}

#[tokio::test]
async fn normal_mailbox_interactions_reactivate_stale_live_participants() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let first = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("initial mailbox send should succeed");

    state
        .mailbox_store
        .mark_session_left(&sender_id)
        .expect("test should reproduce stale sender eviction");
    let mut second_request = mailbox_send_request(&target_id);
    second_request.idempotency_key = "sol-send-after-stale-left".to_owned();
    second_request.message = "A live sender must self-heal before this append.".to_owned();
    let second = state
        .append_mailbox_message_and_notify(&sender_id, second_request)
        .expect("ordinary send should reactivate a stale live sender");
    assert_eq!(second.sequence, first.sequence + 1);

    state
        .mailbox_store
        .mark_session_left(&target_id)
        .expect("test should reproduce stale target eviction before list");
    let Json(summaries) = list_mailboxes(AxumPath(target_id.clone()), State(state.clone()))
        .await
        .expect("ordinary list should reactivate a stale live target");
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].id, first.mailbox_id);

    state
        .mailbox_store
        .mark_session_left(&target_id)
        .expect("test should reproduce stale target eviction before read");
    let Json(messages) = read_mailbox(
        AxumPath((target_id.clone(), first.mailbox_id.clone())),
        State(state.clone()),
        Json(ReadMailboxRequest {
            after_sequence: 0,
            limit: 20,
        }),
    )
    .await
    .expect("ordinary read should reactivate a stale live target");
    assert_eq!(messages.len(), 2);

    state
        .mailbox_store
        .mark_session_left(&target_id)
        .expect("test should reproduce stale target eviction before acknowledge");
    let Json(summary) = acknowledge_mailbox(
        AxumPath((target_id.clone(), first.mailbox_id.clone())),
        State(state.clone()),
        Json(AcknowledgeMailboxRequest {
            expected_processed_through: 0,
            processed_through: second.sequence,
        }),
    )
    .await
    .expect("ordinary acknowledgement should reactivate a stale live target");
    assert_eq!(
        summary
            .participants
            .iter()
            .find(|participant| participant.session_id == target_id)
            .expect("target participant should remain present")
            .processed_through,
        second.sequence
    );
}

#[test]
fn transient_send_eligibility_failure_never_evicts_a_participant() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let first = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("initial mailbox send should succeed");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        inner.sessions[target_index].hidden = true;
    }

    let mut transient_request = mailbox_send_request(&target_id);
    transient_request.idempotency_key = "sol-transient-classification".to_owned();
    let err = state
        .append_mailbox_message_and_notify(&sender_id, transient_request)
        .expect_err("temporarily ineligible target should reject before append");
    assert_eq!(err.status, StatusCode::BAD_REQUEST);

    let sender_summary = state
        .mailbox_store
        .list_for_session(&sender_id)
        .expect("sender mailbox should remain readable")
        .into_iter()
        .find(|summary| summary.id == first.mailbox_id)
        .expect("sender should retain mailbox history");
    assert!(
        sender_summary
            .participants
            .iter()
            .find(|participant| participant.session_id == target_id)
            .expect("target participant should remain present")
            .left_at
            .is_none(),
        "a transient send-time classification failure must not mutate left_at"
    );
}

#[test]
fn deliberate_session_deletion_remains_the_mailbox_eviction_authority() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let first = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("initial mailbox send should succeed");
    state
        .kill_session(&target_id)
        .expect("deliberate session deletion should succeed");

    let err = state
        .ensure_mailbox_session_active(&target_id)
        .expect_err("deleted session must not self-heal");
    assert_eq!(err.status, StatusCode::NOT_FOUND);
    assert!(
        state
            .mailbox_store
            .list_for_session(&target_id)
            .expect("deleted participant list should read")
            .is_empty()
    );
    let sender_summary = state
        .mailbox_store
        .list_for_session(&sender_id)
        .expect("sender mailbox should remain readable")
        .into_iter()
        .find(|summary| summary.id == first.mailbox_id)
        .expect("sender should retain deleted peer history");
    assert!(
        sender_summary
            .participants
            .iter()
            .find(|participant| participant.session_id == target_id)
            .expect("deleted target snapshot should remain")
            .left_at
            .is_some(),
        "deliberate deletion must retain its durable eviction marker"
    );
}

#[test]
fn direct_mailbox_dispatch_marks_every_covered_notification_delivered() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let older = state
        .mailbox_store
        .append(&MailboxAppendInput {
            sender_session_id: sender_id.clone(),
            sender_name: "Sol".to_owned(),
            target_session_id: target_id.clone(),
            target_name: "Fable".to_owned(),
            body: "Older durable notification whose first wake was lost.".to_owned(),
            idempotency_key: "direct-delivery-older".to_owned(),
            topic: Some("delivery".to_owned()),
            state_stamp: None,
        })
        .expect("older mailbox body should commit without a wake");
    let _input_rx = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        let (runtime, input_rx) = test_claude_runtime_handle("direct-mailbox-delivery");
        let target = inner
            .session_mut_by_index(target_index)
            .expect("target should exist");
        target.session.status = SessionStatus::Idle;
        target.runtime = SessionRuntime::Claude(runtime);
        input_rx
    };

    let mut newer_request = mailbox_send_request(&target_id);
    newer_request.idempotency_key = "direct-delivery-newer".to_owned();
    newer_request.message = "Newer notification starts an idle turn.".to_owned();
    let newer = state
        .append_mailbox_message_and_notify(&sender_id, newer_request)
        .expect("newer mailbox send should dispatch directly");
    assert_eq!(newer.sequence, older.sequence + 1);
    assert_eq!(newer.notification_disposition, "deliveredToIdleSession");
    for message_id in [&older.message_id, &newer.message_id] {
        assert_eq!(
            state
                .mailbox_store
                .read_message(&target_id, message_id)
                .expect("notification state should read")
                .notification_state,
            "deliveredToIdleSession",
            "direct dispatch must mark every covered inbound row delivered"
        );
    }
    assert!(
        state
            .mailbox_store
            .unread_wakeups_for_session(&target_id)
            .expect("never-woken query should succeed")
            .is_empty(),
        "an ordinary turn must not recover an older row already covered by the direct wake"
    );
}

#[test]
fn mailbox_send_runtime_channel_failure_keeps_notification_recoverable() {
    let (state, sender_id, target_id) = mailbox_test_state();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        let (runtime, input_rx) = test_claude_runtime_handle("dropped-mailbox-runtime");
        drop(input_rx);
        let target = inner
            .session_mut_by_index(target_index)
            .expect("target should exist");
        target.session.status = SessionStatus::Idle;
        target.runtime = SessionRuntime::Claude(runtime);
    }

    let receipt = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("durable append should survive a failed runtime wake");
    assert_eq!(
        receipt.notification_disposition, "durableButNotWoken",
        "the receipt must not claim delivery when the runtime channel rejected the turn"
    );
    assert_eq!(
        state
            .mailbox_store
            .read_message(&target_id, &receipt.message_id)
            .expect("notification state should remain readable")
            .notification_state,
        "recoveredWake",
        "the failure lifecycle should immediately queue a durable recovery wake"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should exist");
    let recovered = target
        .queued_prompts
        .iter()
        .find_map(|queued| {
            queued
                .pending_prompt
                .source
                .as_ref()
                .and_then(|source| source.mailbox.as_ref())
        })
        .expect("failed delivery should remain queued for recovery");
    assert_eq!(recovered.message_id, receipt.message_id);
    drop(inner);

    let (runtime, input_rx) = test_claude_runtime_handle("accepted-fresh-recovery-runtime");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        inner.sessions[target_index].runtime = SessionRuntime::Claude(runtime);
    }
    let retry = state
        .dispatch_turn(
            &target_id,
            SendMessageRequest {
                text: "ordinary activation retries the rejected fresh wake".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .expect("a later activation should retry the rejected fresh wake");
    let retry = match retry {
        DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        _ => panic!("the restored wake must remain ahead of the later activation"),
    };
    deliver_turn_dispatch(&state, retry).expect("the replacement runtime should accept the wake");
    assert!(matches!(
        input_rx.recv_timeout(Duration::from_secs(1)),
        Ok(ClaudeRuntimeCommand::Prompt(command)) if command.text.contains(&receipt.mailbox_id)
    ));
    assert_eq!(
        state
            .mailbox_store
            .read_message(&target_id, &receipt.message_id)
            .expect("accepted retry state should remain readable")
            .notification_state,
        "deliveredToIdleSession"
    );
}

#[test]
fn idle_blocked_receiver_coalesces_repeated_mailbox_wakes() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let first = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("first mailbox wake should queue while the target is busy");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        let target = inner
            .session_mut_by_index(target_index)
            .expect("target should exist");
        target.session.status = SessionStatus::Idle;
        target.orchestrator_auto_dispatch_blocked = true;
        assert_eq!(target.queued_prompts.len(), 1);
    }

    let mut second_request = mailbox_send_request(&target_id);
    second_request.idempotency_key = "idle-queued-coalesce-2".to_owned();
    second_request.message = "Second durable message updates the existing wake.".to_owned();
    let second = state
        .append_mailbox_message_and_notify(&sender_id, second_request)
        .expect("second mailbox send should coalesce");
    assert_eq!(second.mailbox_id, first.mailbox_id);
    assert_eq!(second.notification_disposition, "queuedBehindActiveTurn");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should exist");
    assert_eq!(
        target.queued_prompts.len(),
        1,
        "idle receivers with blocked queued work must retain one wake per mailbox"
    );
    let source = target.queued_prompts[0]
        .pending_prompt
        .source
        .as_ref()
        .and_then(|source| source.mailbox.as_ref())
        .expect("coalesced wake should retain mailbox metadata");
    assert_eq!(source.message_id, second.message_id);
    assert_eq!(source.sequence, second.sequence);
    assert_eq!(source.unread_count, 2);
}

// Reproduces the user-visible failure where Stop completed after the shared
// runtime had already disappeared, then the queued mailbox wake immediately
// started another turn. Explicit Stop must leave that durable wake queued and
// engage the automatic-resume latch even on the missing-runtime recovery path.
#[test]
fn stop_with_missing_runtime_keeps_the_mailbox_wake_paused() {
    let (state, sender_id, target_id) = mailbox_test_state();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        let target = inner
            .session_mut_by_index(target_index)
            .expect("target should exist");
        target.session.status = SessionStatus::Active;
        target.runtime = SessionRuntime::None;
        target.orchestrator_auto_dispatch_blocked = false;
    }

    let stop_gate = install_test_stop_fence_gate(&state, &target_id);
    let stop_state = state.clone();
    let stop_session_id = target_id.clone();
    let stop_thread = std::thread::spawn(move || stop_state.stop_session(&stop_session_id));
    stop_gate.wait_until_claimed();

    let mut request = mailbox_send_request(&target_id);
    request.idempotency_key = "stop-missing-runtime-mailbox-pause".to_owned();
    let receipt = state
        .append_mailbox_message_and_notify(&sender_id, request)
        .expect("mailbox message should remain durable while Stop owns the fence");
    assert_eq!(receipt.notification_disposition, "queuedBehindActiveTurn");

    stop_gate.release();
    stop_thread
        .join()
        .expect("Stop thread should finish")
        .expect("Stop should recover the active session with no runtime");
    assert!(
        state
            .dispatch_next_queued_turn(&target_id, false)
            .expect("blocked queue inspection should succeed")
            .is_none(),
        "automatic queue drain after Stop must not start the mailbox turn"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should remain present");
    assert_eq!(target.session.status, SessionStatus::Idle);
    assert!(target.orchestrator_auto_dispatch_blocked);
    assert_eq!(target.queued_prompts.len(), 1);
    assert_eq!(target.queued_prompts[0].source, QueuedPromptSource::Mailbox);
    assert_eq!(target.session.pending_prompts.len(), 1);
}

// A failed accepted mailbox wake is restored durably, but it must not be
// dispatched again in the same terminalization cycle. Otherwise a poisoned
// wake loops forever at the queue head and starves every prompt behind it.
#[test]
fn failed_mailbox_turn_is_requeued_once_and_pauses_automatic_dispatch() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let first = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("first mailbox wake should queue");
    let (runtime, input_rx) = test_claude_runtime_handle("mailbox-failure-successor-runtime");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        let target = inner
            .session_mut_by_index(target_index)
            .expect("target should exist");
        target.session.status = SessionStatus::Idle;
        target.runtime = SessionRuntime::Claude(runtime);
    }
    let first_dispatch = state
        .dispatch_next_queued_turn(&target_id, false)
        .expect("first queue drain should succeed")
        .expect("first mailbox wake should dispatch");
    deliver_turn_dispatch(&state, first_dispatch).expect("first wake should reach the runtime");
    assert!(matches!(
        input_rx.recv_timeout(Duration::from_secs(1)),
        Ok(ClaudeRuntimeCommand::Prompt(_))
    ));
    assert_eq!(
        state
            .mailbox_store
            .read_message(&target_id, &first.message_id)
            .expect("first delivery state should read")
            .notification_state,
        "deliveredToIdleSession"
    );

    let mut second_request = mailbox_send_request(&target_id);
    second_request.idempotency_key = "mailbox-failure-successor-2".to_owned();
    let second = state
        .append_mailbox_message_and_notify(&sender_id, second_request)
        .expect("second mailbox wake should queue behind the active turn");
    assert_eq!(second.notification_disposition, "queuedBehindActiveTurn");

    state
        .fail_turn_if_runtime_matches(&target_id, &runtime_token, "accepted mailbox turn failed")
        .expect("failure should recover only the active mailbox delivery");
    assert!(matches!(
        input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should remain present");
    assert!(target.orchestrator_auto_dispatch_blocked);
    assert_eq!(target.session.status, SessionStatus::Error);
    assert_eq!(target.queued_prompts.len(), 1);
    assert_eq!(target.queued_prompts[0].source, QueuedPromptSource::Mailbox);
    let queued_mailbox = target.queued_prompts[0]
        .pending_prompt
        .source
        .as_ref()
        .and_then(|source| source.mailbox.as_ref())
        .expect("restored wake should retain exact mailbox metadata");
    assert_eq!(queued_mailbox.message_id, second.message_id);
    assert_eq!(queued_mailbox.sequence, second.sequence);
    drop(inner);
    assert_eq!(
        state
            .mailbox_store
            .read_message(&target_id, &first.message_id)
            .expect("failed delivery state should read")
            .notification_state,
        "recoveredWake"
    );
}

// Recovery belongs to the failed delivery boundary, not to whichever unread
// row is newest by the time the terminal callback runs. A newer successor wake
// already active on the same mailbox covers the old boundary and must not be
// queued a second time.
#[test]
fn failed_mailbox_recovery_does_not_duplicate_a_newer_active_wake() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let first = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("first mailbox wake should queue");
    let mut second_request = mailbox_send_request(&target_id);
    second_request.idempotency_key = "mailbox-boundary-successor-2".to_owned();
    second_request.message = "A newer wake already belongs to the successor turn.".to_owned();
    let second = state
        .append_mailbox_message_and_notify(&sender_id, second_request)
        .expect("second mailbox wake should coalesce");
    state
        .mailbox_store
        .set_notification_state(&first.message_id, "deliveredToIdleSession")
        .expect("test should model the failed accepted delivery");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("target index should be valid");
        record.queued_prompts.clear();
        sync_pending_prompts(record);
        record.active_turn_mailbox_notification = Some(MailboxNotificationDelivery {
            mailbox_id: second.mailbox_id.clone(),
            session_id: target_id.clone(),
            through_sequence: second.sequence,
        });
    }

    assert!(
        !state
            .requeue_rejected_mailbox_notification(&MailboxNotificationDelivery {
                mailbox_id: first.mailbox_id.clone(),
                session_id: target_id.clone(),
                through_sequence: first.sequence,
            })
            .expect("failed delivery recovery should succeed")
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should remain present");
    assert!(target.queued_prompts.is_empty());
    assert_eq!(target.session.pending_prompts.len(), 0);
    assert_eq!(
        target
            .active_turn_mailbox_notification
            .as_ref()
            .map(|notification| notification.through_sequence),
        Some(second.sequence)
    );
    drop(inner);
    assert_eq!(
        state
            .mailbox_store
            .read_message(&target_id, &first.message_id)
            .expect("failed delivery state should read")
            .notification_state,
        "recoveredWake"
    );
    assert_eq!(
        state
            .mailbox_store
            .read_message(&target_id, &second.message_id)
            .expect("successor delivery state should read")
            .notification_state,
        "queuedBehindActiveTurn"
    );
}

#[test]
fn failed_mailbox_recovery_moves_a_covering_newer_wake_ahead_of_successors() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let first = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("first mailbox wake should queue");
    let mut second_request = mailbox_send_request(&target_id);
    second_request.idempotency_key = "mailbox-covering-successor-2".to_owned();
    second_request.message = "The newer mailbox boundary covers the failed wake.".to_owned();
    let second = state
        .append_mailbox_message_and_notify(&sender_id, second_request)
        .expect("second mailbox wake should coalesce");
    state
        .mailbox_store
        .set_notification_state(&first.message_id, "deliveredToIdleSession")
        .expect("test should model the failed accepted delivery");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        let prompt_id = inner.next_message_id();
        let record = inner
            .session_mut_by_index(index)
            .expect("target index should be valid");
        record.queued_prompts.push_front(QueuedPromptRecord {
            source: QueuedPromptSource::User,
            attachments: Vec::new(),
            pending_prompt: PendingPrompt {
                attachments: Vec::new(),
                id: prompt_id,
                timestamp: stamp_now(),
                text: "successor user prompt".to_owned(),
                expanded_text: None,
                source: None,
            },
        });
        sync_pending_prompts(record);
        assert_eq!(record.queued_prompts[0].source, QueuedPromptSource::User);
        assert_eq!(record.queued_prompts[1].source, QueuedPromptSource::Mailbox);
    }

    assert!(
        state
            .requeue_rejected_mailbox_notification(&MailboxNotificationDelivery {
                mailbox_id: first.mailbox_id.clone(),
                session_id: target_id.clone(),
                through_sequence: first.sequence,
            })
            .expect("failed delivery recovery should succeed")
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should remain present");
    assert_eq!(target.queued_prompts.len(), 2);
    assert_eq!(target.queued_prompts[0].source, QueuedPromptSource::Mailbox);
    assert_eq!(target.queued_prompts[1].source, QueuedPromptSource::User);
    let queued_mailbox = target.queued_prompts[0]
        .pending_prompt
        .source
        .as_ref()
        .and_then(|source| source.mailbox.as_ref())
        .expect("the restored head should retain mailbox metadata");
    assert_eq!(queued_mailbox.sequence, second.sequence);
    assert_eq!(queued_mailbox.message_id, second.message_id);
}

#[test]
fn failed_mailbox_recovery_does_not_mark_an_ineligible_target_recovered() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let receipt = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("mailbox wake should queue");
    state
        .mailbox_store
        .set_notification_state(&receipt.message_id, "deliveredToIdleSession")
        .expect("test should model an accepted delivery");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("target index should be valid");
        record.session.parent_delegation_id = Some("delegation-after-delivery".to_owned());
        record.queued_prompts.clear();
        sync_pending_prompts(record);
    }

    assert!(
        !state
            .requeue_rejected_mailbox_notification(&MailboxNotificationDelivery {
                mailbox_id: receipt.mailbox_id.clone(),
                session_id: target_id.clone(),
                through_sequence: receipt.sequence,
            })
            .expect("ineligible recovery should no-op")
    );
    assert_eq!(
        state
            .mailbox_store
            .read_message(&target_id, &receipt.message_id)
            .expect("delivery state should remain readable")
            .notification_state,
        "deliveredToIdleSession"
    );
}

// The atomic rejected-delivery transition mutates memory before its primary
// state commit can fail. Even in that error case the dispatch still owns the
// exact mailbox boundary and must put it back in the in-process queue. SQLite
// intentionally remains deliveredToIdleSession so the boot-wide recovery pass
// is the durable fallback if the primary state remains unwritable.
#[test]
fn rejected_delivery_persistence_failure_still_requeues_the_mailbox_wake() {
    let (mut state, sender_id, target_id) = mailbox_test_state();
    let receipt = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("mailbox wake should queue");
    state
        .mailbox_store
        .set_notification_state(&receipt.message_id, "deliveredToIdleSession")
        .expect("test should model an accepted delivery");
    let (runtime, _input_rx) = test_claude_runtime_handle("mailbox-rejected-persist-failure");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("target index should be valid");
        record.queued_prompts.clear();
        sync_pending_prompts(record);
        record.runtime = SessionRuntime::Claude(runtime);
        record.active_turn_generation = 1;
        record.active_turn_mailbox_notification = Some(MailboxNotificationDelivery {
            mailbox_id: receipt.mailbox_id.clone(),
            session_id: target_id.clone(),
            through_sequence: receipt.sequence,
        });
        record.session.status = SessionStatus::Active;
        record.session.preview = "MAILBOX TURN".to_owned();
    }

    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-mailbox-rejected-persist-failure-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");
    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(failing_persistence_path.clone());

    assert!(record_rejected_turn_dispatch(
        &state,
        &target_id,
        "runtime command channel rejected the mailbox turn",
        Some(&MailboxNotificationDelivery {
            mailbox_id: receipt.mailbox_id.clone(),
            session_id: target_id.clone(),
            through_sequence: receipt.sequence,
        }),
        None,
        &runtime_token,
        1,
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should remain present");
    assert_eq!(target.session.status, SessionStatus::Error);
    assert!(target.active_turn_mailbox_notification.is_none());
    assert_eq!(target.queued_prompts.len(), 1);
    assert_eq!(
        target.queued_prompts[0]
            .pending_prompt
            .source
            .as_ref()
            .and_then(|source| source.mailbox.as_ref())
            .map(|mailbox| mailbox.sequence),
        Some(receipt.sequence)
    );
    drop(inner);
    assert_eq!(
        state
            .mailbox_store
            .read_message(&target_id, &receipt.message_id)
            .expect("delivery state should remain readable")
            .notification_state,
        "deliveredToIdleSession"
    );

    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Engram settings rotation owns runtime teardown outside the ordinary turn
// failure path. The exact mailbox delivery still has to be restored before the
// replacement runtime is allowed to drain the queue.
#[test]
fn engram_runtime_revocation_restores_the_active_mailbox_wake() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let receipt = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("mailbox wake should queue");
    let (runtime, input_rx) = test_claude_runtime_handle("engram-mailbox-revocation-runtime");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Idle;
    }
    let dispatch = state
        .dispatch_next_queued_turn(&target_id, false)
        .expect("mailbox queue drain should succeed")
        .expect("mailbox wake should dispatch");
    deliver_turn_dispatch(&state, dispatch).expect("runtime should accept the mailbox wake");
    assert!(matches!(
        input_rx.recv_timeout(Duration::from_secs(1)),
        Ok(ClaudeRuntimeCommand::Prompt(_))
    ));

    let mut batch = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        claim_engram_mcp_runtime_revocations_locked(&mut inner, std::slice::from_ref(&target_id))
    };
    let target = batch.targets.pop().expect("runtime should be claimable");
    let finalization = state.finish_revoked_engram_mcp_runtime_if_matches(
        &target_id,
        &target.token,
        target.owner_generation,
        target.stop_options.as_ref(),
        false,
        false,
        None,
    );
    assert!(finalization.failures.is_empty());
    assert!(
        finalization
            .completion
            .as_ref()
            .is_some_and(|completion| completion.should_dispatch_next),
        "successful revocation should allow the restored wake on a replacement runtime"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should remain present");
    assert!(target.active_turn_mailbox_notification.is_none());
    assert_eq!(target.queued_prompts.len(), 1);
    assert_eq!(target.queued_prompts[0].source, QueuedPromptSource::Mailbox);
    assert!(matches!(target.runtime, SessionRuntime::None));
    drop(inner);
    assert_eq!(
        state
            .mailbox_store
            .read_message(&target_id, &receipt.message_id)
            .expect("recovered delivery should remain readable")
            .notification_state,
        "recoveredWake"
    );
}

#[test]
fn errored_mailbox_turn_is_restored_without_immediate_redispatch() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let receipt = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("mailbox wake should queue");
    let (runtime, input_rx) = test_claude_runtime_handle("errored-mailbox-runtime");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Idle;
    }
    let dispatch = state
        .dispatch_next_queued_turn(&target_id, false)
        .expect("mailbox queue drain should succeed")
        .expect("mailbox wake should dispatch");
    deliver_turn_dispatch(&state, dispatch).expect("runtime should accept the mailbox wake");
    assert!(matches!(
        input_rx.recv_timeout(Duration::from_secs(1)),
        Ok(ClaudeRuntimeCommand::Prompt(_))
    ));

    state
        .mark_turn_error_if_runtime_matches(&target_id, &runtime_token, "agent error")
        .expect("turn error should restore its mailbox wake");
    assert!(matches!(
        input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should remain present");
    assert_eq!(target.session.status, SessionStatus::Error);
    assert!(target.orchestrator_auto_dispatch_blocked);
    assert_eq!(target.queued_prompts.len(), 1);
    assert_eq!(target.queued_prompts[0].source, QueuedPromptSource::Mailbox);
    drop(inner);
    assert_eq!(
        state
            .mailbox_store
            .read_message(&target_id, &receipt.message_id)
            .expect("recovered delivery should remain readable")
            .notification_state,
        "recoveredWake"
    );
}

#[test]
fn runtime_exit_restores_the_mailbox_wake_and_pauses_automatic_dispatch() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let receipt = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("mailbox wake should queue");
    let (runtime, input_rx) = test_claude_runtime_handle("exited-mailbox-runtime");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Idle;
    }
    let dispatch = state
        .dispatch_next_queued_turn(&target_id, false)
        .expect("mailbox queue drain should succeed")
        .expect("mailbox wake should dispatch");
    deliver_turn_dispatch(&state, dispatch).expect("runtime should accept the mailbox wake");
    assert!(matches!(
        input_rx.recv_timeout(Duration::from_secs(1)),
        Ok(ClaudeRuntimeCommand::Prompt(_))
    ));

    state
        .handle_runtime_exit_if_matches(&target_id, &runtime_token, Some("mailbox runtime exited"))
        .expect("runtime exit should restore its mailbox wake");
    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should remain present");
    assert_eq!(target.session.status, SessionStatus::Error);
    assert!(target.orchestrator_auto_dispatch_blocked);
    assert_eq!(target.queued_prompts.len(), 1);
    assert_eq!(target.queued_prompts[0].source, QueuedPromptSource::Mailbox);
    drop(inner);
    assert_eq!(
        state
            .mailbox_store
            .read_message(&target_id, &receipt.message_id)
            .expect("recovered delivery should remain readable")
            .notification_state,
        "recoveredWake"
    );
}

#[test]
fn idle_blocked_receiver_queues_its_first_mailbox_wake_without_reactivation() {
    let (state, codex_id, claude_id) = mailbox_test_state();
    let (runtime, input_rx, _process) = test_shared_codex_runtime("mailbox-stopped-session-pause");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let codex_index = inner
            .find_session_index(&codex_id)
            .expect("Codex target should exist");
        let target = inner
            .session_mut_by_index(codex_index)
            .expect("Codex target should exist");
        target.session.status = SessionStatus::Idle;
        target.orchestrator_auto_dispatch_blocked = true;
        assert!(target.queued_prompts.is_empty());
    }

    let mut request = mailbox_send_request(&codex_id);
    request.idempotency_key = "stopped-session-first-wake".to_owned();
    let receipt = state
        .append_mailbox_message_and_notify(&claude_id, request)
        .expect("the durable message should append without waking the stopped session");

    assert_eq!(receipt.notification_disposition, "queuedBehindActiveTurn");
    assert!(matches!(
        input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == codex_id)
        .expect("Codex target should remain present");
    assert_eq!(target.session.status, SessionStatus::Idle);
    assert!(target.orchestrator_auto_dispatch_blocked);
    assert_eq!(target.queued_prompts.len(), 1);
    assert_eq!(target.queued_prompts[0].source, QueuedPromptSource::Mailbox);
}

#[test]
fn dispatch_coalescing_never_regresses_to_an_older_sequence() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let first = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("first mailbox wake should queue");
    let mut second_request = mailbox_send_request(&target_id);
    second_request.idempotency_key = "dispatch-sequence-2".to_owned();
    second_request.message = "Newer durable message owns the retained wake.".to_owned();
    let second = state
        .append_mailbox_message_and_notify(&sender_id, second_request)
        .expect("second mailbox wake should coalesce");

    let stale_dispatch = state
        .dispatch_turn(
            &target_id,
            SendMessageRequest {
                text: "Stale wake that acquired the state lock last.".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: Some(sender_id),
                source_mailbox: Some(MailboxMessageSource {
                    mailbox_id: first.mailbox_id,
                    message_id: first.message_id,
                    sequence: first.sequence,
                    unread_count: first.unread_depth,
                }),
            },
        )
        .expect("stale wake should coalesce without replacing newer metadata");
    assert!(matches!(stale_dispatch, DispatchTurnResult::Queued));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should exist");
    assert_eq!(target.queued_prompts.len(), 1);
    let pending = &target.queued_prompts[0].pending_prompt;
    let source = pending
        .source
        .as_ref()
        .and_then(|source| source.mailbox.as_ref())
        .expect("retained wake should have mailbox metadata");
    assert_eq!(source.message_id, second.message_id);
    assert_eq!(source.sequence, second.sequence);
    assert_eq!(source.unread_count, second.unread_depth);
    assert!(pending.text.contains(&format!("#{}", second.sequence)));
}

#[test]
fn fresh_inbound_wake_coalesces_and_dispatches_recovered_mailbox_prompt_once() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let first = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("first mailbox wake should queue while the target is busy");
    state
        .mailbox_store
        .set_notification_state(&first.message_id, "recoveredWake")
        .expect("test should model the queued state produced by boot recovery");
    let input_rx = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        let (runtime, input_rx) = test_claude_runtime_handle("idle-coalesced-mailbox-runtime");
        let target = inner
            .session_mut_by_index(target_index)
            .expect("target should exist");
        target.session.status = SessionStatus::Idle;
        target.runtime = SessionRuntime::Claude(runtime);
        input_rx
    };

    let mut second_request = mailbox_send_request(&target_id);
    second_request.idempotency_key = "idle-coalesced-dispatch-2".to_owned();
    second_request.message = "The coalesced wake itself starts now.".to_owned();
    let second = state
        .append_mailbox_message_and_notify(&sender_id, second_request)
        .expect("second mailbox send should coalesce and dispatch");
    assert_eq!(second.mailbox_id, first.mailbox_id);
    assert_eq!(
        second.notification_disposition, "deliveredToIdleSession",
        "the receipt must describe the mailbox wake that actually started"
    );
    assert!(matches!(
        input_rx.recv_timeout(Duration::from_secs(1)),
        Ok(ClaudeRuntimeCommand::Prompt(_))
    ));
    assert!(
        matches!(input_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the fresh wake must update and dispatch the recovered prompt, not add a second turn"
    );
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let target = inner
            .sessions
            .iter()
            .find(|record| record.session.id == target_id)
            .expect("target should exist");
        assert!(
            target.queued_prompts.is_empty(),
            "the coalesced mailbox prompt should be promoted exactly once"
        );
    }
    for message_id in [&first.message_id, &second.message_id] {
        assert_eq!(
            state
                .mailbox_store
                .read_message(&target_id, message_id)
                .expect("covered notification state should read")
                .notification_state,
            "deliveredToIdleSession"
        );
    }
}

#[test]
fn recovery_never_regresses_an_existing_wake_to_an_older_sequence() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let first = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("first mailbox wake should queue");
    let mut second_request = mailbox_send_request(&target_id);
    second_request.idempotency_key = "recovery-sequence-2".to_owned();
    second_request.message = "Newer durable message owns the retained wake.".to_owned();
    let second = state
        .append_mailbox_message_and_notify(&sender_id, second_request)
        .expect("second mailbox wake should coalesce");
    state
        .mailbox_store
        .set_notification_state(&first.message_id, "durableButNotWoken")
        .expect("test should simulate an older lost wake");

    state
        .reconcile_never_woken_mailbox_notifications_for_session(&target_id)
        .expect("recovery should succeed");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should exist");
    assert_eq!(target.queued_prompts.len(), 1);
    let pending = &target.queued_prompts[0].pending_prompt;
    let source = pending
        .source
        .as_ref()
        .and_then(|source| source.mailbox.as_ref())
        .expect("retained wake should have mailbox metadata");
    assert_eq!(source.message_id, second.message_id);
    assert_eq!(source.sequence, second.sequence);
    assert!(pending.text.contains(&format!("#{}", second.sequence)));
}

#[test]
fn acknowledgement_eagerly_removes_the_covered_queued_wake() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let receipt = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("mailbox send should queue one wake-up");

    let summary = state
        .acknowledge_mailbox_and_remove_covered_wakeups(
            &target_id,
            &receipt.mailbox_id,
            0,
            receipt.sequence,
        )
        .expect("acknowledgement should succeed");
    assert_eq!(summary.unread_count, 0);

    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should exist");
    assert!(
        target.queued_prompts.is_empty(),
        "a queued wake covered by the durable cursor must disappear immediately"
    );
    assert!(
        target.session.pending_prompts.is_empty(),
        "the public pending-prompt projection must stay in sync"
    );
}

#[test]
fn queue_drain_skips_a_stale_wake_left_after_the_cursor_advanced() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let receipt = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("mailbox send should queue one wake-up");
    state
        .mailbox_store
        .acknowledge(&target_id, &receipt.mailbox_id, 0, receipt.sequence)
        .expect("the test should advance the durable cursor without queue cleanup");

    let input_rx = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let prompt_id = inner.next_message_id();
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        let (runtime, input_rx) = test_claude_runtime_handle("stale-mailbox-wake-runtime");
        let target = inner
            .session_mut_by_index(target_index)
            .expect("target should exist");
        queue_prompt_on_record(
            target,
            PendingPrompt {
                attachments: Vec::new(),
                id: prompt_id,
                timestamp: stamp_now(),
                text: "ordinary queued prompt".to_owned(),
                expanded_text: None,
                source: None,
            },
            Vec::new(),
        );
        target.session.status = SessionStatus::Idle;
        target.runtime = SessionRuntime::Claude(runtime);
        input_rx
    };

    let dispatch = state
        .dispatch_next_queued_turn(&target_id, true)
        .expect("queue drain should succeed")
        .expect("ordinary prompt should remain after the stale wake is dropped");
    let prompt = match &dispatch {
        TurnDispatch::PersistentClaude { command, .. } => command.text.as_str(),
        _ => panic!("expected Claude ordinary prompt"),
    };
    assert_eq!(prompt, "ordinary queued prompt");
    deliver_turn_dispatch(&state, dispatch).expect("runtime should accept the ordinary prompt");
    assert!(matches!(
        input_rx.recv_timeout(Duration::from_secs(1)),
        Ok(ClaudeRuntimeCommand::Prompt(_))
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should exist");
    assert!(target.queued_prompts.is_empty());
    assert!(
        target.session.messages.iter().all(|message| {
            !matches!(
                message,
                Message::Text {
                    source: Some(source),
                    ..
                } if source.is_mailbox()
            )
        }),
        "the stale mailbox wake must not become a transcript turn"
    );
}

#[test]
fn delivered_unacknowledged_notification_does_not_loop_or_starve_user_prompt() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let receipt = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("mailbox send should queue one wake-up");
    state
        .dispatch_turn(
            &target_id,
            SendMessageRequest {
                text: "ordinary queued prompt".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .expect("ordinary prompt should queue behind the active turn");

    let input_rx = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        let (runtime, input_rx) = test_claude_runtime_handle("mailbox-loop-runtime");
        let target = inner
            .session_mut_by_index(target_index)
            .expect("target should exist");
        target.session.status = SessionStatus::Idle;
        target.runtime = SessionRuntime::Claude(runtime);
        assert_eq!(
            target
                .queued_prompts
                .iter()
                .map(|queued| queued.source)
                .collect::<Vec<_>>(),
            vec![QueuedPromptSource::Mailbox, QueuedPromptSource::User]
        );
        input_rx
    };

    let first = state
        .dispatch_next_queued_turn(&target_id, true)
        .expect("mailbox wake should dispatch")
        .expect("mailbox wake should exist");
    let first_prompt = match &first {
        TurnDispatch::PersistentClaude { command, .. } => command.text.clone(),
        _ => panic!("expected Claude mailbox wake"),
    };
    assert_eq!(
        first_prompt,
        mailbox_notification_text(&receipt.mailbox_id, 1, receipt.sequence, "Sol"),
        "human-only queue presentation must not rewrite the agent-facing activation prompt"
    );
    deliver_turn_dispatch(&state, first).expect("runtime should accept the mailbox wake");
    assert!(matches!(
        input_rx.recv_timeout(Duration::from_secs(1)),
        Ok(ClaudeRuntimeCommand::Prompt(_))
    ));
    assert_eq!(
        state
            .mailbox_store
            .read_message(&target_id, &receipt.message_id)
            .expect("notification state should read")
            .notification_state,
        "deliveredToIdleSession"
    );
    assert!(
        state
            .mailbox_store
            .unread_wakeups_for_session(&target_id)
            .expect("notification state should read")
            .is_empty(),
        "runtime acceptance marks every coalesced inbound notification delivered even before ack"
    );
    assert_eq!(
        state
            .mailbox_store
            .list_for_session(&target_id)
            .expect("mailbox summary should read")[0]
            .unread_count,
        1,
        "delivery and acknowledgement remain separate"
    );

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        inner.sessions[target_index].session.status = SessionStatus::Idle;
    }
    let second = state
        .dispatch_next_queued_turn(&target_id, true)
        .expect("ordinary prompt should dispatch")
        .expect("ordinary prompt should remain queued");
    let second_prompt = match second {
        TurnDispatch::PersistentClaude { command, .. } => command.text,
        _ => panic!("expected Claude ordinary prompt"),
    };
    assert_eq!(second_prompt, "ordinary queued prompt");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        inner.sessions[target_index].session.status = SessionStatus::Idle;
    }
    assert!(
        state
            .dispatch_next_queued_turn(&target_id, true)
            .expect("empty queue drain should succeed")
            .is_none(),
        "an unacknowledged delivered notification must not recreate itself"
    );
}

#[test]
fn mailbox_remains_available_after_state_persist_worker_shutdown() {
    let (state, sender_id, target_id) = mailbox_test_state();
    state.shutdown_persist_blocking();
    assert!(!state.persist_worker_alive.load(Ordering::Acquire));

    let receipt = state
        .append_mailbox_message_and_notify(&sender_id, mailbox_send_request(&target_id))
        .expect("mailbox send must bypass the stopped state persist worker");
    assert_eq!(
        state
            .mailbox_store
            .read_message(&target_id, &receipt.message_id)
            .expect("committed mailbox message should remain readable")
            .body,
        "The durable body must never enter the target prompt."
    );
}

#[test]
fn reopened_mailbox_store_recovers_lost_wake_before_receivers_next_turn() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let coordination_path = resolve_coordination_persistence_path(state.persistence_path.as_ref());
    let committed = state
        .mailbox_store
        .append(&MailboxAppendInput {
            sender_session_id: sender_id.clone(),
            sender_name: "Sol".to_owned(),
            target_session_id: target_id.clone(),
            target_name: "Fable".to_owned(),
            body: "Committed before a simulated crash.".to_owned(),
            idempotency_key: "lost-wake-1".to_owned(),
            topic: Some("recovery".to_owned()),
            state_stamp: None,
        })
        .expect("body should commit without attempting notification");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        let (runtime, _input_rx) = test_claude_runtime_handle("mailbox-recovery-runtime");
        let target = inner
            .session_mut_by_index(target_index)
            .expect("target should exist");
        target.session.status = SessionStatus::Idle;
        target.runtime = SessionRuntime::Claude(runtime);
        assert!(target.queued_prompts.is_empty());
    }

    let restarted = AppState {
        mailbox_store: Arc::new(
            MailboxStore::open(&coordination_path).expect("mailbox store should reopen"),
        ),
        ..state.clone()
    };
    let result = restarted
        .dispatch_turn(
            &target_id,
            SendMessageRequest {
                text: "ordinary next-turn prompt".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .expect("next turn should recover unread mailbox metadata");
    let runtime_prompt = match result {
        DispatchTurnResult::DispatchedAfterQueue(TurnDispatch::PersistentClaude {
            command,
            ..
        }) => command.text,
        DispatchTurnResult::DispatchedAfterQueue(_) => {
            panic!("expected recovered mailbox wake to use the Claude runtime")
        }
        DispatchTurnResult::Dispatched(_) => {
            panic!("expected recovered mailbox wake to dispatch before the submitted prompt")
        }
        DispatchTurnResult::Queued => {
            panic!("idle receiver should dispatch the recovered mailbox wake")
        }
    };
    assert!(runtime_prompt.contains(&committed.mailbox_id));
    assert!(runtime_prompt.contains("termal_read_mailbox"));
    assert!(runtime_prompt.contains("TERMAL_CLI"));
    assert!(
        !runtime_prompt.contains("Committed before a simulated crash."),
        "recovery wake must remain metadata-only"
    );
    let inner = restarted.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should exist");
    assert_eq!(target.queued_prompts.len(), 1);
    assert_eq!(
        target.queued_prompts[0].pending_prompt.text,
        "ordinary next-turn prompt"
    );
}

#[test]
fn boot_recovers_a_delivered_notification_after_its_turn_dies_exactly_once() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let coordination_path = resolve_coordination_persistence_path(state.persistence_path.as_ref());
    let committed = state
        .mailbox_store
        .append(&MailboxAppendInput {
            sender_session_id: sender_id,
            sender_name: "Sol".to_owned(),
            target_session_id: target_id.clone(),
            target_name: "Fable".to_owned(),
            body: "The first delivered turn died before acknowledgement.".to_owned(),
            idempotency_key: "dead-delivered-turn-1".to_owned(),
            topic: Some("recovery".to_owned()),
            state_stamp: None,
        })
        .expect("mailbox body should commit");
    state
        .mailbox_store
        .set_notification_state(&committed.message_id, "deliveredToIdleSession")
        .expect("the pre-crash wake should be recorded as delivered");
    let input_rx = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        let (runtime, input_rx) =
            test_acp_runtime_handle(AcpAgent::Cursor, "mailbox-boot-recovery-runtime");
        state.install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
        let target = inner
            .session_mut_by_index(target_index)
            .expect("target should exist");
        target.session.agent = Agent::Cursor;
        target.session.status = SessionStatus::Idle;
        target.runtime = SessionRuntime::None;
        target.queued_prompts.clear();
        sync_pending_prompts(target);
        input_rx
    };

    let restarted = AppState {
        mailbox_store: Arc::new(
            MailboxStore::open(&coordination_path).expect("mailbox store should reopen"),
        ),
        ..state.clone()
    };
    restarted
        .run_post_listen_boot()
        .expect("post-listen boot recovery should run");
    {
        let inner = restarted.inner.lock().expect("state mutex poisoned");
        let target = inner
            .sessions
            .iter()
            .find(|record| record.session.id == target_id)
            .expect("target should exist");
        assert_eq!(
            target.queued_prompts.len(),
            1,
            "boot should recreate the wake for an unread delivered message"
        );
        assert_eq!(target.queued_prompts[0].source, QueuedPromptSource::Mailbox);
        assert_eq!(target.session.pending_prompts.len(), 1);
        assert_eq!(
            target.session.status,
            SessionStatus::Idle,
            "boot recovery must not activate the receiving session"
        );
        assert!(
            matches!(target.runtime, SessionRuntime::None),
            "boot recovery must not spawn or attach an agent runtime"
        );
    }
    assert!(
        matches!(input_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "boot recovery must not deliver the recovered wake"
    );
    assert_eq!(
        restarted
            .mailbox_store
            .read_message(&target_id, &committed.message_id)
            .expect("recovery disposition should read")
            .notification_state,
        "recoveredWake",
        "boot recovery should record that the unread wake is queued again"
    );

    let recovered = restarted
        .dispatch_turn(
            &target_id,
            SendMessageRequest {
                text: "ordinary next-turn prompt".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .expect("a genuine activation should drain the recovered wake first");
    let recovered = match recovered {
        DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Dispatched(_) => {
            panic!("the ordinary prompt must not overtake the recovered wake")
        }
        DispatchTurnResult::Queued => {
            panic!("an idle receiver should dispatch the recovered wake")
        }
    };
    let recovered_prompt = match &recovered {
        TurnDispatch::PersistentAcp { command, .. } => command.prompt.clone(),
        _ => panic!("expected Cursor recovery wake"),
    };
    assert!(recovered_prompt.contains(&committed.mailbox_id));
    deliver_turn_dispatch(&restarted, recovered)
        .expect("runtime should accept the boot recovery wake");
    assert!(matches!(
        input_rx.recv_timeout(Duration::from_secs(1)),
        Ok(AcpRuntimeCommand::Prompt(_))
    ));

    {
        let inner = restarted.inner.lock().expect("state mutex poisoned");
        let target = inner
            .sessions
            .iter()
            .find(|record| record.session.id == target_id)
            .expect("target should exist");
        assert_eq!(
            target.queued_prompts.len(),
            1,
            "the activating prompt should remain queued behind the recovered wake"
        );
        assert_eq!(
            target.queued_prompts[0].pending_prompt.text,
            "ordinary next-turn prompt"
        );
        assert!(
            target
                .queued_prompts
                .iter()
                .all(|queued| queued.source != QueuedPromptSource::Mailbox),
            "the recovered mailbox wake should be promoted exactly once"
        );
    }
    assert_eq!(
        restarted
            .mailbox_store
            .read_message(&target_id, &committed.message_id)
            .expect("delivered recovery state should read")
            .notification_state,
        "deliveredToIdleSession",
        "runtime acceptance should advance the recovered wake to delivered"
    );
    assert_eq!(
        restarted
            .mailbox_store
            .list_for_session(&target_id)
            .expect("mailbox summary should read")[0]
            .unread_count,
        1,
        "delivery recovery must not acknowledge the durable message"
    );
}

#[test]
fn rejected_boot_recovery_wake_is_immediately_requeued() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let committed = state
        .mailbox_store
        .append(&MailboxAppendInput {
            sender_session_id: sender_id,
            sender_name: "Sol".to_owned(),
            target_session_id: target_id.clone(),
            target_name: "Fable".to_owned(),
            body: "The recovered wake must survive runtime rejection.".to_owned(),
            idempotency_key: "rejected-boot-recovery-wake".to_owned(),
            topic: Some("recovery".to_owned()),
            state_stamp: None,
        })
        .expect("mailbox body should commit");
    state
        .mailbox_store
        .set_notification_state(&committed.message_id, "deliveredToIdleSession")
        .expect("the pre-crash wake should be recorded as delivered");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        let (runtime, input_rx) =
            test_acp_runtime_handle(AcpAgent::Cursor, "rejected-boot-recovery-runtime");
        drop(input_rx);
        state.install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
        let target = inner
            .session_mut_by_index(target_index)
            .expect("target should exist");
        target.session.agent = Agent::Cursor;
        target.session.status = SessionStatus::Idle;
        target.runtime = SessionRuntime::None;
        target.queued_prompts.clear();
        sync_pending_prompts(target);
    }

    state
        .run_post_listen_boot()
        .expect("post-listen boot recovery should run");
    let dispatch = state
        .dispatch_turn(
            &target_id,
            SendMessageRequest {
                text: "ordinary activation after restart".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .expect("the genuine activation should promote the recovery wake");
    let dispatch = match dispatch {
        DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        _ => panic!("the recovered wake must stay ahead of the ordinary activation"),
    };
    deliver_turn_dispatch(&state, dispatch)
        .expect_err("the closed runtime channel should reject the recovered wake");

    assert_eq!(
        state
            .mailbox_store
            .read_message(&target_id, &committed.message_id)
            .expect("recovery state should remain readable")
            .notification_state,
        "recoveredWake"
    );
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let target = inner
            .sessions
            .iter()
            .find(|record| record.session.id == target_id)
            .expect("target should exist");
        assert_eq!(
            target.queued_prompts.front().map(|queued| queued.source),
            Some(QueuedPromptSource::Mailbox),
            "runtime rejection must immediately recreate the durable recovery wake"
        );
        assert!(
            target
                .queued_prompts
                .iter()
                .any(|queued| queued.pending_prompt.text == "ordinary activation after restart"),
            "the activation that exposed the failed channel must remain queued behind the wake"
        );
    }

    let (runtime, input_rx) =
        test_acp_runtime_handle(AcpAgent::Cursor, "accepted-recovery-retry-runtime");
    state.install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
    let retry = state
        .dispatch_turn(
            &target_id,
            SendMessageRequest {
                text: "second genuine activation".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .expect("a later activation should retry the restored wake");
    let retry = match retry {
        DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        _ => panic!("the restored wake must remain ahead of later activations"),
    };
    deliver_turn_dispatch(&state, retry).expect("the replacement runtime should accept the wake");
    assert!(matches!(
        input_rx.recv_timeout(Duration::from_secs(1)),
        Ok(AcpRuntimeCommand::Prompt(command)) if command.prompt.contains(&committed.mailbox_id)
    ));
    assert_eq!(
        state
            .mailbox_store
            .read_message(&target_id, &committed.message_id)
            .expect("accepted retry state should remain readable")
            .notification_state,
        "deliveredToIdleSession"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should exist");
    assert!(
        target
            .queued_prompts
            .iter()
            .all(|queued| queued.source != QueuedPromptSource::Mailbox),
        "successful retry must remove the recovery wake exactly once"
    );
}

#[test]
fn boot_dispatches_committed_workflow_queue_heads() {
    let (state, _sender_id, target_id) = mailbox_test_state();
    let input_rx = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        let (runtime, input_rx) =
            test_acp_runtime_handle(AcpAgent::Cursor, "workflow-boot-recovery-runtime");
        state.install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
        let target = inner
            .session_mut_by_index(target_index)
            .expect("target should exist");
        target.session.agent = Agent::Cursor;
        target.session.status = SessionStatus::Idle;
        target.runtime = SessionRuntime::None;
        target.queued_prompts.clear();
        queue_orchestrator_prompt_on_record(
            target,
            PendingPrompt {
                attachments: Vec::new(),
                id: "committed-workflow-resume".to_owned(),
                timestamp: stamp_now(),
                text: "resume committed delegation or orchestrator workflow".to_owned(),
                expanded_text: None,
                source: None,
            },
            Vec::new(),
        );
        input_rx
    };

    state
        .run_post_listen_boot()
        .expect("post-listen boot recovery should run");

    assert!(matches!(
        input_rx.recv_timeout(Duration::from_secs(1)),
        Ok(AcpRuntimeCommand::Prompt(command))
            if command.prompt == "resume committed delegation or orchestrator workflow"
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should exist");
    assert!(target.queued_prompts.is_empty());
    assert_eq!(target.session.status, SessionStatus::Active);
}

#[test]
fn boot_keeps_a_user_queue_barrier_and_workflow_behind_it_dormant() {
    let (state, _sender_id, target_id) = mailbox_test_state();
    let input_rx = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        let (runtime, input_rx) =
            test_acp_runtime_handle(AcpAgent::Cursor, "user-boot-dormancy-runtime");
        state.install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
        let target = inner
            .session_mut_by_index(target_index)
            .expect("target should exist");
        target.session.agent = Agent::Cursor;
        target.session.status = SessionStatus::Idle;
        target.runtime = SessionRuntime::None;
        target.queued_prompts.clear();
        queue_prompt_on_record(
            target,
            PendingPrompt {
                attachments: Vec::new(),
                id: "committed-user-prompt".to_owned(),
                timestamp: stamp_now(),
                text: "remain dormant until genuine activation".to_owned(),
                expanded_text: None,
                source: None,
            },
            Vec::new(),
        );
        queue_orchestrator_prompt_on_record(
            target,
            PendingPrompt {
                attachments: Vec::new(),
                id: "workflow-behind-user-barrier".to_owned(),
                timestamp: stamp_now(),
                text: "remain behind the committed user prompt".to_owned(),
                expanded_text: None,
                source: None,
            },
            Vec::new(),
        );
        input_rx
    };

    state
        .run_post_listen_boot()
        .expect("post-listen boot recovery should run");

    assert!(
        matches!(input_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "boot must not deliver an ordinary user queue head"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should exist");
    assert_eq!(target.queued_prompts.len(), 2);
    assert_eq!(target.queued_prompts[0].source, QueuedPromptSource::User);
    assert_eq!(
        target.queued_prompts[1].source,
        QueuedPromptSource::Orchestrator
    );
    assert_eq!(target.session.status, SessionStatus::Idle);
    assert!(matches!(target.runtime, SessionRuntime::None));
}

#[test]
fn boot_workflow_activation_drains_a_recovered_mailbox_wake_first() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let committed = state
        .mailbox_store
        .append(&MailboxAppendInput {
            sender_session_id: sender_id,
            sender_name: "Sol".to_owned(),
            target_session_id: target_id.clone(),
            target_name: "Fable".to_owned(),
            body: "Unread mailbox body before workflow crash recovery.".to_owned(),
            idempotency_key: "mixed-mailbox-workflow-boot".to_owned(),
            topic: Some("recovery".to_owned()),
            state_stamp: None,
        })
        .expect("mailbox body should commit");
    state
        .mailbox_store
        .set_notification_state(&committed.message_id, "deliveredToIdleSession")
        .expect("the pre-crash wake should be recorded as delivered");

    let input_rx = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        let (runtime, input_rx) =
            test_acp_runtime_handle(AcpAgent::Cursor, "mixed-boot-recovery-runtime");
        state.install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
        let target = inner
            .session_mut_by_index(target_index)
            .expect("target should exist");
        target.session.agent = Agent::Cursor;
        target.session.status = SessionStatus::Idle;
        target.runtime = SessionRuntime::None;
        target.queued_prompts.clear();
        queue_orchestrator_prompt_on_record(
            target,
            PendingPrompt {
                attachments: Vec::new(),
                id: "committed-workflow-behind-mailbox".to_owned(),
                timestamp: stamp_now(),
                text: "resume workflow after recovered mailbox wake".to_owned(),
                expanded_text: None,
                source: None,
            },
            Vec::new(),
        );
        input_rx
    };

    state
        .run_post_listen_boot()
        .expect("post-listen boot recovery should run");

    let runtime_prompt = match input_rx.recv_timeout(Duration::from_secs(1)) {
        Ok(AcpRuntimeCommand::Prompt(command)) => command.prompt,
        Ok(_) => panic!("expected recovered mailbox wake prompt dispatch"),
        Err(err) => panic!("expected recovered mailbox wake dispatch: {err}"),
    };
    assert!(runtime_prompt.contains(&committed.mailbox_id));
    assert!(
        !runtime_prompt.contains("resume workflow after recovered mailbox wake"),
        "the workflow activation must not overtake the recovered mailbox wake"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should exist");
    assert_eq!(target.queued_prompts.len(), 1);
    assert_eq!(
        target.queued_prompts[0].source,
        QueuedPromptSource::Orchestrator
    );
    assert_eq!(
        target.queued_prompts[0].pending_prompt.text,
        "resume workflow after recovered mailbox wake"
    );
    assert_eq!(target.session.status, SessionStatus::Active);
}

#[test]
fn boot_workflow_activation_retries_a_rejected_recovered_wake_once() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let committed = state
        .mailbox_store
        .append(&MailboxAppendInput {
            sender_session_id: sender_id,
            sender_name: "Sol".to_owned(),
            target_session_id: target_id.clone(),
            target_name: "Fable".to_owned(),
            body: "Recovered wake survives one stale runtime before workflow recovery.".to_owned(),
            idempotency_key: "mixed-mailbox-workflow-rejected-boot".to_owned(),
            topic: Some("recovery".to_owned()),
            state_stamp: None,
        })
        .expect("mailbox body should commit");
    state
        .mailbox_store
        .set_notification_state(&committed.message_id, "deliveredToIdleSession")
        .expect("the pre-crash wake should be recorded as delivered");

    let (rejected_runtime, rejected_rx) =
        test_acp_runtime_handle(AcpAgent::Cursor, "rejected-mixed-boot-runtime");
    drop(rejected_rx);
    state.install_test_acp_runtime_override(AcpAgent::Cursor, rejected_runtime);
    let (accepted_runtime, accepted_rx) =
        test_acp_runtime_handle(AcpAgent::Cursor, "accepted-mixed-boot-retry-runtime");
    state.install_test_acp_runtime_override(AcpAgent::Cursor, accepted_runtime);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        let target = inner
            .session_mut_by_index(target_index)
            .expect("target should exist");
        target.session.agent = Agent::Cursor;
        target.session.status = SessionStatus::Idle;
        target.runtime = SessionRuntime::None;
        target.queued_prompts.clear();
        queue_orchestrator_prompt_on_record(
            target,
            PendingPrompt {
                attachments: Vec::new(),
                id: "workflow-behind-rejected-recovery-wake".to_owned(),
                timestamp: stamp_now(),
                text: "resume workflow after the recovered wake retry".to_owned(),
                expanded_text: None,
                source: None,
            },
            Vec::new(),
        );
    }

    state
        .run_post_listen_boot()
        .expect("post-listen boot recovery should run");

    assert!(matches!(
        accepted_rx.recv_timeout(Duration::from_secs(1)),
        Ok(AcpRuntimeCommand::Prompt(command)) if command.prompt.contains(&committed.mailbox_id)
    ));
    assert_eq!(
        state
            .mailbox_store
            .read_message(&target_id, &committed.message_id)
            .expect("accepted retry state should remain readable")
            .notification_state,
        "deliveredToIdleSession"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should exist");
    assert_eq!(target.session.status, SessionStatus::Active);
    assert_eq!(target.queued_prompts.len(), 1);
    assert_eq!(
        target.queued_prompts[0].source,
        QueuedPromptSource::Orchestrator
    );
    assert_eq!(
        target.queued_prompts[0].pending_prompt.text,
        "resume workflow after the recovered wake retry"
    );
}

#[test]
fn boot_requeues_a_rejected_workflow_head_for_a_later_recovery_pass() {
    let (state, _sender_id, target_id) = mailbox_test_state();
    let (rejected_runtime, rejected_rx) =
        test_acp_runtime_handle(AcpAgent::Cursor, "rejected-workflow-head-runtime");
    drop(rejected_rx);
    state.install_test_acp_runtime_override(AcpAgent::Cursor, rejected_runtime);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let target_index = inner
            .find_session_index(&target_id)
            .expect("target should exist");
        let target = inner
            .session_mut_by_index(target_index)
            .expect("target should exist");
        target.session.agent = Agent::Cursor;
        target.session.status = SessionStatus::Idle;
        target.runtime = SessionRuntime::None;
        target.queued_prompts.clear();
        queue_orchestrator_prompt_on_record(
            target,
            PendingPrompt {
                attachments: Vec::new(),
                id: "rejected-workflow-head".to_owned(),
                timestamp: stamp_now(),
                text: "retry this committed workflow on the next recovery pass".to_owned(),
                expanded_text: None,
                source: None,
            },
            Vec::new(),
        );
    }

    state
        .run_post_listen_boot()
        .expect("post-listen boot recovery should run");

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let target = inner
            .sessions
            .iter()
            .find(|record| record.session.id == target_id)
            .expect("target should exist");
        assert_eq!(target.session.status, SessionStatus::Error);
        assert!(matches!(target.runtime, SessionRuntime::None));
        assert_eq!(target.queued_prompts.len(), 1);
        assert_eq!(
            target.queued_prompts[0].source,
            QueuedPromptSource::Orchestrator
        );
        assert_eq!(
            target.queued_prompts[0].pending_prompt.text,
            "retry this committed workflow on the next recovery pass"
        );
        assert_ne!(
            target.queued_prompts[0].pending_prompt.id, "rejected-workflow-head",
            "a retry needs a fresh transcript message id"
        );
    }

    let (accepted_runtime, accepted_rx) =
        test_acp_runtime_handle(AcpAgent::Cursor, "accepted-workflow-head-retry-runtime");
    state.install_test_acp_runtime_override(AcpAgent::Cursor, accepted_runtime);
    state.dispatch_orphaned_workflow_prompts();

    assert!(matches!(
        accepted_rx.recv_timeout(Duration::from_secs(1)),
        Ok(AcpRuntimeCommand::Prompt(command))
            if command.prompt.contains("retry this committed workflow on the next recovery pass")
    ));
    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should exist");
    assert_eq!(target.session.status, SessionStatus::Active);
    assert!(target.queued_prompts.is_empty());
}

#[test]
fn mailbox_stop_class_is_rejected_without_persisting_or_waking() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let mut request = mailbox_send_request(&target_id);
    request.class = Some("stop".to_owned());
    let error = state
        .append_mailbox_message_and_notify(&sender_id, request)
        .expect_err("STOP semantics are not active in the foundation");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(error.message.contains("STOP/urgent"));
    assert!(
        state
            .mailbox_store
            .list_for_session(&target_id)
            .expect("mailbox listing should succeed")
            .is_empty()
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let target = inner
        .sessions
        .iter()
        .find(|record| record.session.id == target_id)
        .expect("target should exist");
    assert!(target.queued_prompts.is_empty());
}

#[tokio::test]
async fn mailbox_http_routes_append_read_and_acknowledge_without_implicit_read_ack() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let app = app_router(state.clone());
    let send_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{sender_id}/mailboxes/send"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "targetSessionId": target_id,
                        "message": "HTTP durable body",
                        "idempotencyKey": "http-send-1",
                        "class": "routine"
                    }))
                    .expect("send request should serialize"),
                ))
                .expect("send request should build"),
        )
        .await
        .expect("send route should respond");
    assert_eq!(send_response.status(), StatusCode::ACCEPTED);
    let send_body = to_bytes(send_response.into_body(), usize::MAX)
        .await
        .expect("send response body should read");
    let receipt: MailboxAppendReceipt =
        serde_json::from_slice(&send_body).expect("receipt should deserialize");

    let read_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/sessions/{target_id}/mailboxes/{}/read",
                    receipt.mailbox_id
                ))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"afterSequence":0,"limit":20}"#))
                .expect("read request should build"),
        )
        .await
        .expect("read route should respond");
    assert_eq!(read_response.status(), StatusCode::OK);
    let read_body = to_bytes(read_response.into_body(), usize::MAX)
        .await
        .expect("read response body should read");
    let read_json: Value =
        serde_json::from_slice(&read_body).expect("message JSON should deserialize");
    assert_eq!(
        read_json[0]["notificationState"], "queuedBehindActiveTurn",
        "read responses expose the current mutable notification lifecycle"
    );
    assert!(
        read_json[0].get("notificationDisposition").is_none(),
        "read responses must not reuse the immutable receipt field name"
    );
    let messages: Vec<MailboxMessage> =
        serde_json::from_slice(&read_body).expect("messages should deserialize");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].body, "HTTP durable body");

    let outsider_id = test_session_id(&state, Agent::Codex);
    for request in [
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{outsider_id}/mailboxes/{}/read",
                receipt.mailbox_id
            ))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"afterSequence":0,"limit":20}"#))
            .expect("outsider read request should build"),
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{outsider_id}/mailboxes/{}/acknowledge",
                receipt.mailbox_id
            ))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"expectedProcessedThrough":0,"processedThrough":1}"#,
            ))
            .expect("outsider ack request should build"),
    ] {
        let response = app
            .clone()
            .oneshot(request)
            .await
            .expect("non-participant route should respond");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    let missing_session_list = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/sessions/session-missing/mailboxes")
                .body(Body::empty())
                .expect("missing-session list request should build"),
        )
        .await
        .expect("missing-session list route should respond");
    assert_eq!(missing_session_list.status(), StatusCode::NOT_FOUND);

    let list_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/sessions/{target_id}/mailboxes"))
                .body(Body::empty())
                .expect("list request should build"),
        )
        .await
        .expect("list route should respond");
    let list_body = to_bytes(list_response.into_body(), usize::MAX)
        .await
        .expect("list response body should read");
    let before_ack: Vec<MailboxSummary> =
        serde_json::from_slice(&list_body).expect("mailboxes should deserialize");
    assert_eq!(
        before_ack[0].unread_count, 1,
        "reading bodies must not acknowledge them"
    );

    let ack_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/sessions/{target_id}/mailboxes/{}/acknowledge",
                    receipt.mailbox_id
                ))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"expectedProcessedThrough":0,"processedThrough":1}"#,
                ))
                .expect("ack request should build"),
        )
        .await
        .expect("ack route should respond");
    assert_eq!(ack_response.status(), StatusCode::OK);
    let ack_body = to_bytes(ack_response.into_body(), usize::MAX)
        .await
        .expect("ack response body should read");
    let after_ack: MailboxSummary =
        serde_json::from_slice(&ack_body).expect("ack summary should deserialize");
    assert_eq!(after_ack.unread_count, 0);
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let target = inner
            .sessions
            .iter()
            .find(|record| record.session.id == target_id)
            .expect("target should exist");
        assert!(
            target.queued_prompts.is_empty(),
            "the HTTP acknowledgement must retire its covered queued wake"
        );
    }

    for (body, expected_status) in [
        (
            json!({
                "targetSessionId": target_id,
                "message": "invalid empty key",
                "idempotencyKey": "",
                "class": "routine"
            }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({
                "targetSessionId": target_id,
                "message": "invalid oversized key",
                "idempotencyKey": "x".repeat(257),
                "class": "routine"
            }),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{sender_id}/mailboxes/send"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&body).expect("invalid request should serialize"),
                    ))
                    .expect("invalid send request should build"),
            )
            .await
            .expect("invalid send route should respond");
        assert_eq!(response.status(), expected_status);
    }

    let second_send_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{sender_id}/mailboxes/send"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "targetSessionId": target_id,
                        "message": "second message",
                        "idempotencyKey": "mailbox-http-second",
                        "class": "routine"
                    }))
                    .expect("second send should serialize"),
                ))
                .expect("second send request should build"),
        )
        .await
        .expect("second send route should respond");
    assert_eq!(second_send_response.status(), StatusCode::ACCEPTED);

    for (body, expected_status) in [
        (
            r#"{"expectedProcessedThrough":0,"processedThrough":1}"#,
            StatusCode::OK,
        ),
        (
            r#"{"expectedProcessedThrough":0,"processedThrough":2}"#,
            StatusCode::CONFLICT,
        ),
        (
            r#"{"expectedProcessedThrough":1,"processedThrough":0}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            r#"{"expectedProcessedThrough":1,"processedThrough":3}"#,
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/sessions/{target_id}/mailboxes/{}/acknowledge",
                        receipt.mailbox_id
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .expect("invalid ack request should build"),
            )
            .await
            .expect("invalid ack route should respond");
        assert_eq!(response.status(), expected_status);
    }
}

#[tokio::test]
async fn mailbox_http_send_surfaces_writer_admission_exhaustion_as_retryable_503() {
    let (base_state, sender_id, target_id) = mailbox_test_state();
    let coordination_path =
        resolve_coordination_persistence_path(base_state.persistence_path.as_ref());
    let state = AppState {
        mailbox_store: Arc::new(
            MailboxStore::open_with_write_admission_timeout(&coordination_path, Duration::ZERO)
                .expect("zero-deadline mailbox store should open"),
        ),
        ..base_state
    };
    let writer_lock = sqlite_state_write_lock(&coordination_path);
    let (locked_tx, locked_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = lock_sqlite_state_writer(&writer_lock);
        locked_tx
            .send(())
            .expect("writer-lock observer should remain connected");
        release_rx
            .recv()
            .expect("writer-lock holder should be released");
    });
    locked_rx
        .recv()
        .expect("test should hold the shared writer boundary");

    let response = app_router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{sender_id}/mailboxes/send"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "targetSessionId": target_id,
                        "message": "HTTP admission test",
                        "idempotencyKey": "http-busy-1",
                        "class": "routine"
                    }))
                    .expect("send request should serialize"),
                ))
                .expect("send request should build"),
        )
        .await
        .expect("send route should respond");
    release_tx
        .send(())
        .expect("writer-lock holder should release");
    holder.join().expect("writer-lock holder should join");

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("503 response body should read");
    let body: Value = serde_json::from_slice(&body).expect("503 body should decode");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|message| message.contains("retry the same request")),
        "writer admission response should preserve retry guidance: {body}"
    );
}
