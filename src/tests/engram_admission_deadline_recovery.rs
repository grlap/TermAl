//! Admission failures before Evaluate preserve the original Bind identity.
//! A structured mailbox wake can reconcile automatically; a user head's
//! Resume must expose either real recovery progress or its retained hold.
//! These tests use the actual producer, admission, persistence and drain
//! boundaries with scripted control replies, never a live provider/store.

use super::*;
use axum::body::{Body, to_bytes};
use axum::http::Request;
use tower::ServiceExt;

fn record_value<T>(state: &AppState, session: &str, read: impl FnOnce(&SessionRecord) -> T) -> T {
    let inner = state.inner.lock().unwrap();
    read(&inner.sessions[inner.find_session_index(session).unwrap()])
}

fn reconcile_failure() -> ScriptedEngramControlResponse {
    ScriptedEngramControlResponse::Reply(Err(EngramTransportError::remote(
        EngramControlErrorBody {
            code: "authorization_unknown".to_owned(),
            message: "the original binding needs reconciliation".to_owned(),
        },
    )))
}

fn mailbox_root(
    replies: impl IntoIterator<Item = ScriptedEngramControlResponse>,
) -> (RootFixture, String) {
    let (mut state, session, receiver, transport) = root_fixture(replies);
    state.mailbox_store = Arc::new(
        MailboxStore::open(&resolve_coordination_persistence_path(
            state.persistence_path.as_ref(),
        ))
        .unwrap(),
    );
    let sender = test_session_id(&state, Agent::Claude);
    ((state, session, receiver, transport), sender)
}

fn assert_unread(state: &AppState, session: &str, receipt: &MailboxAppendReceipt) {
    let page = state
        .mailbox_store
        .read_range(session, &receipt.mailbox_id, None, 10)
        .unwrap();
    assert_eq!(
        page.processed_through, 0,
        "recovery is not a mailbox acknowledgement"
    );
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].id, receipt.message_id);
    assert!(page.messages[0].body.contains("stays in the mailbox"));
}

fn due_ticks(state: &AppState, session: &str) {
    abort_retry::await_settlement_acknowledgement(state, session);
    state.engram_budget_clock().advance(Duration::from_secs(3));
    for offset in [10, 20, 30] {
        state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::seconds(offset));
    }
}

#[test]
fn mailbox_remote_reconcile_replays_original_bind_without_operator_action() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let ((state, session, receiver, transport), sender) = mailbox_root([
        reconcile_failure(),
        bind_reply("reconciled-token"),
        grant_reply("reconciled-grant"),
        begin_reply("reconciled-grant"),
    ]);
    let receipt = crate::tests::mailbox_acknowledged_wake::send(
        &state,
        &sender,
        &session,
        "reconcile-original-bind",
    );
    assert_unread(&state, &session, &receipt);
    assert!(receiver.try_recv().is_err());
    assert_eq!(operations(&transport), ["session_bind"]);
    let original = record_value(&state, &session, |record| {
        let head = &record.queued_prompts[0];
        assert!(head.engram_bind.is_some());
        assert!(head.engram_evaluate.is_none());
        assert!(record.engram.active_grant_id.is_none());
        assert!(record.engram.uncertain_grant_id.is_none());
        head.pending_prompt.id.clone()
    });

    // No Resume, Cancel, toggle, new mail or restart; only acknowledged
    // settlement and the existing scheduler may continue this exact head.
    due_ticks(&state, &session);
    assert!(
        matches!(receiver.try_recv(), Ok(CodexRuntimeCommand::Prompt { .. })),
        "a pure mailbox Bind Reconcile must recover automatically"
    );
    assert!(receiver.try_recv().is_err(), "one provider handoff");
    assert_unread(&state, &session, &receipt);
    let requests = transport.requests();
    assert_eq!(
        requests[0].request, requests[1].request,
        "same Bind payload and key"
    );
    assert_eq!(
        operations(&transport),
        [
            "session_bind",
            "session_bind",
            "turn_evaluate",
            "turn_begin"
        ]
    );
    record_value(&state, &session, |record| {
        assert!(record.queued_prompts.is_empty());
        assert!(!record.orchestrator_auto_dispatch_blocked);
        assert_eq!(
            record
                .session
                .messages
                .iter()
                .filter(|message| message.id() == original)
                .count(),
            1
        );
    });
    state
        .reconcile_never_woken_mailbox_notifications_for_session(&session)
        .unwrap();
    due_ticks(&state, &session);
    assert!(
        receiver.try_recv().is_err(),
        "accepted wake cannot be delivered twice"
    );
    assert_eq!(transport.requests().len(), 4);
}

#[tokio::test]
async fn user_remote_reconcile_resume_refuses_without_replacing_its_bind() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (state, session, receiver, transport) = root_fixture([
        reconcile_failure(),
        reconcile_failure(),
        reconcile_failure(),
    ]);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    let original_bind = transport.requests()[0].request.clone();
    let original = record_value(&state, &session, |record| {
        assert!(record.queued_prompts[0].engram_interrupted);
        assert!(record.orchestrator_auto_dispatch_blocked);
        serde_json::to_value(&record.queued_prompts[0]).unwrap()
    });
    due_ticks(&state, &session);
    assert_eq!(
        operations(&transport),
        ["session_bind"],
        "user content is not an automatic mailbox retry"
    );
    for _ in 0..2 {
        let response = app_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/sessions/{session}/queue/resume"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "retained user work must refuse: {body}"
        );
        assert!(
            body.contains("retained") && body.contains("cancel"),
            "actionable hold: {body}"
        );
        assert_eq!(
            record_value(&state, &session, |r| serde_json::to_value(
                &r.queued_prompts[0]
            )
            .unwrap()),
            original
        );
        assert!(record_value(&state, &session, |r| r.orchestrator_auto_dispatch_blocked));
        assert!(receiver.try_recv().is_err());
        // Resume may reconcile, but cannot replace the operation or claim
        // delivery from an unknown Bind result. Every explicit replay must
        // keep the complete original request, including its key.
        assert!(
            transport
                .requests()
                .iter()
                .all(|request| request.request == original_bind)
        );
    }
}

#[tokio::test]
async fn stale_mailbox_head_resume_refuses_for_user_head_actually_admitted() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let ((state, session, receiver, transport), sender) = mailbox_root([reconcile_failure()]);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].set_auto_dispatch_blocked(true);
    }
    let receipt = crate::tests::mailbox_acknowledged_wake::send(
        &state,
        &sender,
        &session,
        "covered-wake-before-user-resume",
    );
    queue_test_engram_prompt(
        &state,
        &session,
        "user work after the covered mailbox wake",
        QueuedPromptSource::User,
        None,
    );
    let (wake_id, user_id) = record_value(&state, &session, |record| {
        assert_eq!(record.queued_prompts.len(), 2);
        (
            record.queued_prompts[0].pending_prompt.id.clone(),
            record.queued_prompts[1].pending_prompt.id.clone(),
        )
    });
    // The defensive queue-drain branch also covers a stale queued wake over
    // a boundary whose handoff and cursor already settled. Write those store
    // facts without refreshing the queue, so Resume itself revalidates A and
    // admits B. Acknowledgement alone would still owe a continuation.
    state
        .mailbox_store
        .read_range(&session, &receipt.mailbox_id, None, 10)
        .unwrap();
    state
        .mailbox_store
        .acknowledge(&session, &receipt.mailbox_id, 0, receipt.sequence)
        .unwrap();
    state
        .mailbox_store
        .mark_notifications_delivered_through(&session, &receipt.mailbox_id, receipt.sequence)
        .unwrap();
    assert!(transport.requests().is_empty());

    let response = app_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{session}/queue/resume"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert_eq!(operations(&transport), ["session_bind"]);
    let original_bind = transport.requests()[0].request.clone();
    record_value(&state, &session, |record| {
        assert_eq!(record.queued_prompts.len(), 1);
        let head = &record.queued_prompts[0];
        assert_ne!(head.pending_prompt.id, wake_id);
        assert_eq!(head.pending_prompt.id, user_id);
        assert!(head.engram_interrupted);
        assert!(record.orchestrator_auto_dispatch_blocked);
        let bind = head
            .engram_bind
            .as_ref()
            .expect("the actual user's Bind stays retained");
        assert_eq!(serde_json::to_value(&bind.request).unwrap(), original_bind);
        assert!(head.engram_evaluate.is_none());
        assert!(record.engram.active_grant_id.is_none());
        assert!(record.engram.uncertain_grant_id.is_none());
    });
    assert!(
        receiver.try_recv().is_err(),
        "unknown Bind cannot hand off user work"
    );
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "the admitted user head is held: {body}"
    );
    assert!(
        body.contains("retained") && body.contains("cancel"),
        "actionable refusal: {body}"
    );
}

#[test]
fn a_mailbox_label_or_embedded_content_does_not_grant_remote_bind_recovery() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for shape in ["unstructured", "user-source", "expanded-content"] {
        let ((state, session, receiver, transport), sender) = mailbox_root([reconcile_failure()]);
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].set_auto_dispatch_blocked(true);
        }
        let receipt =
            crate::tests::mailbox_acknowledged_wake::send(&state, &sender, &session, shape);
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            let head = &mut record.queued_prompts[0];
            match shape {
                "unstructured" => head.pending_prompt.source = None,
                "user-source" => head.source = QueuedPromptSource::User,
                "expanded-content" => {
                    head.pending_prompt.expanded_text =
                        Some("real work carried alongside the wake".to_owned())
                }
                _ => unreachable!(),
            }
            record.set_auto_dispatch_blocked(false);
            sync_pending_prompts(record);
        }
        let started = state
            .start_next_queued_turn_off_lock(&session, false, false)
            .unwrap()
            .unwrap();
        deliver_turn_dispatch(&state, started.dispatch).unwrap();
        let retained = record_value(&state, &session, |r| {
            assert!(r.engram.bind_retry.is_none(), "{shape}");
            serde_json::to_value(&r.queued_prompts[0]).unwrap()
        });
        due_ticks(&state, &session);
        assert_eq!(operations(&transport), ["session_bind"], "{shape}");
        assert!(receiver.try_recv().is_err(), "{shape}");
        assert_eq!(
            record_value(&state, &session, |r| serde_json::to_value(
                &r.queued_prompts[0]
            )
            .unwrap()),
            retained,
            "{shape}"
        );
        assert_unread(&state, &session, &receipt);
    }
}

#[tokio::test]
async fn resume_with_a_durable_bind_retry_returns_success_before_provider_delivery() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (state, session, receiver, transport) = root_fixture([
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
            "binding reply lost during Resume",
        ))),
        bind_reply("scheduled-token"),
        grant_reply("scheduled-grant"),
        begin_reply("scheduled-grant"),
    ]);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].set_auto_dispatch_blocked(true);
    }
    queue_test_engram_prompt(
        &state,
        &session,
        "Resume may schedule this exact user Bind",
        QueuedPromptSource::User,
        None,
    );
    let prompt_id = record_value(&state, &session, |record| {
        record.queued_prompts[0].pending_prompt.id.clone()
    });
    let response = app_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{session}/queue/resume"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(operations(&transport), ["session_bind"]);
    let original_bind = transport.requests()[0].request.clone();
    record_value(&state, &session, |record| {
        assert_eq!(record.queued_prompts[0].pending_prompt.id, prompt_id);
        assert!(record.engram.bind_retry.is_some());
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(!record.queued_prompts[0].engram_interrupted);
    });
    assert!(receiver.try_recv().is_err(), "scheduled is not delivered");
    due_ticks(&state, &session);
    assert!(matches!(
        receiver.try_recv(),
        Ok(CodexRuntimeCommand::Prompt { .. })
    ));
    assert!(
        receiver.try_recv().is_err(),
        "the scheduled retry hands off once"
    );
    assert_eq!(transport.requests()[1].request, original_bind);
    assert_eq!(
        operations(&transport),
        [
            "session_bind",
            "session_bind",
            "turn_evaluate",
            "turn_begin"
        ]
    );
}

#[test]
fn a_mailbox_wake_preserves_explicit_evaluate_refuse_and_defer() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for refuse in [false, true] {
        let decision = if refuse {
            evaluation_refusal_reply("policy_denied")
        } else {
            defer_reply("busy")
        };
        let ((state, session, receiver, transport), sender) =
            mailbox_root([bind_reply("known-decision-token"), decision]);
        let receipt = crate::tests::mailbox_acknowledged_wake::send(
            &state,
            &sender,
            &session,
            "explicit-evaluate-decision",
        );
        due_ticks(&state, &session);
        assert_eq!(operations(&transport), ["session_bind", "turn_evaluate"]);
        assert!(receiver.try_recv().is_err());
        assert!(record_value(&state, &session, |r| r
            .engram
            .bind_retry
            .is_none()));
        assert_unread(&state, &session, &receipt);
    }
}

#[test]
fn mailbox_actual_before_focus_deadline_keeps_unknown_identity_and_one_delivery() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let ((state, session, receiver, transport), sender) = mailbox_root([
        bind_reply("before-focus-token"),
        grant_reply("before-focus-grant"),
        begin_reply("before-focus-grant"),
    ]);
    // The real admission budget guard expires before work focus/Bind. It
    // cannot certify what an earlier invocation with this identity did.
    state.inner.lock().unwrap().test_engram_dispatch_budget = Some(Duration::ZERO);
    let receipt = crate::tests::mailbox_acknowledged_wake::send(
        &state,
        &sender,
        &session,
        "before-focus-deadline",
    );
    state.inner.lock().unwrap().test_engram_dispatch_budget = None;
    assert!(receiver.try_recv().is_err());
    assert!(transport.requests().is_empty());
    let card = record_value(&state, &session, |record| {
        record
            .session
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::EngramControl { card, .. } => Some(serde_json::to_value(card).unwrap()),
                _ => None,
            })
            .unwrap()
    });
    assert_eq!(card["refusalCode"], "deadline_exceeded");
    assert_eq!(card["causalFailure"]["operation"], "admission_binding");
    assert_eq!(card["causalFailure"]["remoteApplication"], "unknown");
    assert_unread(&state, &session, &receipt);
    due_ticks(&state, &session);
    assert!(matches!(
        receiver.try_recv(),
        Ok(CodexRuntimeCommand::Prompt { .. })
    ));
    assert!(receiver.try_recv().is_err());
    assert_eq!(
        operations(&transport),
        ["session_bind", "turn_evaluate", "turn_begin"]
    );
    assert_unread(&state, &session, &receipt);
    due_ticks(&state, &session);
    assert!(receiver.try_recv().is_err());
    assert_eq!(transport.requests().len(), 3);
}

#[test]
fn deadline_origin_user_resume_reconciles_without_a_settings_toggle() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (state, session, receiver, transport) = root_fixture([
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
            "binding reply lost",
        ))),
        bind_reply("deadline-token"),
        grant_reply("deadline-grant"),
        begin_reply("deadline-grant"),
    ]);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    assert!(receiver.try_recv().is_err());
    let original = transport.requests()[0].request.clone();
    abort_retry::await_settlement_acknowledgement(&state, &session);
    elapse_bind_backoff(&state, &session);
    state
        .resume_session_queue(&session)
        .expect("Resume reconciles the original deadline without a toggle");
    assert!(matches!(
        receiver.try_recv(),
        Ok(CodexRuntimeCommand::Prompt { .. })
    ));
    assert!(receiver.try_recv().is_err());
    assert_eq!(transport.requests()[1].request, original);
    assert_eq!(
        operations(&transport),
        [
            "session_bind",
            "session_bind",
            "turn_evaluate",
            "turn_begin"
        ]
    );
    assert!(record_value(&state, &session, |r| r
        .queued_prompts
        .is_empty()
        && !r.orchestrator_auto_dispatch_blocked));
    due_ticks(&state, &session);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn post_drain_same_head_reconcile_hold_cannot_be_reported_as_success() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (state, session, receiver, transport) = root_fixture([reconcile_failure()]);
    let dispatch = root_dispatch(&state, &session, false);
    // This is Resume's checked identity before the first drain completes.
    let checked = record_value(&state, &session, |r| {
        r.queued_prompts[0].pending_prompt.id.clone()
    });
    deliver_turn_dispatch(&state, dispatch).unwrap();
    let original = record_value(&state, &session, |r| {
        serde_json::to_value(&r.queued_prompts[0]).unwrap()
    });
    let error = state
        .finish_resume_drain(&session, None, Some(&checked), None)
        .err()
        .expect("the same newly retained head must not return silent success");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("retained") && error.message.contains("cancel"));
    assert_eq!(
        record_value(&state, &session, |r| serde_json::to_value(
            &r.queued_prompts[0]
        )
        .unwrap()),
        original
    );
    assert!(receiver.try_recv().is_err());
    assert_eq!(operations(&transport), ["session_bind"]);
}
