// Behaviour of the operator's resume-paused action: what it lists before it
// runs, what it resumes and skips, and that only the explicit action resumes.

use super::mailboxes::mailbox_test_state;
use super::*;

fn wake(target_session_id: &str, key: &str) -> SendMailboxMessageRequest {
    SendMailboxMessageRequest {
        target_session_id: target_session_id.to_owned(),
        message: "A wake for the target's queue.".to_owned(),
        idempotency_key: key.to_owned(),
        topic: None,
        state_stamp: None,
        class: Some("routine".to_owned()),
    }
}

/// A second busy target, so a wake queues instead of starting a runtime.
fn add_target(state: &AppState, name: &str) -> String {
    let target_id = test_session_id(state, Agent::Claude);
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(&target_id).expect("target exists");
    inner.sessions[index].session.name = name.to_owned();
    inner.sessions[index].session.status = SessionStatus::Active;
    target_id
}

/// Starts the queued mailbox wake on the target, then fails that turn, which
/// pauses the queue and restores the wake. Returns the runtime's input.
fn fail_mail_woken_turn(state: &AppState, target_id: &str) -> mpsc::Receiver<ClaudeRuntimeCommand> {
    let (runtime, input_rx) = test_claude_runtime_handle("resume-paused-runtime");
    let token = RuntimeToken::Claude(runtime.runtime_id.clone());
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(target_id).expect("target exists");
        inner.sessions[index].session.status = SessionStatus::Idle;
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }
    let dispatch = state
        .dispatch_next_queued_turn(target_id, false)
        .expect("queue drain succeeds")
        .expect("the mailbox wake dispatches");
    deliver_turn_dispatch(state, dispatch).expect("the wake reaches the runtime");
    assert!(matches!(
        recv_within_guard(&input_rx, "resume paused: the first prompt"),
        Ok(ClaudeRuntimeCommand::Prompt(_))
    ));
    state
        .fail_turn_if_runtime_matches(target_id, &token, "the provider refused the turn")
        .expect("the failure settles");
    input_rx
}

/// An operator's Stop pause, as the Stop paths record it.
fn operator_pause(state: &AppState, target_id: &str) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(target_id).expect("target exists");
    let record = &mut inner.sessions[index];
    record.session.status = SessionStatus::Idle;
    record.pause_auto_dispatch(QueuePauseReason::now(QueuePauseCause::StopRequested));
    record.engram.operator_paused = true;
}

fn is_paused(state: &AppState, session_id: &str) -> bool {
    let inner = state.inner.lock().expect("state mutex poisoned");
    inner.sessions[inner
        .find_session_index(session_id)
        .expect("session exists")]
    .orchestrator_auto_dispatch_blocked
}

fn row<'a>(response: &'a ResumePausedQueuesResponse, id: &str) -> &'a ResumePausedQueueRow {
    response
        .sessions
        .iter()
        .find(|row| row.session_id == id)
        .expect("the paused session is listed")
}

#[test]
fn the_action_resumes_failed_mail_turns_and_skips_the_operators_pause() {
    let (state, sender_id, first_id) = mailbox_test_state();
    let second_id = add_target(&state, "Second");
    let operator_id = add_target(&state, "Operator");
    for (target, key) in [(&first_id, "first"), (&second_id, "second")] {
        state
            .append_mailbox_message_and_notify(&sender_id, wake(target, key))
            .expect("the wake queues");
    }
    let first_rx = fail_mail_woken_turn(&state, &first_id);
    let second_rx = fail_mail_woken_turn(&state, &second_id);
    operator_pause(&state, &operator_id);

    // Before the action, the listing names each paused session, its reason,
    // and whether the action resumes or skips it; the dry run resumes nothing.
    let listing = state.resume_paused_queues(true, None);
    assert!(listing.dry_run);
    for id in [&first_id, &second_id] {
        let listed = row(&listing, id);
        assert_eq!(listed.action, ResumePausedQueueAction::Resume);
        assert_eq!(
            listed
                .queue_pause_reason
                .as_ref()
                .map(|reason| reason.cause),
            Some(QueuePauseCause::TurnFailed)
        );
        assert!(listed.outcome.is_none());
    }
    let skipped = row(&listing, &operator_id);
    assert_eq!(skipped.action, ResumePausedQueueAction::Skip);
    assert_eq!(
        skipped.skip_reason,
        Some(ResumePausedQueueSkipReason::OperatorPaused)
    );
    assert_eq!(
        skipped
            .queue_pause_reason
            .as_ref()
            .map(|reason| reason.cause),
        Some(QueuePauseCause::StopRequested)
    );
    for id in [&first_id, &second_id, &operator_id] {
        assert!(is_paused(&state, id), "a dry run resumes nothing");
    }
    assert!(matches!(
        first_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    // The action resumes the two failed mail turns; their restored wakes are
    // dispatched. The operator's pause stays.
    let result = state.resume_paused_queues(false, None);
    assert!(!result.dry_run);
    for (id, rx) in [(&first_id, &first_rx), (&second_id, &second_rx)] {
        assert_eq!(
            row(&result, id).outcome,
            Some(ResumePausedQueueOutcome::Resumed)
        );
        assert!(!is_paused(&state, id));
        assert!(matches!(
            recv_within_guard(rx, "resume paused: the restored wake"),
            Ok(ClaudeRuntimeCommand::Prompt(_))
        ));
    }
    let skipped = row(&result, &operator_id);
    assert_eq!(skipped.action, ResumePausedQueueAction::Skip);
    assert!(skipped.outcome.is_none());
    assert!(is_paused(&state, &operator_id));
}

#[test]
fn paused_delegation_children_are_counted_not_listed() {
    let (state, hidden_id, target_id) = mailbox_test_state();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&target_id).expect("target exists");
        let record = &mut inner.sessions[index];
        record.session.status = SessionStatus::Idle;
        record.session.parent_delegation_id = Some("delegation-parent".to_owned());
        record.pause_auto_dispatch(QueuePauseReason::now(QueuePauseCause::StopRequested));
        // A hidden session that is no delegation child is neither listed
        // nor counted as one.
        let index = inner
            .find_session_index(&hidden_id)
            .expect("session exists");
        let record = &mut inner.sessions[index];
        record.session.status = SessionStatus::Idle;
        record.hidden = true;
        record.pause_auto_dispatch(QueuePauseReason::now(QueuePauseCause::TurnFailed));
    }
    let listing = state.resume_paused_queues(true, None);
    assert!(
        listing
            .sessions
            .iter()
            .all(|row| row.session_id != target_id && row.session_id != hidden_id)
    );
    assert_eq!(listing.paused_delegation_sessions_not_shown, 1);
}

#[test]
fn a_run_past_its_budget_starts_no_resume_and_says_so() {
    let (state, sender_id, target_id) = mailbox_test_state();
    state
        .append_mailbox_message_and_notify(&sender_id, wake(&target_id, "late"))
        .expect("the wake queues");
    let target_rx = fail_mail_woken_turn(&state, &target_id);

    let result = state.resume_paused_queues_until(false, None, std::time::Instant::now());
    let late = row(&result, &target_id);
    assert_eq!(late.action, ResumePausedQueueAction::Resume);
    assert_eq!(late.outcome, Some(ResumePausedQueueOutcome::NotStarted));
    assert!(is_paused(&state, &target_id));
    assert!(matches!(
        target_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

/// An operator's Stop as `stop_session` records it on the queue.
fn operator_stop(state: &AppState, target_id: &str) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner.find_session_index(target_id).expect("target exists");
    inner.sessions[index].pause_queue_for_operator_stop();
}

fn is_operator_paused(state: &AppState, session_id: &str) -> bool {
    let inner = state.inner.lock().expect("state mutex poisoned");
    inner.sessions[inner
        .find_session_index(session_id)
        .expect("session exists")]
    .engram
    .operator_paused
}

#[test]
fn a_stop_after_the_plan_wins_over_the_planned_resume() {
    let (state, sender_id, target_id) = mailbox_test_state();
    state
        .append_mailbox_message_and_notify(&sender_id, wake(&target_id, "stopped"))
        .expect("the wake queues");
    let target_rx = fail_mail_woken_turn(&state, &target_id);
    let (mut rows, _) = state.paused_queue_plan(None);
    let mut planned = rows
        .iter()
        .position(|row| row.session_id == target_id)
        .map(|index| rows.swap_remove(index))
        .expect("the failed turn is planned");
    assert_eq!(planned.action, ResumePausedQueueAction::Resume);

    // The operator stops the session between the plan and the run.
    operator_stop(&state, &target_id);

    // The per-session step refuses, and the bulk row reports the skip.
    let refused = state
        .resume_local_session_queue_fenced(&target_id, planned.fence)
        .expect_err("a Stop since the plan refuses the fenced Resume");
    assert_eq!(refused.status, StatusCode::CONFLICT);
    state.resume_planned_row(&mut planned);
    assert_eq!(planned.action, ResumePausedQueueAction::Skip);
    assert_eq!(
        planned.skip_reason,
        Some(ResumePausedQueueSkipReason::OperatorPaused)
    );
    assert!(planned.outcome.is_none());
    assert!(is_paused(&state, &target_id));
    assert!(is_operator_paused(&state, &target_id));
    assert!(matches!(
        target_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    // The session's own Resume still lifts the operator's pause.
    state
        .resume_session_queue(&target_id)
        .expect("the session's own Resume succeeds");
    assert!(!is_paused(&state, &target_id));
    assert!(!is_operator_paused(&state, &target_id));
}

/// An operator's Delete that is held until the session's observation is
/// reported, as Delete records it.
fn retained_delete(state: &AppState, target_id: &str) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    retain_source_observation_removal_locked(&mut inner, &[target_id.to_owned()]);
}

fn is_delete_pending(state: &AppState, session_id: &str) -> bool {
    let inner = state.inner.lock().expect("state mutex poisoned");
    inner.sessions[inner
        .find_session_index(session_id)
        .expect("session exists")]
    .engram
    .observation_delete_requested
}

#[test]
fn a_pending_delete_is_skipped_and_kept() {
    let (state, sender_id, target_id) = mailbox_test_state();
    state
        .append_mailbox_message_and_notify(&sender_id, wake(&target_id, "deleted"))
        .expect("the wake queues");
    let target_rx = fail_mail_woken_turn(&state, &target_id);
    retained_delete(&state, &target_id);

    let result = state.resume_paused_queues(false, None);
    let skipped = row(&result, &target_id);
    assert_eq!(skipped.action, ResumePausedQueueAction::Skip);
    assert_eq!(
        skipped.skip_reason,
        Some(ResumePausedQueueSkipReason::DeletePending)
    );
    assert!(skipped.outcome.is_none());
    assert!(is_delete_pending(&state, &target_id));
    assert!(is_paused(&state, &target_id));
    assert!(matches!(
        target_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

#[test]
fn a_delete_after_the_plan_wins_over_the_planned_resume() {
    let (state, sender_id, target_id) = mailbox_test_state();
    state
        .append_mailbox_message_and_notify(&sender_id, wake(&target_id, "deleted-later"))
        .expect("the wake queues");
    let target_rx = fail_mail_woken_turn(&state, &target_id);
    let (mut rows, _) = state.paused_queue_plan(None);
    let mut planned = rows
        .iter()
        .position(|row| row.session_id == target_id)
        .map(|index| rows.swap_remove(index))
        .expect("the failed turn is planned");
    assert_eq!(planned.action, ResumePausedQueueAction::Resume);

    // The operator deletes the session between the plan and the run.
    retained_delete(&state, &target_id);

    state.resume_planned_row(&mut planned);
    assert_eq!(planned.action, ResumePausedQueueAction::Skip);
    assert_eq!(
        planned.skip_reason,
        Some(ResumePausedQueueSkipReason::DeletePending)
    );
    assert!(planned.outcome.is_none());
    assert!(is_delete_pending(&state, &target_id));
    assert!(is_paused(&state, &target_id));
    assert!(matches!(
        target_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

#[test]
fn a_stop_and_the_operators_own_resume_during_the_run_report_the_queue_as_it_is() {
    let (state, sender_id, target_id) = mailbox_test_state();
    state
        .append_mailbox_message_and_notify(&sender_id, wake(&target_id, "resumed-by-hand"))
        .expect("the wake queues");
    let _target_rx = fail_mail_woken_turn(&state, &target_id);
    let (mut rows, _) = state.paused_queue_plan(None);
    let mut planned = rows
        .iter()
        .position(|row| row.session_id == target_id)
        .map(|index| rows.swap_remove(index))
        .expect("the failed turn is planned");

    // The operator stops the session, then resumes it themselves, before
    // the action reaches it.
    operator_stop(&state, &target_id);
    state
        .resume_session_queue(&target_id)
        .expect("the session's own Resume succeeds");

    state.resume_planned_row(&mut planned);
    assert_eq!(planned.action, ResumePausedQueueAction::Resume);
    assert!(planned.skip_reason.is_none());
    assert_eq!(planned.outcome, Some(ResumePausedQueueOutcome::Resumed));
    assert!(!is_paused(&state, &target_id));
}

#[test]
fn a_run_naming_its_sessions_resumes_only_those() {
    let (state, sender_id, first_id) = mailbox_test_state();
    let second_id = add_target(&state, "Second");
    for (target, key) in [(&first_id, "named"), (&second_id, "unnamed")] {
        state
            .append_mailbox_message_and_notify(&sender_id, wake(target, key))
            .expect("the wake queues");
    }
    let first_rx = fail_mail_woken_turn(&state, &first_id);
    let second_rx = fail_mail_woken_turn(&state, &second_id);

    let result = state.resume_paused_queues(false, Some(std::slice::from_ref(&first_id)));
    assert_eq!(
        row(&result, &first_id).outcome,
        Some(ResumePausedQueueOutcome::Resumed)
    );
    assert!(!is_paused(&state, &first_id));
    assert!(matches!(
        recv_within_guard(&first_rx, "resume paused: the named session's wake"),
        Ok(ClaudeRuntimeCommand::Prompt(_))
    ));
    let unnamed = row(&result, &second_id);
    assert_eq!(unnamed.action, ResumePausedQueueAction::Skip);
    assert_eq!(
        unnamed.skip_reason,
        Some(ResumePausedQueueSkipReason::NotPreviewed)
    );
    assert!(unnamed.outcome.is_none());
    assert!(is_paused(&state, &second_id));
    assert!(matches!(
        second_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

fn resume_paused_request(content_type: &str, site: Option<&str>, body: &str) -> Request<Body> {
    let mut request = Request::builder()
        .method("POST")
        .uri("/api/queues/resume-paused")
        .header("content-type", content_type);
    if let Some(site) = site {
        request = request.header("sec-fetch-site", site);
    }
    request.body(Body::from(body.to_owned())).unwrap()
}

/// A browser request without Fetch Metadata, from `origin` to `host`.
fn origin_request(origin: &str, host: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/queues/resume-paused")
        .header("content-type", "application/json")
        .header("origin", origin)
        .header("host", host)
        .body(Body::from(body.to_owned()))
        .unwrap()
}

#[tokio::test]
async fn the_route_resumes_only_on_an_explicit_same_origin_json_request() {
    let (state, _sender_id, target_id) = mailbox_test_state();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&target_id).expect("target exists");
        let record = &mut inner.sessions[index];
        record.session.status = SessionStatus::Idle;
        record.pause_auto_dispatch(QueuePauseReason::now(QueuePauseCause::TurnFailed));
    }
    let app = app_router(state.clone());
    let run = r#"{"dryRun":false}"#;
    for (request, expected) in [
        // A form or text body, which a browser on another site may send
        // without a preflight.
        (
            resume_paused_request("text/plain", None, run),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            resume_paused_request("application/json", Some("cross-site"), run),
            StatusCode::FORBIDDEN,
        ),
        (
            resume_paused_request("application/json", Some("same-site"), run),
            StatusCode::FORBIDDEN,
        ),
        // Without Fetch Metadata, another origin that CORS admits is still
        // another origin.
        (
            origin_request("http://localhost:8787", "127.0.0.1:8787", run),
            StatusCode::FORBIDDEN,
        ),
        (
            origin_request("null", "127.0.0.1:8787", run),
            StatusCode::FORBIDDEN,
        ),
        // Leaving dryRun out does not run the action.
        (
            resume_paused_request("application/json", None, "{}"),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            resume_paused_request("application/json", None, ""),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let (status, _): (StatusCode, Value) = request_json(&app, request).await;
        assert_eq!(status, expected);
        assert!(
            is_paused(&state, &target_id),
            "a refused request resumes nothing"
        );
    }

    let (status, listing): (StatusCode, Value) = request_json(
        &app,
        resume_paused_request(
            "application/json",
            Some("same-origin"),
            r#"{"dryRun":true}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listing["sessions"][0]["sessionId"], target_id.as_str());
    assert_eq!(listing["sessions"][0]["action"], "resume");
    assert!(is_paused(&state, &target_id));

    // Without Fetch Metadata, a request from the origin it was sent to.
    let (status, _): (StatusCode, Value) = request_json(
        &app,
        origin_request(
            "http://127.0.0.1:8787",
            "127.0.0.1:8787",
            r#"{"dryRun":true}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(is_paused(&state, &target_id));

    // The CLI's request carries no browser header and runs the action.
    let (status, result): (StatusCode, Value) =
        request_json(&app, resume_paused_request("application/json", None, run)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["sessions"][0]["outcome"], "resumed");
    assert!(!is_paused(&state, &target_id));
}
