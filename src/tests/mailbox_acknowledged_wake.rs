//! A mailbox wake accepted while its target's turn runs stays queued when the
//! target reads and acknowledges that mail in the same turn: acknowledgement
//! advances the data cursor only, and the wake still starts one continuation
//! turn when the turn ends.
//!
//! Owns: the acknowledgement path of that contract: one coalesced turn, the
//! continuation's honest text at zero unread, the stored state while the wake
//! waits, FIFO, Stop, pause and Cancel barriers, and the immutability of a
//! retained head. Does not own: failure, restart and Cancel label paths,
//! which are tracked separately (wake ownership redesign), or ordinary unread
//! wake delivery, which `mailboxes.rs` pins. Split out of `tests/mailboxes.rs`
//! to keep that file from growing.
//!
//! Every runtime here is a test channel, so delivery is synchronous: a prompt
//! the drain hands off is already in the channel when the call returns, and
//! an empty channel proves no turn was started. No timing is involved.

use super::mailboxes::mailbox_test_state;
use super::*;

const RUNTIME_ID: &str = "acknowledged-wake-runtime";

pub(super) fn send(
    state: &AppState,
    sender_id: &str,
    target_id: &str,
    key: &str,
) -> MailboxAppendReceipt {
    state
        .append_mailbox_message_and_notify(
            sender_id,
            SendMailboxMessageRequest {
                target_session_id: target_id.to_owned(),
                message: format!("body of {key} stays in the mailbox"),
                idempotency_key: key.to_owned(),
                topic: Some("next step".to_owned()),
                state_stamp: None,
                class: Some("routine".to_owned()),
            },
        )
        .expect("mailbox send should commit")
}

async fn post(state: &AppState, path: String, body: Value) -> Value {
    let response = app_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .expect("mailbox route should respond");
    assert!(response.status().is_success(), "{}", response.status());
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// The production read-then-acknowledge sequence an agent runs inside its
/// turn: an issued page, then a receipt acknowledgement of that page.
/// Returns the page receipt, which stays valid for a repeated acknowledgement.
pub(super) async fn read_and_acknowledge(
    state: &AppState,
    target_id: &str,
    mailbox_id: &str,
) -> Value {
    let base = format!("/api/sessions/{target_id}/mailboxes/{mailbox_id}");
    let page = post(state, format!("{base}/read"), json!({"issueReceipt": true})).await;
    let receipt = page["receipt"].clone();
    assert!(receipt.is_string(), "the page must be issued: {page}");
    acknowledge_receipt(state, target_id, mailbox_id, &receipt).await;
    receipt
}

async fn acknowledge_receipt(state: &AppState, target_id: &str, mailbox_id: &str, receipt: &Value) {
    let summary = post(
        state,
        format!("/api/sessions/{target_id}/mailboxes/{mailbox_id}/acknowledge"),
        json!({"receipt": receipt}),
    )
    .await;
    assert_eq!(summary["unreadCount"], 0, "the page covers every message");
}

/// Reads and acknowledges through the store alone, with no queue refresh, the
/// way a cursor can move when the refresh fails after the store commit.
fn acknowledge_through_store_only(
    state: &AppState,
    target_id: &str,
    mailbox_id: &str,
    through: u64,
) {
    state
        .mailbox_store
        .read_range(target_id, mailbox_id, None, 10)
        .expect("the page should read");
    state
        .mailbox_store
        .acknowledge(target_id, mailbox_id, 0, through)
        .expect("the store cursor should advance");
}

fn install_claude_runtime(
    state: &AppState,
    target_id: &str,
) -> mpsc::Receiver<ClaudeRuntimeCommand> {
    let (runtime, input_rx) = test_claude_runtime_handle(RUNTIME_ID);
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(target_id)
        .expect("target should exist");
    inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    input_rx
}

fn finish_turn(state: &AppState, target_id: &str) {
    state
        .finish_turn_ok_if_runtime_matches(target_id, &RuntimeToken::Claude(RUNTIME_ID.to_owned()))
        .expect("the active turn should finish");
}

fn stored_state(state: &AppState, target_id: &str, message_id: &str) -> String {
    state
        .mailbox_store
        .read_message(target_id, message_id)
        .expect("the message should read")
        .notification_state
}

fn queued_mailbox_wakes(state: &AppState, target_id: &str) -> Vec<(String, u64, String)> {
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = &inner.sessions[inner.find_session_index(target_id).unwrap()];
    record
        .queued_prompts
        .iter()
        .filter_map(|queued| {
            let mailbox = queued.pending_prompt.source.as_ref()?.mailbox.as_ref()?;
            Some((
                queued.pending_prompt.id.clone(),
                mailbox.sequence,
                queued.pending_prompt.text.clone(),
            ))
        })
        .collect()
}

fn session_status(state: &AppState, target_id: &str) -> SessionStatus {
    let inner = state.inner.lock().expect("state mutex poisoned");
    inner.sessions[inner.find_session_index(target_id).unwrap()]
        .session
        .status
}

fn expect_prompt(input_rx: &mpsc::Receiver<ClaudeRuntimeCommand>, context: &str) -> String {
    match input_rx.try_recv() {
        Ok(ClaudeRuntimeCommand::Prompt(command)) => command.text,
        Ok(_) => panic!("{context}: expected a prompt command"),
        Err(error) => panic!("{context}: no turn was handed to the runtime ({error})"),
    }
}

fn expect_no_turn(input_rx: &mpsc::Receiver<ClaudeRuntimeCommand>, context: &str) {
    assert!(
        matches!(input_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "{context}: no turn may be handed to the runtime"
    );
}

/// The continuation text names the mailbox and boundary, says the mail was
/// already acknowledged, never claims unread mail or repeats a body, and does
/// not ask for an acknowledgement-only reply.
pub(super) fn assert_continuation_text(text: &str, mailbox_id: &str, through: u64) {
    assert!(text.contains(mailbox_id), "{text}");
    assert!(text.contains(&format!("#{through}")), "{text}");
    assert!(text.contains("already acknowledged"), "{text}");
    assert!(
        text.contains("Do not send a reply only to acknowledge it."),
        "{text}"
    );
    assert!(
        !text.contains("unread message(s)"),
        "must not claim unread mail: {text}"
    );
    assert!(
        !text.contains("stays in the mailbox"),
        "wakes stay metadata-only: {text}"
    );
}

// Criteria 1 and 2: the diagnosed incident sequence. A message arrives
// mid-turn, the agent reads and acknowledges it in that turn, the turn ends
// normally. On master the acknowledgement deleted the wake and no turn
// followed.
#[tokio::test]
async fn acknowledged_mid_turn_mail_starts_one_turn_after_the_turn_ends() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let input_rx = install_claude_runtime(&state, &target_id);
    let receipt = send(&state, &sender_id, &target_id, "mid-turn-1");
    assert_eq!(receipt.notification_disposition, "queuedBehindActiveTurn");

    read_and_acknowledge(&state, &target_id, &receipt.mailbox_id).await;
    let wakes = queued_mailbox_wakes(&state, &target_id);
    assert_eq!(
        wakes.len(),
        1,
        "acknowledging the data must not cancel the accepted wake"
    );
    assert_continuation_text(&wakes[0].2, &receipt.mailbox_id, receipt.sequence);
    {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&target_id).unwrap()];
        assert_continuation_text(
            &record.session.pending_prompts[0].text,
            &receipt.mailbox_id,
            receipt.sequence,
        );
    }
    assert_eq!(
        stored_state(&state, &target_id, &receipt.message_id),
        "queuedBehindActiveTurn",
        "the queued label is true because the wake still exists"
    );

    finish_turn(&state, &target_id);
    let prompt = expect_prompt(&input_rx, "turn end after an in-turn acknowledgement");
    assert_continuation_text(&prompt, &receipt.mailbox_id, receipt.sequence);
    assert_eq!(session_status(&state, &target_id), SessionStatus::Active);
    assert_eq!(
        stored_state(&state, &target_id, &receipt.message_id),
        "deliveredToIdleSession",
        "only the real handoff marks the message delivered"
    );
    assert!(queued_mailbox_wakes(&state, &target_id).is_empty());

    // The continuation turn ends; nothing is unread and nothing new arrived.
    finish_turn(&state, &target_id);
    expect_no_turn(&input_rx, "the continuation discharged the wake");
    assert_eq!(session_status(&state, &target_id), SessionStatus::Idle);
}

// Positive control: a turn that ends with no mail accepted during it starts
// nothing, so the continuation above is caused by the accepted wake alone.
#[tokio::test]
async fn turn_end_without_mid_turn_mail_starts_no_turn() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let input_rx = install_claude_runtime(&state, &target_id);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&target_id).unwrap();
        inner.sessions[index].session.status = SessionStatus::Idle;
    }
    let receipt = send(&state, &sender_id, &target_id, "idle-delivery");
    assert_eq!(receipt.notification_disposition, "deliveredToIdleSession");
    expect_prompt(&input_rx, "an idle target is woken directly");
    read_and_acknowledge(&state, &target_id, &receipt.mailbox_id).await;
    assert!(queued_mailbox_wakes(&state, &target_id).is_empty());

    finish_turn(&state, &target_id);
    expect_no_turn(&input_rx, "a turn with no newly accepted wake");
    assert_eq!(session_status(&state, &target_id), SessionStatus::Idle);
}

// Criterion 3: several arrivals acknowledged in one turn coalesce into one
// continuation, and the continuation turn acknowledging its own boundary
// again queues nothing further.
#[tokio::test]
async fn acknowledged_arrivals_coalesce_into_one_continuation_turn() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let input_rx = install_claude_runtime(&state, &target_id);
    let first = send(&state, &sender_id, &target_id, "coalesce-1");
    read_and_acknowledge(&state, &target_id, &first.mailbox_id).await;
    let second = send(&state, &sender_id, &target_id, "coalesce-2");
    let receipt = read_and_acknowledge(&state, &target_id, &first.mailbox_id).await;
    assert_eq!(
        queued_mailbox_wakes(&state, &target_id)
            .iter()
            .map(|(_, sequence, _)| *sequence)
            .collect::<Vec<_>>(),
        vec![second.sequence],
        "one wake per mailbox, at the newest accepted boundary"
    );

    finish_turn(&state, &target_id);
    let prompt = expect_prompt(&input_rx, "one coalesced continuation");
    assert_continuation_text(&prompt, &second.mailbox_id, second.sequence);
    expect_no_turn(&input_rx, "exactly one continuation for both arrivals");

    // Inside the continuation turn the agent polls its mailbox again (an empty
    // page) and repeats its acknowledgement through its own boundary: nothing
    // at or below that boundary may queue another wake.
    let base = format!("/api/sessions/{target_id}/mailboxes/{}", first.mailbox_id);
    post(
        &state,
        format!("{base}/read"),
        json!({"issueReceipt": true}),
    )
    .await;
    acknowledge_receipt(&state, &target_id, &first.mailbox_id, &receipt).await;
    assert!(queued_mailbox_wakes(&state, &target_id).is_empty());
    finish_turn(&state, &target_id);
    expect_no_turn(&input_rx, "a wake turn's own boundary queues nothing");
}

// Criterion 3: a message accepted while the continuation turn itself runs is
// beyond that turn's boundary, so it gets its own successor.
#[tokio::test]
async fn arrival_during_the_continuation_turn_gets_its_own_successor() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let input_rx = install_claude_runtime(&state, &target_id);
    let first = send(&state, &sender_id, &target_id, "frozen-1");
    read_and_acknowledge(&state, &target_id, &first.mailbox_id).await;
    finish_turn(&state, &target_id);
    expect_prompt(&input_rx, "continuation for the first arrival");

    let second = send(&state, &sender_id, &target_id, "frozen-2");
    assert_eq!(second.notification_disposition, "queuedBehindActiveTurn");
    assert_eq!(
        stored_state(&state, &target_id, &second.message_id),
        "queuedBehindActiveTurn",
        "the earlier handoff covered only its own boundary"
    );
    read_and_acknowledge(&state, &target_id, &first.mailbox_id).await;
    finish_turn(&state, &target_id);
    let prompt = expect_prompt(&input_rx, "successor for the later arrival");
    assert_continuation_text(&prompt, &second.mailbox_id, second.sequence);
    finish_turn(&state, &target_id);
    expect_no_turn(&input_rx, "both wakes are discharged");
}

// Criterion 3: a user prompt queued ahead of the wake keeps FIFO; the wake
// survives that user turn and starts after it.
#[tokio::test]
async fn user_prompt_ahead_runs_first_and_the_acknowledged_wake_survives_it() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let input_rx = install_claude_runtime(&state, &target_id);
    let queued = state
        .dispatch_turn(
            &target_id,
            SendMessageRequest {
                text: "user prompt queued first".to_owned(),
                expanded_text: None,
                attachments: Vec::new(),
                source_session_id: None,
                source_mailbox: None,
            },
        )
        .expect("the user prompt should queue behind the active turn");
    assert!(matches!(queued, DispatchTurnResult::Queued));
    let receipt = send(&state, &sender_id, &target_id, "behind-user");
    read_and_acknowledge(&state, &target_id, &receipt.mailbox_id).await;

    finish_turn(&state, &target_id);
    assert_eq!(
        expect_prompt(&input_rx, "the user prompt is ahead in FIFO"),
        "user prompt queued first"
    );
    assert_eq!(queued_mailbox_wakes(&state, &target_id).len(), 1);
    finish_turn(&state, &target_id);
    let prompt = expect_prompt(&input_rx, "the wake follows the user turn");
    assert_continuation_text(&prompt, &receipt.mailbox_id, receipt.sequence);
}

// Criterion 3: Stop suspends the acknowledged wake without discharging it;
// an explicit resume starts it.
#[tokio::test]
async fn stop_holds_the_acknowledged_wake_until_an_explicit_resume() {
    let (state, sender_id, target_id) = mailbox_test_state();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&target_id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::None;
        inner.sessions[index].orchestrator_auto_dispatch_blocked = false;
    }
    let receipt = send(&state, &sender_id, &target_id, "before-stop");
    read_and_acknowledge(&state, &target_id, &receipt.mailbox_id).await;
    state.stop_session(&target_id).expect("Stop should settle");
    assert!(
        state
            .dispatch_next_queued_turn(&target_id, false)
            .expect("blocked queue inspection should succeed")
            .is_none(),
        "Stop's pause must hold the wake"
    );
    assert_eq!(queued_mailbox_wakes(&state, &target_id).len(), 1);

    let input_rx = install_claude_runtime(&state, &target_id);
    state
        .resume_session_queue(&target_id)
        .expect("an explicit resume should start the held wake");
    let prompt = expect_prompt(&input_rx, "resume starts the held wake");
    assert_continuation_text(&prompt, &receipt.mailbox_id, receipt.sequence);
    assert_eq!(
        stored_state(&state, &target_id, &receipt.message_id),
        "deliveredToIdleSession"
    );
}

// Criterion 2: mail accepted while the queue is paused reads held; the
// acknowledgement keeps both the wake and that label, and the explicit
// resume's handoff marks it delivered.
#[tokio::test]
async fn an_acknowledged_wake_held_behind_a_paused_queue_stays_held() {
    let (state, sender_id, target_id) = mailbox_test_state();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&target_id).unwrap();
        let record = &mut inner.sessions[index];
        record.session.status = SessionStatus::Idle;
        record.runtime = SessionRuntime::None;
        record.set_auto_dispatch_blocked(true);
    }
    let receipt = send(&state, &sender_id, &target_id, "paused");
    assert_eq!(receipt.notification_disposition, "heldBehindPausedQueue");
    read_and_acknowledge(&state, &target_id, &receipt.mailbox_id).await;
    let wakes = queued_mailbox_wakes(&state, &target_id);
    assert_eq!(
        wakes.len(),
        1,
        "the paused wake survives the acknowledgement"
    );
    assert_continuation_text(&wakes[0].2, &receipt.mailbox_id, receipt.sequence);
    assert_eq!(
        stored_state(&state, &target_id, &receipt.message_id),
        "heldBehindPausedQueue"
    );

    let input_rx = install_claude_runtime(&state, &target_id);
    state.resume_session_queue(&target_id).unwrap();
    let prompt = expect_prompt(&input_rx, "resume starts the held wake");
    assert_continuation_text(&prompt, &receipt.mailbox_id, receipt.sequence);
    assert_eq!(
        stored_state(&state, &target_id, &receipt.message_id),
        "deliveredToIdleSession"
    );
}

// Explicit Cancel still retires the acknowledged wake: no continuation runs.
#[tokio::test]
async fn cancelling_the_acknowledged_wake_starts_no_turn() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let input_rx = install_claude_runtime(&state, &target_id);
    let receipt = send(&state, &sender_id, &target_id, "cancelled");
    read_and_acknowledge(&state, &target_id, &receipt.mailbox_id).await;
    let (prompt_id, _, _) = queued_mailbox_wakes(&state, &target_id)
        .pop()
        .expect("the acknowledged wake should be queued");
    state
        .cancel_queued_prompt(&target_id, &prompt_id)
        .expect("the queued wake should cancel");
    assert!(queued_mailbox_wakes(&state, &target_id).is_empty());

    finish_turn(&state, &target_id);
    expect_no_turn(&input_rx, "a cancelled wake starts nothing");
}

// A retained Engram head is immutable replay input: the acknowledgement
// neither removes it nor rewrites its text. On master the acknowledgement
// sweep removed even a retained head.
#[tokio::test]
async fn acknowledgement_leaves_a_retained_head_untouched() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let receipt = send(&state, &sender_id, &target_id, "retained");
    let original_text = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&target_id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        let head = record
            .queued_prompts
            .front_mut()
            .expect("the wake is queued");
        // The record's retention state, not the projected display flag.
        head.engram_waiting = true;
        assert!(head.is_engram_retained());
        head.pending_prompt.text.clone()
    };
    read_and_acknowledge(&state, &target_id, &receipt.mailbox_id).await;
    let wakes = queued_mailbox_wakes(&state, &target_id);
    assert_eq!(
        wakes.len(),
        1,
        "a retained head is never removed by an acknowledgement"
    );
    assert_eq!(
        wakes[0].2, original_text,
        "a retained head's text is immutable"
    );
}

// Revalidation's own fallback: when the cursor moved through the store alone
// (no acknowledgement-time refresh), the drain still keeps the wake and hands
// off the continuation text, not the stale unread text.
#[tokio::test]
async fn a_cursor_moved_without_the_refresh_still_starts_the_continuation() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let input_rx = install_claude_runtime(&state, &target_id);
    let receipt = send(&state, &sender_id, &target_id, "store-only");
    acknowledge_through_store_only(&state, &target_id, &receipt.mailbox_id, receipt.sequence);
    assert!(
        queued_mailbox_wakes(&state, &target_id)[0]
            .2
            .contains("unread message(s)"),
        "without the refresh the queued text is still the unread wake"
    );

    finish_turn(&state, &target_id);
    let prompt = expect_prompt(&input_rx, "revalidation keeps the acknowledged wake");
    assert_continuation_text(&prompt, &receipt.mailbox_id, receipt.sequence);
}

// A zero-unread head whose boundary row is no longer visible to the receiver
// (here the receiver has left the mailbox) is dropped, not run.
#[tokio::test]
async fn a_wake_whose_boundary_row_is_no_longer_visible_is_dropped() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let input_rx = install_claude_runtime(&state, &target_id);
    let receipt = send(&state, &sender_id, &target_id, "invisible");
    acknowledge_through_store_only(&state, &target_id, &receipt.mailbox_id, receipt.sequence);
    state
        .mailbox_store
        .mark_session_left(&target_id)
        .expect("the receiver should leave the mailbox");

    finish_turn(&state, &target_id);
    expect_no_turn(&input_rx, "an invisible boundary row starts nothing");
    assert!(queued_mailbox_wakes(&state, &target_id).is_empty());
}

// The optimistic revalidation read must not write its older snapshot back
// over a head an acknowledgement rewrote in the meantime. The acknowledgement
// keeps the head's identity and boundary, so only the text and source show
// the change. The interleaving is driven step by step, without timing.
#[tokio::test]
async fn revalidation_never_restores_unread_text_over_a_concurrent_acknowledgement() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let input_rx = install_claude_runtime(&state, &target_id);
    let receipt = send(&state, &sender_id, &target_id, "race");

    // Revalidation snapshots the head and reads one unread message off-lock.
    let read = state
        .read_front_mailbox_revalidation(&target_id)
        .expect("the off-lock read should succeed")
        .expect("the head is a mailbox wake");
    assert_eq!(
        read.wakeup.as_ref().map(|wakeup| wakeup.unread_count),
        Some(1)
    );

    // Before it reacquires the state lock, the acknowledgement commits and
    // rewrites the same head as the continuation.
    read_and_acknowledge(&state, &target_id, &receipt.mailbox_id).await;
    let (prompt_id, sequence, _) = queued_mailbox_wakes(&state, &target_id)[0].clone();
    assert_eq!(
        prompt_id, read.head.prompt_id,
        "the head keeps its identity"
    );
    assert_eq!(sequence, read.head.sequence, "and its boundary");

    assert!(
        state
            .apply_front_mailbox_revalidation(&target_id, read)
            .expect("applying the stale read should succeed"),
        "a changed head must be inspected again"
    );
    let wakes = queued_mailbox_wakes(&state, &target_id);
    assert_eq!(wakes.len(), 1);
    assert_continuation_text(&wakes[0].2, &receipt.mailbox_id, receipt.sequence);

    finish_turn(&state, &target_id);
    let prompt = expect_prompt(&input_rx, "the continuation, not the stale unread wake");
    assert_continuation_text(&prompt, &receipt.mailbox_id, receipt.sequence);
}

// Wakes coalesce per mailbox, so mail from two peers acknowledged in one turn
// keeps one continuation per mailbox, in FIFO order. This pins the chosen
// per-mailbox behaviour.
#[tokio::test]
async fn acknowledged_mail_from_two_mailboxes_keeps_one_continuation_each() {
    let (state, sender_id, target_id) = mailbox_test_state();
    let other_sender_id = test_session_id(&state, Agent::Codex);
    let input_rx = install_claude_runtime(&state, &target_id);
    let first = send(&state, &sender_id, &target_id, "two-mailboxes-1");
    let second = send(&state, &other_sender_id, &target_id, "two-mailboxes-2");
    assert_ne!(first.mailbox_id, second.mailbox_id);
    read_and_acknowledge(&state, &target_id, &first.mailbox_id).await;
    read_and_acknowledge(&state, &target_id, &second.mailbox_id).await;
    assert_eq!(queued_mailbox_wakes(&state, &target_id).len(), 2);

    finish_turn(&state, &target_id);
    let prompt = expect_prompt(&input_rx, "the first mailbox's continuation");
    assert_continuation_text(&prompt, &first.mailbox_id, first.sequence);
    finish_turn(&state, &target_id);
    let prompt = expect_prompt(&input_rx, "the second mailbox's continuation");
    assert_continuation_text(&prompt, &second.mailbox_id, second.sequence);
    finish_turn(&state, &target_id);
    expect_no_turn(&input_rx, "one continuation per mailbox");
}
