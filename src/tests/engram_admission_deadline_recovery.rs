//! Ordinary mailbox custody and truthful Resume responses, without admission RPCs.
use super::*;
use crate::tests::engram_host_adapter::uncertain_begin::OrdinaryRoot;
use axum::body::{Body, to_bytes};
use axum::http::Request;
use tower::ServiceExt;

fn mailbox_root() -> (OrdinaryRoot, String) {
    let mut turn = OrdinaryRoot::new("ordinary-mailbox-resume");
    turn.state.mailbox_store = Arc::new(MailboxStore::open(
        &resolve_coordination_persistence_path(turn.state.persistence_path.as_ref()),
    ).unwrap());
    let sender = test_session_id(&turn.state, Agent::Claude);
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    (turn, sender)
}

fn send(turn: &OrdinaryRoot, sender: &str, key: &str) -> MailboxAppendReceipt {
    turn.state.append_mailbox_message_and_notify(sender, SendMailboxMessageRequest {
        target_session_id: turn.session.clone(), message: "body stays in the mailbox".into(),
        idempotency_key: key.into(), topic: Some(key.into()), state_stamp: None, class: Some("routine".into()),
    }).unwrap()
}

async fn resume(turn: &OrdinaryRoot) -> (StatusCode, String) {
    let response = app_router(turn.state.clone()).oneshot(Request::builder().method("POST")
        .uri(format!("/api/sessions/{}/queue/resume", turn.session)).body(Body::empty()).unwrap())
        .await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn ordinary_mailbox_resume_hands_off_once_without_acknowledging_the_body() {
    let (turn, sender) = mailbox_root();
    let receipt = send(&turn, &sender, "ordinary-wake");
    turn.assert_no_prompt();
    let (status, body) = resume(&turn).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let prompt = turn.prompt();
    assert!(prompt.prompt.contains("ordinary-wake"));
    let page = turn.state.mailbox_store.read_range(&turn.session, &receipt.mailbox_id, None, 10).unwrap();
    assert_eq!(page.processed_through, 0, "provider delivery is never a body acknowledgement");
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].id, receipt.message_id);
    assert_eq!(page.messages[0].body, "body stays in the mailbox");
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[tokio::test]
async fn ordinary_resume_removes_a_covered_wake_without_lending_its_owner_to_the_successor() {
    let (turn, sender) = mailbox_root();
    let receipt = send(&turn, &sender, "already-covered-wake");
    let wake = turn.record(|record| record.queued_prompts[0].pending_prompt.id.clone());
    let user = turn.enqueue("actual user successor", QueuedPromptSource::User);
    turn.state.mailbox_store.read_range(&turn.session, &receipt.mailbox_id, None, 10).unwrap();
    turn.state.mailbox_store.acknowledge(&turn.session, &receipt.mailbox_id, 0, receipt.sequence).unwrap();
    turn.state.mailbox_store.mark_notifications_delivered_through(&turn.session, &receipt.mailbox_id, receipt.sequence).unwrap();
    let (status, body) = resume(&turn).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    turn.assert_no_prompt();
    turn.record(|record| {
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(record.queued_prompts[0].pending_prompt.id, user);
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(!record.session.messages.iter().any(|message| message.id() == wake));
        assert!(!record.session.messages.iter().any(|message| message.id() == user));
    });
    let (status, body) = resume(&turn).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let prompt = turn.prompt().prompt;
    assert!(prompt.contains("actual user successor"));
    assert!(!prompt.contains("already-covered-wake"));
    turn.record(|record| {
        assert!(record.queued_prompts.is_empty());
        assert!(!record.session.messages.iter().any(|message| message.id() == wake));
        assert_eq!(record.session.messages.iter().filter(|message| message.id() == user).count(), 1);
    });
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[tokio::test]
async fn ordinary_resume_reports_the_actual_durability_hold_without_starting_anything() {
    let turn = OrdinaryRoot::new("ordinary-resume-durability");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let id = turn.enqueue_durable("must first be durable", false);
    let before = turn.saved();
    for _ in 0..2 {
        let (status, body) = resume(&turn).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("the next queued prompt is still being saved; Resume again in a moment"), "{body}");
        assert_eq!(turn.saved(), before);
    }
    turn.record(|record| assert_eq!(record.queued_prompts[0].pending_prompt.id, id));
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_post_drain_hold_cannot_return_a_success_that_started_nothing() {
    let turn = OrdinaryRoot::new("ordinary-post-drain-hold");
    turn.edit(|record| record.set_auto_dispatch_blocked(true));
    let id = turn.enqueue_durable("held at the second check", false);
    let owner = turn.record(|record| QueuedTurnOwner::capture(record).unwrap());
    let result = turn.state.finish_resume_drain(&turn.session, Some(owner), Some(&id), None);
    let error = match result { Err(error) => error, Ok(_) => panic!("a real durability hold must refuse") };
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, "the next queued prompt is still being saved; Resume again in a moment");
    turn.assert_no_prompt();
    turn.assert_no_control();
}
