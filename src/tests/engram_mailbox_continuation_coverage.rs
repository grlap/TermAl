//! Real mailbox read/receipt coverage precedes ordinary turn-end queue drain.
//! No checkpoint or control admission is produced.
use super::*;
use super::super::uncertain_begin::OrdinaryRoot;
use super::super::super::mailbox_acknowledged_wake::send;

#[tokio::test]
async fn ordinary_mailbox_coverage_removes_only_an_explicitly_handled_same_turn_wake() {
    let _temp = TestTempRootDirectoryScope::repository_local();
    for explicit in [false, true] {
        let mut turn = OrdinaryRoot::new("ordinary-mailbox-coverage");
        let coordination = resolve_coordination_persistence_path(turn.state.persistence_path.as_ref());
        turn.state.mailbox_store = Arc::new(MailboxStore::open(&coordination).unwrap());
        deliver_turn_dispatch(&turn.state, turn.prepare("Handle current mailbox work")).unwrap();
        turn.prompt();
        let sender = test_session_id(&turn.state, Agent::Claude);
        let message = send(&turn.state, &sender, &turn.session, "fully-handled-mail");
        let path = format!("/api/sessions/{}/mailboxes/{}", turn.session, message.mailbox_id);
        let read = app_router(turn.state.clone()).oneshot(
            Request::builder().method("POST").uri(format!("{path}/read"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"issueReceipt":true}).to_string())).unwrap()
        ).await.unwrap();
        assert_eq!(read.status(), StatusCode::OK);
        let page: Value = serde_json::from_slice(
            &to_bytes(read.into_body(), usize::MAX).await.unwrap()).unwrap();
        assert!(page["receipt"].is_string());
        let acknowledged = app_router(turn.state.clone()).oneshot(
            Request::builder().method("POST").uri(format!("{path}/acknowledge"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"receipt":page["receipt"],"noContinuation":explicit}).to_string())).unwrap()
        ).await.unwrap();
        assert_eq!(acknowledged.status(), StatusCode::OK);
        let token = turn.record(|record| record.runtime.runtime_token().unwrap());
        turn.state.finish_turn_ok_if_runtime_matches(&turn.session, &token).unwrap();
        if explicit {
            turn.record(|record| {
                assert_eq!(record.mailbox_continuations.removed, 1);
                assert!(!record.queued_prompts.iter().any(|entry| entry.source == QueuedPromptSource::Mailbox));
                assert_eq!(record.session.status, SessionStatus::Idle);
            });
        } else {
            let prompt = turn.prompt();
            super::super::super::mailbox_acknowledged_wake::assert_continuation_text(
                &prompt.prompt, &message.mailbox_id, message.sequence);
            turn.record(|record| assert_eq!(record.mailbox_continuations.removed, 0));
        }
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}
