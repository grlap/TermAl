//! Explicit coverage of a queued mailbox continuation, through production
//! read/acknowledge routes and synchronous fixture runtime handoffs. A plain
//! acknowledgement remains a data-cursor operation. No live providers run.

use super::*;

fn user_request(text: &str) -> SendMessageRequest {
    SendMessageRequest {
        text: text.to_owned(),
        expanded_text: None,
        attachments: Vec::new(),
        source_session_id: None,
        source_mailbox: None,
    }
}

fn active_fixture() -> (
    AppState,
    String,
    String,
    mpsc::Receiver<ClaudeRuntimeCommand>,
) {
    let (state, sender, target) = mailbox_test_state();
    let receiver = install_claude_runtime(&state, &target);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&target).unwrap();
        inner.sessions[index].session.status = SessionStatus::Idle;
    }
    let dispatch = match state
        .dispatch_turn(&target, user_request("first real turn"))
        .unwrap()
    {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("the idle fixture must dispatch"),
    };
    deliver_turn_dispatch(&state, dispatch).unwrap();
    assert_eq!(expect_prompt(&receiver, "first turn"), "first real turn");
    assert_eq!(generation(&state, &target), 1);
    (state, sender, target, receiver)
}

fn generation(state: &AppState, target: &str) -> u64 {
    let inner = state.inner.lock().unwrap();
    inner.sessions[inner.find_session_index(target).unwrap()].active_turn_generation
}

fn queue_user(state: &AppState, target: &str, text: &str) {
    assert!(matches!(
        state.dispatch_turn(target, user_request(text)).unwrap(),
        DispatchTurnResult::Queued
    ));
}

async fn read_page(state: &AppState, target: &str, mailbox: &str, after: Option<u64>) -> Value {
    let mut body = json!({"issueReceipt": true});
    if let Some(after) = after {
        body["afterSequence"] = json!(after);
    }
    let page = post(
        state,
        format!("/api/sessions/{target}/mailboxes/{mailbox}/read"),
        body,
    )
    .await;
    assert!(page["receipt"].is_string(), "must issue a page: {page}");
    page
}

async fn ack(state: &AppState, target: &str, mailbox: &str, receipt: &Value, explicit: bool) {
    let summary = post(
        state,
        format!("/api/sessions/{target}/mailboxes/{mailbox}/acknowledge"),
        json!({"receipt": receipt, "noContinuation": explicit}),
    )
    .await;
    assert_eq!(summary["unreadCount"], 0);
}

async fn post_status(state: &AppState, target: &str, mailbox: &str, body: Value) -> StatusCode {
    app_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/sessions/{target}/mailboxes/{mailbox}/acknowledge"
                ))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn mailbox_continuation_coverage_later_turn_read_and_receipt_ack_remove_wake() {
    let _temp = TestTempRootDirectoryScope::repository_local();
    let (state, sender, target, receiver) = active_fixture();
    queue_user(&state, &target, "later work before the wake");
    let message = send(&state, &sender, &target, "later-turn");
    let accepted = generation(&state, &target);
    finish_turn(&state, &target);
    assert_eq!(
        expect_prompt(&receiver, "user turn first"),
        "later work before the wake"
    );
    assert!(generation(&state, &target) > accepted);
    let page = read_page(&state, &target, &message.mailbox_id, None).await;
    ack(
        &state,
        &target,
        &message.mailbox_id,
        &page["receipt"],
        false,
    )
    .await;
    finish_turn(&state, &target);
    expect_no_turn(
        &receiver,
        "the later turn covered the exact mailbox boundary",
    );
    assert!(queued_mailbox_wakes(&state, &target).is_empty());
    assert_eq!(session_status(&state, &target), SessionStatus::Idle);
    let body = state
        .mailbox_store
        .read_message(&target, &message.message_id)
        .unwrap();
    assert_eq!(body.body, "body of later-turn stays in the mailbox");
}

#[tokio::test]
async fn mailbox_continuation_coverage_reread_stable_receipt_in_later_turn_is_current_proof() {
    let _temp = TestTempRootDirectoryScope::repository_local();
    let (state, sender, target, receiver) = active_fixture();
    queue_user(&state, &target, "later reread");
    let message = send(&state, &sender, &target, "stable-receipt");
    let first = read_page(&state, &target, &message.mailbox_id, None).await;
    finish_turn(&state, &target);
    assert_eq!(
        expect_prompt(&receiver, "later reread turn"),
        "later reread"
    );
    let second = read_page(&state, &target, &message.mailbox_id, None).await;
    assert_eq!(
        first["receipt"], second["receipt"],
        "SQLite receipt reuse is intentional"
    );
    ack(
        &state,
        &target,
        &message.mailbox_id,
        &second["receipt"],
        false,
    )
    .await;
    finish_turn(&state, &target);
    expect_no_turn(
        &receiver,
        "the new read observation, not original issuance, owns coverage",
    );
}

#[tokio::test]
async fn mailbox_continuation_coverage_explicit_same_turn_declaration_removes_wake() {
    let _temp = TestTempRootDirectoryScope::repository_local();
    let (state, sender, target, receiver) = active_fixture();
    let message = send(&state, &sender, &target, "fully-handled");
    let page = read_page(&state, &target, &message.mailbox_id, None).await;
    ack(&state, &target, &message.mailbox_id, &page["receipt"], true).await;
    finish_turn(&state, &target);
    expect_no_turn(
        &receiver,
        "explicit fulfillment leaves no successor obligation",
    );
    assert!(queued_mailbox_wakes(&state, &target).is_empty());
    // Idempotent replay after completion must not manufacture a new proof or wake.
    ack(&state, &target, &message.mailbox_id, &page["receipt"], true).await;
    expect_no_turn(
        &receiver,
        "repeating the successful acknowledgement is harmless",
    );
}

#[tokio::test]
async fn mailbox_continuation_coverage_numeric_ack_cannot_declare_no_continuation() {
    let _temp = TestTempRootDirectoryScope::repository_local();
    let (state, sender, target, receiver) = active_fixture();
    let message = send(&state, &sender, &target, "numeric");
    read_page(&state, &target, &message.mailbox_id, None).await;
    let status = post_status(
        &state,
        &target,
        &message.mailbox_id,
        json!({
            "expectedProcessedThrough": 0, "processedThrough": message.sequence,
            "noContinuation": true
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "numeric ACK cannot express receipt fulfillment"
    );
    let summary = state.mailbox_store.list_for_session(&target).unwrap();
    assert_eq!(
        summary[0].unread_count, 1,
        "invalid declaration cannot commit its cursor"
    );
    finish_turn(&state, &target);
    expect_prompt(&receiver, "refusal leaves the wake owed");
}

#[tokio::test]
async fn mailbox_continuation_coverage_plain_ack_and_split_turn_proofs_keep_the_wake() {
    let _temp = TestTempRootDirectoryScope::repository_local();
    for split_turn in [false, true] {
        let (state, sender, target, receiver) = active_fixture();
        if split_turn {
            queue_user(&state, &target, "ack in another turn");
        }
        let message = send(&state, &sender, &target, "plain-or-split");
        let page = read_page(&state, &target, &message.mailbox_id, None).await;
        if split_turn {
            finish_turn(&state, &target);
            assert_eq!(
                expect_prompt(&receiver, "later user turn"),
                "ack in another turn"
            );
        }
        ack(
            &state,
            &target,
            &message.mailbox_id,
            &page["receipt"],
            false,
        )
        .await;
        finish_turn(&state, &target);
        let prompt = expect_prompt(
            &receiver,
            "ACK alone or read/ACK in different turns owes continuation",
        );
        assert_continuation_text(&prompt, &message.mailbox_id, message.sequence);
        expect_no_turn(&receiver, "one handoff only");
    }
}

#[tokio::test]
async fn mailbox_continuation_coverage_later_message_rearms_after_explicit_declaration() {
    let _temp = TestTempRootDirectoryScope::repository_local();
    let (state, sender, target, receiver) = active_fixture();
    let first = send(&state, &sender, &target, "before-declaration");
    let page = read_page(&state, &target, &first.mailbox_id, None).await;
    ack(&state, &target, &first.mailbox_id, &page["receipt"], true).await;
    let later = send(&state, &sender, &target, "reply-after-declaration");
    assert!(later.sequence > first.sequence);
    // The old declaration cannot be extended to the newer, now acknowledged page.
    read_and_acknowledge(&state, &target, &first.mailbox_id).await;
    finish_turn(&state, &target);
    let prompt = expect_prompt(&receiver, "a later reply requires its own continuation");
    assert_continuation_text(&prompt, &later.mailbox_id, later.sequence);
    expect_no_turn(&receiver, "exactly one later wake");
}

#[tokio::test]
async fn mailbox_continuation_coverage_failed_ack_does_not_declare_fulfillment() {
    let _temp = TestTempRootDirectoryScope::repository_local();
    let (state, sender, target, receiver) = active_fixture();
    let message = send(&state, &sender, &target, "failed-ack");
    read_page(&state, &target, &message.mailbox_id, None).await;
    assert_eq!(
        post_status(
            &state,
            &target,
            &message.mailbox_id,
            json!({"receipt": "not-an-issued-receipt", "noContinuation": true})
        )
        .await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        state
            .mailbox_store
            .connection()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM mailbox_continuation_declarations",
                [],
                |r| r.get::<_, u64>(0)
            )
            .unwrap(),
        0
    );
    read_and_acknowledge(&state, &target, &message.mailbox_id).await;
    finish_turn(&state, &target);
    expect_prompt(&receiver, "a failed explicit ACK grants no proof");
}

#[tokio::test]
async fn mailbox_continuation_coverage_declaration_does_not_cover_another_mailbox() {
    let _temp = TestTempRootDirectoryScope::repository_local();
    let (state, sender, target, receiver) = active_fixture();
    let other = test_session_id(&state, Agent::Codex);
    let first = send(&state, &sender, &target, "covered-mailbox");
    let second = send(&state, &other, &target, "owed-mailbox");
    assert_ne!(first.mailbox_id, second.mailbox_id);
    let page = read_page(&state, &target, &first.mailbox_id, None).await;
    ack(&state, &target, &first.mailbox_id, &page["receipt"], true).await;
    read_and_acknowledge(&state, &target, &second.mailbox_id).await;
    finish_turn(&state, &target);
    let prompt = expect_prompt(&receiver, "only the other mailbox's continuation is owed");
    assert_continuation_text(&prompt, &second.mailbox_id, second.sequence);
    finish_turn(&state, &target);
    expect_no_turn(&receiver, "one mailbox declaration cannot affect another");
}

#[tokio::test]
async fn mailbox_continuation_coverage_retained_or_paused_head_is_not_removed() {
    let _temp = TestTempRootDirectoryScope::repository_local();
    for retained in [false, true] {
        let (state, sender, target, receiver) = active_fixture();
        let message = send(&state, &sender, &target, "protected-head");
        let original = {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&target).unwrap();
            let record = &mut inner.sessions[index];
            if retained {
                record.queued_prompts.front_mut().unwrap().engram_waiting = true;
            } else {
                record.set_auto_dispatch_blocked(true);
            }
            serde_json::to_value(&record.queued_prompts[0]).unwrap()
        };
        let page = read_page(&state, &target, &message.mailbox_id, None).await;
        ack(&state, &target, &message.mailbox_id, &page["receipt"], true).await;
        assert_eq!(queued_mailbox_wakes(&state, &target).len(), 1);
        if retained {
            let inner = state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&target).unwrap()];
            assert_eq!(
                serde_json::to_value(&record.queued_prompts[0]).unwrap(),
                original,
                "prepared authorization is immutable even with explicit fulfillment"
            );
        }
        expect_no_turn(&receiver, "a declaration is never a provider handoff");
    }
}

#[path = "mailbox_continuation_coverage_edges.rs"]
mod edges;
