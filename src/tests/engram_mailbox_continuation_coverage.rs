//! Coverage is settled before the real turn-end checkpoint chooses Wait.
//! The control transport and provider are fixtures; no live Engram is used.

use super::super::super::mailbox_acknowledged_wake::send;
use super::*;

#[tokio::test]
async fn mailbox_continuation_coverage_checkpoint_wait_precedes_any_second_admission() {
    let _temp = TestTempRootDirectoryScope::repository_local();
    let (base, session, receiver, transport) = root_fixture([
        bind_reply("root-token"),
        grant_reply("first"),
        begin_reply("first"),
        checkpoint_reply("first"),
    ]);
    let coordination = resolve_coordination_persistence_path(base.persistence_path.as_ref());
    let state = AppState {
        mailbox_store: Arc::new(MailboxStore::open(&coordination).unwrap()),
        ..base
    };
    let sender = test_session_id(&state, Agent::Claude);
    let dispatch = root_dispatch(&state, &session, false);
    deliver_turn_dispatch(&state, dispatch).unwrap();
    assert!(matches!(
        receiver.try_recv().unwrap(),
        CodexRuntimeCommand::Prompt { .. }
    ));
    let message = send(&state, &sender, &session, "checkpoint-fully-handled");
    let path = format!("/api/sessions/{session}/mailboxes/{}", message.mailbox_id);
    let read = app_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("{path}/read"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"issueReceipt": true}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(read.status(), StatusCode::OK);
    let page: Value =
        serde_json::from_slice(&to_bytes(read.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert!(page["receipt"].is_string());
    let acknowledged = app_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("{path}/acknowledge"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"receipt": page["receipt"], "noContinuation": true}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(acknowledged.status(), StatusCode::OK);
    let token = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session).unwrap()]
            .runtime
            .runtime_token()
            .unwrap()
    };
    state
        .finish_turn_ok_if_runtime_matches(&session, &token)
        .unwrap();
    let requests = transport.requests();
    let checkpoint = requests
        .iter()
        .find(|r| r.request["operation"] == "turn_checkpoint")
        .unwrap();
    assert_eq!(
        checkpoint.request["next_intent"], "wait",
        "coverage must precede checkpoint intent selection"
    );
    assert_eq!(
        operations(&transport),
        [
            "session_bind",
            "turn_evaluate",
            "turn_begin",
            "turn_checkpoint"
        ]
    );
    assert!(
        matches!(receiver.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "no redundant provider handoff"
    );
}
