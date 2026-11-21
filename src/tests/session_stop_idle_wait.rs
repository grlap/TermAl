//! Stop on an Idle session that is waiting for automatic work.
//!
//! A root session that registers a delegation wait or a test-run wait and then
//! ends its turn is Idle until the awaited work resumes it. The public Stop
//! route is the operator's brake on that resume: it preserves delegation
//! waits while consuming test-run waits, leaves the session Idle, and starts
//! nothing until an explicit Resume or a new user prompt. Idle with no pending
//! wait still has nothing to stop.
//!
//! Owns these route-level tests only. It does not own Stop of an Active
//! session (`session_stop.rs`, `session_stop_runtime.rs`,
//! `delegation_wait.rs`) or run-wait settling (`test_run_waits.rs`). New
//! module; not split from an existing file.

use super::delegation_support::{
    finish_delegation_child_with_assistant_text, structured_review_request,
    test_app_state_with_structured_reviewer_runtime,
};
use super::*;

async fn post_stop(state: &AppState, session_id: &str) -> (StatusCode, Value) {
    let app = app_router(state.clone());
    tokio::time::timeout(
        super::phase_sync::DEADLOCK_GUARD,
        request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{session_id}/stop"))
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await
    .expect("Stop route should answer")
}

fn user_prompt(text: &str) -> SendMessageRequest {
    SendMessageRequest {
        text: text.to_owned(),
        expanded_text: None,
        attachments: Vec::new(),
        source_session_id: None,
        source_mailbox: None,
    }
}

fn session_record(state: &AppState, session_id: &str) -> SessionRecord {
    let inner = state.inner.lock().expect("state mutex poisoned");
    inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("session should exist")
        .clone()
}

fn wire_session<'a>(response: &'a Value, session_id: &str) -> &'a Value {
    response["sessions"]
        .as_array()
        .expect("Stop should answer with a state snapshot")
        .iter()
        .find(|session| session["id"] == session_id)
        .expect("stopped session should stay visible")
}

#[tokio::test]
async fn stop_on_an_idle_parent_holds_a_delegation_result_until_resume() {
    let (mut state, input_rx) =
        test_app_state_with_structured_reviewer_runtime("idle-wait-stop-delegation");
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review work the parent waits for while Idle.".to_owned(),
                title: Some("Idle-parent review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    match recv_within_guard(&input_rx, "delegation child prompt should be delivered")
        .expect("delegation child prompt should be delivered")
    {
        CodexRuntimeCommand::Prompt { session_id, .. } => {
            assert_eq!(session_id, created.delegation.child_session_id);
        }
        _ => panic!("delegation should dispatch the child review prompt"),
    }
    let wait = state
        .create_delegation_wait(
            &parent_session_id,
            CreateDelegationWaitRequest {
                delegation_ids: vec![created.delegation.id.clone()],
                mode: DelegationWaitMode::All,
                title: Some("Idle parent fan-in".to_owned()),
            },
        )
        .expect("wait should be scheduled");
    assert_eq!(
        session_record(&state, &parent_session_id).session.status,
        SessionStatus::Idle,
        "the parent ended its turn and waits Idle"
    );

    let (status, response) = post_stop(&state, &parent_session_id).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "Stop on a waiting Idle parent: {response}"
    );
    let stopped = wire_session(&response, &parent_session_id);
    assert_eq!(stopped["status"], "idle");
    assert_eq!(stopped["queuePaused"], true);
    assert!(
        state
            .snapshot()
            .delegation_waits
            .iter()
            .any(|pending| pending.id == wait.wait.id),
        "Stop keeps the original wait registered"
    );
    let record = session_record(&state, &parent_session_id);
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(record.engram.operator_paused);
    assert!(matches!(record.runtime, SessionRuntime::None));
    let (status, response) = post_stop(&state, &parent_session_id).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(
        session_record(&state, &parent_session_id)
            .session
            .messages
            .len(),
        record.session.messages.len(),
        "repeating Stop while the wait is held adds no stopped message"
    );

    let mut result = structured_review_request();
    result.summary = "Late review result.".to_owned();
    state
        .submit_delegation_review_result(&created.delegation.child_session_id, result)
        .expect("reviewer should submit its original result");
    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nLate review result.",
    );
    {
        // Duplicate transcript identities isolate this parent row. The
        // writer must still commit the completed child and recoverable wait.
        let mut inner = state.inner.lock().unwrap();
        let message = Message::Text {
            attachments: Vec::new(),
            id: inner.next_message_id(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: "unsaved duplicate transcript fixture".to_owned(),
            expanded_text: None,
            source: None,
        };
        let parent = inner.session_mut(&parent_session_id).unwrap();
        parent.session.messages.push(message.clone());
        parent.session.messages.push(message);
    }
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("late child completion should reconcile without resuming the parent");

    assert!(
        matches!(input_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "no fan-in prompt may reach the stopped parent"
    );
    let record = session_record(&state, &parent_session_id);
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(state.snapshot().delegation_waits.is_empty());
    assert_eq!(record.queued_prompts.len(), 1);
    assert!(
        record.queued_prompts[0]
            .pending_prompt
            .text
            .contains(&wait.wait.id)
    );

    let restarted = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let parent = &restarted.sessions[restarted.find_session_index(&parent_session_id).unwrap()];
    assert!(parent.orchestrator_auto_dispatch_blocked);
    assert!(parent.queued_prompts.is_empty());
    assert_eq!(restarted.delegation_waits.as_slice(), &[wait.wait.clone()]);
    assert_eq!(
        restarted.delegations[restarted
            .find_delegation_index(&created.delegation.id)
            .unwrap()]
        .status,
        DelegationStatus::Completed,
    );
    state.inner = Arc::new(StateMutex::new(restarted));
    state.reconcile_delegation_waits_after_boot().unwrap();
    let saved = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(saved.delegation_waits.is_empty());
    let parent = &saved.sessions[saved.find_session_index(&parent_session_id).unwrap()];
    assert_eq!(parent.queued_prompts.len(), 1);
    assert!(matches!(
        input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    state
        .resume_session_queue(&parent_session_id)
        .expect("Resume should deliver the stored fan-in");
    match recv_within_guard(&input_rx, "fan-in should reach the provider")
        .expect("fan-in should reach the provider")
    {
        CodexRuntimeCommand::Prompt {
            session_id,
            command,
        } => {
            assert_eq!(session_id, parent_session_id);
            assert!(command.prompt.contains(&wait.wait.id));
            assert!(command.prompt.contains("Late review result."));
        }
        _ => panic!("expected the paused delegation result"),
    }
    assert!(matches!(
        input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn peer_prompt_before_fan_in_waits_for_explicit_resume() {
    assert_peer_prompt_waits_for_explicit_resume(true).await;
}

#[tokio::test]
async fn peer_prompt_after_fan_in_waits_for_explicit_resume() {
    assert_peer_prompt_waits_for_explicit_resume(false).await;
}

async fn assert_peer_prompt_waits_for_explicit_resume(peer_before_fan_in: bool) {
    const PEER_TEXT: &str = "A peer update for the stopped parent";
    let (state, input_rx) =
        test_app_state_with_structured_reviewer_runtime("idle-stop-peer-prompt");
    let parent = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent,
            CreateDelegationRequest {
                prompt: "Review the change.".to_owned(),
                title: None,
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .unwrap();
    let child = &created.delegation.child_session_id;
    assert!(matches!(
        recv_within_guard(&input_rx, "the child should start"),
        Ok(CodexRuntimeCommand::Prompt { session_id, .. }) if session_id == *child
    ));
    let wait = state
        .create_delegation_wait(
            &parent,
            CreateDelegationWaitRequest {
                delegation_ids: vec![created.delegation.id.clone()],
                mode: DelegationWaitMode::All,
                title: None,
            },
        )
        .unwrap();
    let (status, response) = post_stop(&state, &parent).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(wire_session(&response, &parent)["queuePaused"], true);

    let app = app_router(state.clone());
    let send_peer = || async {
        let (status, response): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{parent}/messages"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "text": PEER_TEXT,
                        "sourceSessionId": child,
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{response}");
        assert_eq!(response["messageDisposition"], "queuedBehindActiveTurn");
        assert_eq!(wire_session(&response, &parent)["queuePaused"], true);
        assert!(matches!(
            input_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    };

    if peer_before_fan_in {
        send_peer().await;
    }
    state
        .submit_delegation_review_result(child, structured_review_request())
        .unwrap();
    finish_delegation_child_with_assistant_text(&state, child, "Review finished.");
    state.refresh_delegation_for_child_session(child).unwrap();
    if !peer_before_fan_in {
        send_peer().await;
    }

    let held = session_record(&state, &parent);
    assert_eq!(held.session.status, SessionStatus::Idle);
    assert!(held.orchestrator_auto_dispatch_blocked);
    assert!(held.engram.operator_paused);
    assert_eq!(held.queued_prompts.len(), 2);
    assert!(matches!(
        input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    let (status, response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{parent}/queue/resume"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let fan_in = wait.wait.id.as_str();
    let order = if peer_before_fan_in {
        [PEER_TEXT, fan_in]
    } else {
        [fan_in, PEER_TEXT]
    };
    for expected in order {
        let CodexRuntimeCommand::Prompt {
            session_id,
            command,
        } = recv_within_guard(&input_rx, "the resumed queue should reach the provider").unwrap()
        else {
            panic!("expected a parent prompt");
        };
        assert_eq!(session_id, parent);
        assert!(command.prompt.contains(expected));
        let running = session_record(&state, &parent);
        state
            .finish_turn_ok_if_runtime_and_generation_match(
                &parent,
                &running.runtime.runtime_token().unwrap(),
                running.active_turn_generation,
            )
            .unwrap();
    }
    let resumed = session_record(&state, &parent);
    assert!(!resumed.orchestrator_auto_dispatch_blocked);
    assert!(resumed.queued_prompts.is_empty());
    assert!(matches!(
        input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn stop_on_an_idle_session_consumes_its_test_run_wait_and_its_result_starts_nothing() {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join("idle-wait-stop-test-run");
    fs::create_dir_all(&root).unwrap();
    run_git_test_command(&root, &["init", "--quiet"]);
    let project_id = create_test_project(&state, &root, "Idle waits");
    let session_id = create_test_project_session(&state, Agent::Claude, &project_id, &root);
    let run_dir = root.join(".git").join("review-runs").join("test-idle-stop");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(
        run_dir.join("request.json"),
        json!({ "runId": "test-idle-stop", "root": root.to_string_lossy(),
            "full": true, "started": "2026-09-26T10:00:00.000Z" })
        .to_string(),
    )
    .unwrap();
    let write_results = |results: Value| {
        let mut results = results;
        results["runId"] = json!("test-idle-stop");
        fs::write(run_dir.join("results.json"), results.to_string()).unwrap();
    };
    let scan = |alive: Vec<u32>| {
        state.refresh_test_runs_with(&move |pid, _| alive.contains(&pid), &|inner, event| {
            state.publish_delta_locked(inner, event.clone())
        });
        state.refresh_test_run_waits();
    };
    write_results(json!({ "state": "running", "pid": 7,
        "stages": [{ "name": "rust-tests", "state": "running" }] }));
    scan(vec![7]);
    state
        .create_test_run_wait_with(
            &session_id,
            serde_json::from_value(json!({ "runIds": ["test-idle-stop"], "mode": "all" })).unwrap(),
            &|| {},
        )
        .expect("run wait should register");
    assert_eq!(
        session_record(&state, &session_id).session.status,
        SessionStatus::Idle,
        "the session ended its turn and waits Idle"
    );
    let mut delta_events = state.subscribe_delta_events();

    let (status, response) = post_stop(&state, &session_id).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "Stop on a waiting Idle session: {response}"
    );
    assert_eq!(wire_session(&response, &session_id)["status"], "idle");
    let mut consumed = Vec::new();
    while let Ok(payload) = delta_events.try_recv() {
        if let Ok(DeltaEvent::TestRunWaitConsumed {
            reason,
            session_id: consumed_session,
            ..
        }) = serde_json::from_str::<DeltaEvent>(&payload)
        {
            consumed.push((reason, consumed_session));
        }
    }
    assert_eq!(
        consumed,
        vec![(
            TestRunWaitConsumedReason::SessionStopped,
            session_id.clone()
        )]
    );
    assert!(state.inner.lock().unwrap().test_run_waits.is_empty());

    write_results(json!({ "state": "passed", "exitCode": 0,
        "ended": "2026-09-26T10:10:00.000Z",
        "stages": [{ "name": "rust-tests", "state": "passed", "code": 0 }] }));
    scan(Vec::new());
    let record = session_record(&state, &session_id);
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(
        record
            .queued_prompts
            .iter()
            .all(|queued| !queued.pending_prompt.text.contains("test-idle-stop")),
        "the settled run may not queue a resume for the stopped session"
    );

    // A new user prompt is itself the explicit resume.
    let (runtime, _runtime_rx) = test_claude_runtime_handle("idle-wait-stop-user-prompt");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
    }
    let dispatched = state
        .dispatch_turn(&session_id, user_prompt("Look at the result."))
        .expect("a user prompt should be accepted");
    assert!(
        matches!(dispatched, DispatchTurnResult::Dispatched(_)),
        "a user prompt lifts the latch and dispatches at once"
    );
    let record = session_record(&state, &session_id);
    assert!(!record.orchestrator_auto_dispatch_blocked);
    assert_eq!(record.session.status, SessionStatus::Active);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn stop_on_an_idle_session_without_a_pending_wait_still_conflicts() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    assert_eq!(
        session_record(&state, &session_id).session.status,
        SessionStatus::Idle
    );

    let (status, response) = post_stop(&state, &session_id).await;

    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    let record = session_record(&state, &session_id);
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(
        !record.orchestrator_auto_dispatch_blocked,
        "a refused Stop pauses nothing"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

fn seeded_delegation_wait(id: &str, parent_session_id: &str) -> DelegationWaitRecord {
    DelegationWaitRecord {
        id: id.to_owned(),
        parent_session_id: parent_session_id.to_owned(),
        delegation_ids: vec![format!("delegation-for-{id}")],
        mode: DelegationWaitMode::All,
        created_at: stamp_now(),
        title: None,
    }
}

fn settle_final_wait_before_dispatch(state: &AppState, session_id: &str) -> String {
    let wait_id = format!("delegation-wait-{}", Uuid::new_v4());
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    inner
        .delegation_waits
        .push(seeded_delegation_wait(&wait_id, session_id));
    let refresh = refresh_delegation_waits_locked(&mut inner);
    assert_eq!(refresh.dispatch_parents, [session_id.to_owned()]);
    assert!(inner.delegation_waits.is_empty());
    state.commit_locked(&mut inner).unwrap();
    wait_id
}

#[tokio::test]
async fn idle_stop_holds_a_result_after_the_final_wait_has_settled() {
    let (state, input_rx) =
        test_app_state_with_structured_reviewer_runtime("idle-stop-after-settle");
    let session_id = test_session_id(&state, Agent::Codex);
    assert!(
        !state
            .summary_snapshot()
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .unwrap()
            .has_runnable_delegation_result
    );
    let wait_id = settle_final_wait_before_dispatch(&state, &session_id);
    let settled = state.summary_snapshot();
    let parent = settled
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .unwrap();
    assert_eq!(parent.status, SessionStatus::Idle);
    assert!(parent.has_runnable_delegation_result);

    let (status, response) = post_stop(&state, &session_id).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(wire_session(&response, &session_id)["queuePaused"], true);
    assert!(
        state
            .dispatch_next_queued_turn(&session_id, false)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    state.resume_session_queue(&session_id).unwrap();
    let CodexRuntimeCommand::Prompt { command, .. } =
        recv_within_guard(&input_rx, "the settled result should run on Resume").unwrap()
    else {
        panic!("expected a delegation result");
    };
    assert!(command.prompt.contains(&wait_id));
    assert!(
        !state
            .summary_snapshot()
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .unwrap()
            .has_runnable_delegation_result
    );
}

#[tokio::test]
async fn stop_between_delegation_promotion_and_provider_enqueue_keeps_the_result() {
    let (state, input_rx) =
        test_app_state_with_structured_reviewer_runtime("stop-before-delegation-enqueue");
    let session_id = test_session_id(&state, Agent::Codex);
    let wait_id = settle_final_wait_before_dispatch(&state, &session_id);
    let started = state
        .start_next_queued_turn_off_lock(&session_id, false, false)
        .unwrap()
        .expect("the wait result should be promoted");
    let prompt_id = started.queued.pending_prompt.id;
    let mut events = state.subscribe_delta_events();
    let stop_gate = install_test_stop_fence_gate(&state, &session_id);

    let (status, response) = post_stop(&state, &session_id).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    stop_gate.wait_until_claimed();
    let outcome = handoff_prepared_turn_dispatch(&state, started.dispatch).unwrap();
    assert!(
        matches!(
            outcome,
            HandoffPreparedTurnDispatchOutcome::Scheduled
                | HandoffPreparedTurnDispatchOutcome::Superseded
        ),
        "the claimed Stop must fence provider enqueue"
    );
    stop_gate.release();
    tokio::time::timeout(super::phase_sync::DEADLOCK_GUARD, async {
        loop {
            let event = events
                .recv()
                .await
                .expect("the Stop must publish its result");
            if matches!(
                serde_json::from_str::<DeltaEvent>(&event).unwrap(),
                DeltaEvent::MessageCreated {
                    session_id: ref stopped_id,
                    message: Message::Text { ref text, .. },
                    ..
                } if stopped_id == &session_id && text == SESSION_STOPPED_BY_USER_MESSAGE
            ) {
                break;
            }
        }
    })
    .await
    .expect("the Stop worker must finish");

    let stopped = session_record(&state, &session_id);
    assert_eq!(stopped.session.status, SessionStatus::Idle);
    assert!(stopped.session.queue_paused);
    assert_eq!(stopped.queued_prompts.len(), 1);
    assert_eq!(stopped.queued_prompts[0].pending_prompt.id, prompt_id);
    assert!(matches!(
        input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    state.resume_session_queue(&session_id).unwrap();
    let CodexRuntimeCommand::Prompt { command, .. } =
        recv_within_guard(&input_rx, "the unsent result should run on Resume").unwrap()
    else {
        panic!("expected a delegation result");
    };
    assert!(command.prompt.contains(&wait_id));
    assert!(matches!(
        input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let resumed = session_record(&state, &session_id);
    assert!(resumed.queued_prompts.is_empty());
    assert_eq!(
        resumed
            .session
            .messages
            .iter()
            .filter(|message| message.id() == prompt_id)
            .count(),
        1
    );
}

#[test]
fn boot_rejection_keeps_one_fan_in_and_resume_reuses_its_transcript() {
    let (state, rejected_rx) =
        test_app_state_with_structured_reviewer_runtime("boot-fan-in-rejected");
    let session_id = test_session_id(&state, Agent::Codex);
    let wait_id = settle_final_wait_before_dispatch(&state, &session_id);
    let prompt_id = session_record(&state, &session_id).queued_prompts[0]
        .pending_prompt
        .id
        .clone();

    drop(rejected_rx);
    state.dispatch_orphaned_workflow_prompts();
    let rejected = session_record(&state, &session_id);
    assert_eq!(rejected.session.status, SessionStatus::Error);
    assert!(rejected.session.queue_paused);
    assert_eq!(rejected.queued_prompts.len(), 1);
    assert_eq!(rejected.queued_prompts[0].pending_prompt.id, prompt_id);

    let (runtime, received, _) = test_shared_codex_runtime("boot-fan-in-replacement");
    install_single_wire_codex_fixture(&state, Some(runtime));
    state.resume_session_queue(&session_id).unwrap();
    assert!(matches!(
        recv_within_guard(&received, "the replacement should receive the original result"),
        Ok(CodexRuntimeCommand::Prompt { command, .. }) if command.prompt.contains(&wait_id)
    ));
    assert!(matches!(
        received.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let resumed = session_record(&state, &session_id);
    assert!(resumed.queued_prompts.is_empty());
    assert_eq!(
        resumed
            .session
            .messages
            .iter()
            .filter(|message| message.id() == prompt_id)
            .count(),
        1
    );
}

#[test]
fn failed_fast_discovery_retains_the_exact_fan_in_for_resume() {
    let (state, _) = test_app_state_with_structured_reviewer_runtime("fast-fan-in-rejected");
    let session_id = test_session_id(&state, Agent::Codex);
    let wait_id = settle_final_wait_before_dispatch(&state, &session_id);
    let (runtime, discovery) = test_codex_runtime_handle("fan-in-fast-discovery");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session_id).unwrap();
        let record = &mut inner.sessions[index];
        record.runtime = SessionRuntime::Codex(runtime);
        record.session.model = "catalog-model".to_owned();
        record.session.model_options.clear();
        record.session.codex_fast_mode = true;
    }
    let started = state
        .start_next_queued_turn_off_lock(&session_id, false, false)
        .unwrap()
        .expect("the wait result should be promoted");
    let prompt_id = started.queued.pending_prompt.id;
    std::thread::scope(|scope| {
        let state = &state;
        let worker = scope.spawn(move || handoff_prepared_turn_dispatch(state, started.dispatch));
        let CodexRuntimeCommand::RefreshModelList { response_tx } =
            super::phase_sync::receive(&discovery, "Fast must request the current model catalog")
        else {
            panic!("expected catalog discovery before provider enqueue");
        };
        response_tx
            .send(Err("model/list unavailable".to_owned()))
            .unwrap();
        let error = worker
            .join()
            .unwrap()
            .expect_err("Fast discovery must fail");
        assert!(error.message.contains("failed to resolve Codex Fast"));
    });
    assert!(matches!(
        discovery.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    ));
    let rejected = session_record(&state, &session_id);
    assert_eq!(rejected.session.status, SessionStatus::Error);
    assert!(rejected.session.queue_paused);
    assert_eq!(rejected.queued_prompts.len(), 1);
    assert_eq!(rejected.queued_prompts[0].pending_prompt.id, prompt_id);

    let (runtime, received, _) = test_shared_codex_runtime("fast-fan-in-standard-retry");
    install_single_wire_codex_fixture(&state, Some(runtime));
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].session.codex_fast_mode = false;
    }
    state.resume_session_queue(&session_id).unwrap();
    assert!(matches!(
        recv_within_guard(&received, "the same result should reach the Standard provider"),
        Ok(CodexRuntimeCommand::Prompt { command, .. }) if command.prompt.contains(&wait_id)
    ));
    assert!(
        session_record(&state, &session_id)
            .queued_prompts
            .is_empty()
    );
}

#[tokio::test]
async fn stop_and_resume_a_legacy_fan_in_with_a_blank_line_in_its_title() {
    let (state, received) =
        test_app_state_with_structured_reviewer_runtime("legacy-multiline-fan-in");
    let session_id = test_session_id(&state, Agent::Codex);
    let wait_id = format!("delegation-wait-{}", Uuid::new_v4());
    let mut wait = seeded_delegation_wait(&wait_id, &session_id);
    wait.title = Some("Review round\n\nBoth results".to_owned());
    let prompt = build_delegation_wait_resume_prompt(&wait, &[], &[], &[]);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session_id).unwrap();
        let record = &mut inner.sessions[index];
        queue_seeded_prompt(
            record,
            "legacy-wait-result",
            QueuedPromptSource::Orchestrator,
        );
        let entry = &mut record.queued_prompts[0];
        entry.workflow = None;
        entry.pending_prompt.text = prompt;
        *entry = serde_json::from_value(serde_json::to_value(&*entry).unwrap()).unwrap();
        sync_pending_prompts(record);
        state.commit_locked(&mut inner).unwrap();
    }
    let (status, response) = post_stop(&state, &session_id).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(wire_session(&response, &session_id)["queuePaused"], true);

    state.resume_session_queue(&session_id).unwrap();
    assert!(matches!(
        recv_within_guard(&received, "Resume must hand off the legacy result"),
        Ok(CodexRuntimeCommand::Prompt { command, .. }) if command.prompt.contains(&wait_id)
    ));
    assert!(
        session_record(&state, &session_id)
            .queued_prompts
            .is_empty()
    );
}

fn queue_seeded_prompt(record: &mut SessionRecord, id: &str, source: QueuedPromptSource) {
    queue_prompt_on_record_with_source(
        record,
        PendingPrompt {
            engram_interrupted: false,
            is_engram_retained: false,
            attachments: Vec::new(),
            id: id.to_owned(),
            timestamp: stamp_now(),
            text: format!("queued {id}"),
            expanded_text: None,
            source: None,
        },
        Vec::new(),
        source,
    );
}

#[test]
fn delete_hold_before_enqueue_allows_one_explicit_fan_in_resume() {
    const ORIGINAL_TIMESTAMP: &str = "retained transcript timestamp";
    let (state, provider) = test_app_state_with_structured_reviewer_runtime("fan-in-delete-hold");
    let session_id = test_session_id(&state, Agent::Codex);
    let wait_id = settle_final_wait_before_dispatch(&state, &session_id);
    let started = state
        .start_next_queued_turn_off_lock(&session_id, false, false)
        .unwrap()
        .expect("fan-in should promote before the Delete hold");
    let prompt_id = started.queued.pending_prompt.id;
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session_id).unwrap();
        let record = &mut inner.sessions[index];
        let message_index = cached_message_index_on_record(record, &prompt_id).unwrap();
        let Message::Text { timestamp, .. } = &mut record.session.messages[message_index] else {
            panic!("the prepared fan-in should have a transcript message");
        };
        *timestamp = ORIGINAL_TIMESTAMP.to_owned();
        record.mark_body_changed(&prompt_id);
        retain_source_observation_removal_locked(&mut inner, &[session_id.clone()]);
        assert!(inner.sessions[index].engram.observation_delete_requested);
        state.commit_locked(&mut inner).unwrap();
    }
    assert!(matches!(
        handoff_prepared_turn_dispatch(&state, started.dispatch).unwrap(),
        HandoffPreparedTurnDispatchOutcome::HeldBeforeSend
    ));
    let held = session_record(&state, &session_id);
    assert_eq!(held.session.status, SessionStatus::Idle);
    assert!(held.session.queue_paused);
    assert!(held.engram.observation_delete_requested);
    assert_eq!(held.queued_prompts.len(), 1);
    assert_eq!(held.queued_prompts[0].pending_prompt.id, prompt_id);
    assert!(!held.session.pending_prompts[0].is_engram_retained);
    assert!(held.session.prompt_deliveries.is_empty());
    assert!(session_has_runnable_queued_prompt(&held));
    let thread_error = state
        .resolve_codex_thread_action_context(&session_id)
        .err()
        .expect("a runnable result must hold Codex thread actions");
    assert_eq!(thread_error.status, StatusCode::CONFLICT);
    assert!(thread_error.message.contains("queued Codex prompts"));
    assert!(matches!(
        provider.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    let mut deltas = state.subscribe_delta_events();
    state.resume_session_queue(&session_id).unwrap();
    assert!(matches!(
        recv_within_guard(&provider, "Resume should deliver the result after clearing Delete"),
        Ok(CodexRuntimeCommand::Prompt { command, .. }) if command.prompt.contains(&wait_id)
    ));
    let resumed = session_record(&state, &session_id);
    assert!(!resumed.engram.observation_delete_requested);
    assert!(resumed.queued_prompts.is_empty());
    let mut resumed_timestamp = None;
    while let Ok(payload) = deltas.try_recv() {
        if let Ok(DeltaEvent::MessageCreated {
            message_id,
            message: Message::Text { timestamp, .. },
            ..
        }) = serde_json::from_str::<DeltaEvent>(&payload)
            && message_id == prompt_id
        {
            resumed_timestamp = Some(timestamp);
        }
    }
    assert_eq!(resumed_timestamp.as_deref(), Some(ORIGINAL_TIMESTAMP));
    assert!(matches!(
        provider.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert_eq!(
        resumed
            .session
            .messages
            .iter()
            .filter(|message| message.id() == prompt_id)
            .count(),
        1
    );
}

#[test]
fn cancel_refuses_the_exact_fan_in_while_its_handoff_is_in_flight() {
    let (state, provider) =
        test_app_state_with_structured_reviewer_runtime("fan-in-inflight-cancel");
    let session_id = test_session_id(&state, Agent::Codex);
    let wait_id = settle_final_wait_before_dispatch(&state, &session_id);
    let started = state
        .start_next_queued_turn_off_lock(&session_id, false, false)
        .unwrap()
        .expect("fan-in should have a prepared owner");
    let prompt_id = started.queued.pending_prompt.id;
    let prepared = session_record(&state, &session_id);
    assert!(prepared.session.prompt_deliveries.iter().any(|entry| {
        entry.prompt_id == prompt_id && entry.disposition == PromptDeliveryDisposition::HandingOff
    }));

    let error = state
        .cancel_queued_prompt(&session_id, &prompt_id)
        .err()
        .expect("Cancel cannot remove the current handoff owner's result");
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(matches!(
        provider.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(matches!(
        handoff_prepared_turn_dispatch(&state, started.dispatch).unwrap(),
        HandoffPreparedTurnDispatchOutcome::Delivered
    ));
    assert!(matches!(
        recv_within_guard(&provider, "the owned result should arrive"),
        Ok(CodexRuntimeCommand::Prompt { command, .. }) if command.prompt.contains(&wait_id)
    ));
    assert!(
        session_record(&state, &session_id)
            .queued_prompts
            .is_empty()
    );
}

#[test]
fn automatic_fan_in_promotion_save_rejection_keeps_a_durable_resume() {
    let (state, provider) =
        test_app_state_with_structured_reviewer_runtime("fan-in-promotion-save-rejected");
    let session_id = test_session_id(&state, Agent::Codex);
    let wait_id = settle_final_wait_before_dispatch(&state, &session_id);
    let original = session_record(&state, &session_id);
    let prompt_id = &original.queued_prompts[0].pending_prompt.id;
    let db = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    db.execute_batch(&format!(
        "CREATE TRIGGER reject_fan_in_promotion BEFORE INSERT ON messages
         WHEN NEW.message_id='{}' BEGIN SELECT RAISE(ABORT,'fan-in promotion rejected'); END;",
        prompt_id.replace('\'', "''"),
    ))
    .unwrap();

    let revision = state.inner.lock().unwrap().revision;
    state.dispatch_delegation_wait_resumes(revision, vec![session_id.clone()]);
    assert!(matches!(
        provider.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let saved = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let saved = &saved.sessions[saved.find_session_index(&session_id).unwrap()];
    assert!(saved.session.queue_paused);
    assert_eq!(saved.queued_prompts.len(), 1);
    assert_eq!(&saved.queued_prompts[0].pending_prompt.id, prompt_id);
    assert!(cached_message_index_on_record(saved, prompt_id).is_none());

    db.execute_batch("DROP TRIGGER reject_fan_in_promotion;")
        .unwrap();
    state.resume_session_queue(&session_id).unwrap();
    assert!(matches!(
        recv_within_guard(&provider, "Resume should deliver the result after storage recovers"),
        Ok(CodexRuntimeCommand::Prompt { command, .. }) if command.prompt.contains(&wait_id)
    ));
    assert!(matches!(
        provider.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let record = session_record(&state, &session_id);
    assert!(record.queued_prompts.is_empty());
    assert!(cached_message_index_on_record(&record, prompt_id).is_some());
}

#[test]
fn queued_fan_in_waits_for_its_background_promotion_row_before_enqueue() {
    let (mut state, provider) =
        test_app_state_with_structured_reviewer_runtime("fan-in-background-promotion");
    let session_id = test_session_id(&state, Agent::Codex);
    let wait_id = settle_final_wait_before_dispatch(&state, &session_id);
    let prompt_id = session_record(&state, &session_id).queued_prompts[0]
        .pending_prompt
        .id
        .clone();
    let (tx, rx) = mpsc::channel();
    state.persist_tx = tx;
    let started = state
        .start_next_queued_turn_off_lock(&session_id, false, false)
        .unwrap()
        .expect("the background writer has queued the promotion");
    let mut cache = SqlitePersistConnectionCache::new();
    let mut batch = PersistFenceBatch::default();
    std::thread::scope(|scope| {
        let handoff = scope.spawn(|| handoff_prepared_turn_dispatch(&state, started.dispatch));
        let acknowledgement = loop {
            let request = phase_sync::receive(&rx, "exact promotion fence must reach the writer");
            let waiter = match &request {
                PersistRequest::Fence(fence)
                    if matches!(&fence.target,
                    PersistFenceTarget::SessionRowAtOrAfter { session_id: target, .. }
                        if target == &session_id) =>
                {
                    Some(PersistFenceWaiter {
                        completion: fence.completion.clone(),
                    })
                }
                _ => None,
            };
            batch.accept(request);
            if let Some(waiter) = waiter {
                break waiter;
            }
        };
        let mut rejected = collect_persist_delta_from_shared_state(&state.inner, 0);
        let parent = rejected
            .changed_sessions
            .iter_mut()
            .find(|row| row.session.id == session_id)
            .unwrap();
        let promoted_message = parent
            .session
            .messages
            .iter()
            .find(|message| message.id() == prompt_id)
            .unwrap()
            .clone();
        // Reject just this captured parent row, keeping the live carrier and
        // its valid successor available for a later write acknowledgement.
        parent.session.messages.push(promoted_message);
        let ids = persist_delta_with_fences(
            &mut cache,
            state.persistence_path.as_path(),
            &rejected,
            &mut batch,
        )
        .unwrap();
        assert!(!ids.contains(&session_id));
        assert_eq!(
            acknowledgement.completion.poll_at(acknowledgement.now()),
            None
        );
        assert!(matches!(
            provider.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        let saved = load_state(state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        let parent = &saved.sessions[saved.find_session_index(&session_id).unwrap()];
        assert_eq!(parent.queued_prompts[0].pending_prompt.id, prompt_id);
        assert!(parent.queued_prompts[0].promoted_message_index.is_none());

        let retry = collect_persist_delta_from_shared_state(&state.inner, 0);
        let ids = persist_delta_with_fences(
            &mut cache,
            state.persistence_path.as_path(),
            &retry,
            &mut batch,
        )
        .unwrap();
        assert!(ids.contains(&session_id));
        assert!(matches!(
            handoff.join().unwrap().unwrap(),
            HandoffPreparedTurnDispatchOutcome::Delivered
        ));
    });
    assert!(matches!(
        phase_sync::receive(&provider, "the acknowledged promotion reaches the provider"),
        CodexRuntimeCommand::Prompt { command, .. } if command.prompt.contains(&wait_id)
    ));
    assert!(matches!(
        provider.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let saved = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let parent = &saved.sessions[saved.find_session_index(&session_id).unwrap()];
    assert_eq!(parent.queued_prompts[0].pending_prompt.id, prompt_id);
    assert!(parent.queued_prompts[0].promoted_message_index.is_some());
    assert!(!ordinary_queue_entry_is_runnable(
        parent,
        &parent.queued_prompts[0]
    ));
}

#[test]
fn shutdown_holds_an_unsent_fan_in_for_one_boot_delivery() {
    let (mut state, provider) =
        test_app_state_with_structured_reviewer_runtime("fan-in-shutdown-before-enqueue");
    let session_id = test_session_id(&state, Agent::Codex);
    let wait_id = settle_final_wait_before_dispatch(&state, &session_id);
    let started = state
        .start_next_queued_turn_off_lock(&session_id, false, false)
        .unwrap()
        .expect("the fan-in should have a prepared dispatch");
    let prompt_id = started.queued.pending_prompt.id;
    state.shutdown_signal_tx.send_replace(true);
    // The carrier can remain parked past the final persistence cut.
    state.shutdown_persist_checked().unwrap();
    assert!(matches!(
        provider.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let saved = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let parent = &saved.sessions[saved.find_session_index(&session_id).unwrap()];
    assert_eq!(parent.queued_prompts.len(), 1);
    assert_eq!(parent.queued_prompts[0].pending_prompt.id, prompt_id);
    assert!(parent.queued_prompts[0].fan_in_promotion_is_unsent());
    assert!(matches!(
        handoff_prepared_turn_dispatch(&state, started.dispatch).unwrap(),
        HandoffPreparedTurnDispatchOutcome::HeldBeforeSend
    ));

    state.inner = Arc::new(StateMutex::new(saved));
    state.shutdown_signal_tx.send_replace(false);
    state.dispatch_orphaned_workflow_prompts();
    assert!(matches!(
        phase_sync::receive(&provider, "the proven-unsent result is recovered on boot"),
        CodexRuntimeCommand::Prompt { command, .. } if command.prompt.contains(&wait_id)
    ));
    assert!(
        session_record(&state, &session_id)
            .queued_prompts
            .is_empty()
    );
    state.dispatch_orphaned_workflow_prompts();
    assert!(matches!(
        provider.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

#[test]
fn fan_in_save_rejection_after_enqueue_retries_storage_without_provider_replay() {
    let (state, provider) =
        test_app_state_with_structured_reviewer_runtime("fan-in-enqueued-save-rejected");
    let session_id = test_session_id(&state, Agent::Codex);
    let wait_id = settle_final_wait_before_dispatch(&state, &session_id);
    let started = state
        .start_next_queued_turn_off_lock(&session_id, false, false)
        .unwrap()
        .expect("the fan-in should promote before its save is refused");
    let generation = started.dispatch.active_turn_generation();
    let runtime = started.dispatch.runtime_token().clone();
    let db = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
    db.execute_batch(&format!(
        "CREATE TRIGGER reject_fan_in_settlement BEFORE INSERT ON sessions
         WHEN NEW.id='{}' BEGIN SELECT RAISE(ABORT,'fan-in settlement rejected'); END;",
        session_id.replace('\'', "''"),
    ))
    .unwrap();
    assert!(matches!(
        handoff_prepared_turn_dispatch(&state, started.dispatch).unwrap(),
        HandoffPreparedTurnDispatchOutcome::Delivered
    ));
    assert!(matches!(
        recv_within_guard(&provider, "one provider send occurred before the save rejection"),
        Ok(CodexRuntimeCommand::Prompt { command, .. }) if command.prompt.contains(&wait_id)
    ));
    assert!(
        session_record(&state, &session_id)
            .queued_prompts
            .is_empty()
    );
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session_id).unwrap();
        let error = state
            .persist_delegation_fan_in_custody_locked(&mut inner, index)
            .expect_err("an isolated rejection of the exact row must not count as saved");
        assert!(format!("{error:#}").contains("fan-in settlement rejected"));
    }
    db.execute_batch("DROP TRIGGER reject_fan_in_settlement;")
        .unwrap();
    state
        .finish_turn_ok_if_runtime_and_generation_match(&session_id, &runtime, generation)
        .unwrap();
    let saved = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let saved = &saved.sessions[saved.find_session_index(&session_id).unwrap()];
    assert!(saved.queued_prompts.is_empty());
    assert!(matches!(
        provider.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

#[test]
fn failed_automatic_fan_in_launch_leaves_the_owned_result_available_for_resume() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session_id).unwrap();
        inner.sessions[index].session.agent = Agent::Cursor;
        inner.sessions[index].runtime = SessionRuntime::None;
    }
    let wait_id = settle_final_wait_before_dispatch(&state, &session_id);
    let original = session_record(&state, &session_id);
    let prompt_id = &original.queued_prompts[0].pending_prompt.id;

    // This fixture has runtime spawning disabled until the explicit override.
    let revision = state.inner.lock().unwrap().revision;
    state.dispatch_delegation_wait_resumes(revision, vec![session_id.clone()]);
    let failed = session_record(&state, &session_id);
    assert_eq!(failed.session.status, SessionStatus::Idle);
    assert!(failed.session.queue_paused);
    assert_eq!(failed.queued_prompts.len(), 1);
    assert_eq!(&failed.queued_prompts[0].pending_prompt.id, prompt_id);
    assert_eq!(
        failed.session.messages.len(),
        original.session.messages.len()
    );

    let (runtime, provider) = test_acp_runtime_handle(AcpAgent::Cursor, "fan-in-launch-retry");
    state.install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
    state.resume_session_queue(&session_id).unwrap();
    assert!(matches!(
        recv_within_guard(&provider, "the original result should launch on Resume"),
        Ok(AcpRuntimeCommand::Prompt(command)) if command.prompt.contains(&wait_id)
    ));
    assert!(
        session_record(&state, &session_id)
            .queued_prompts
            .is_empty()
    );
}

#[test]
fn idle_stop_of_a_queued_fan_in_cancels_overlapping_capacity_recovery() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let wait_id = settle_final_wait_before_dispatch(&state, &session_id);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session_id).unwrap();
        let record = &mut inner.sessions[index];
        record.set_auto_dispatch_blocked(true);
        record.session.provider_capacity_waiting = true;
        record.session.preview = "Waiting: provider at capacity, retry 1/8".to_owned();
        record.provider_capacity_retry = Some(ProviderCapacityRetry {
            nonce: "fan-in-capacity-retry".to_owned(),
            server_instance: state.server_instance_id.clone(),
            runtime_id: "capacity-runtime".to_owned(),
            generation: record.active_turn_generation,
            stop_generation: record.runtime_stop_generation,
            pause_epoch: record.auto_dispatch_pause_epoch,
            settings: "capacity-retry-settings".to_owned(),
            attempts: 1,
            due_at: Some("2099-01-01T00:00:00Z".to_owned()),
            recovering: false,
            dispatched: false,
            original_preview: "capacity error".to_owned(),
            saved: false,
            fence: None,
        });
    }
    state.request_stop_session(&session_id).unwrap();
    let stopped = session_record(&state, &session_id);
    assert_eq!(stopped.session.status, SessionStatus::Idle);
    assert!(stopped.session.queue_paused);
    assert!(stopped.engram.operator_paused);
    assert!(!stopped.session.provider_capacity_waiting);
    assert!(stopped.provider_capacity_retry.is_none());
    assert_eq!(stopped.queued_prompts.len(), 1);
    assert!(
        stopped.queued_prompts[0]
            .pending_prompt
            .text
            .contains(&wait_id)
    );
}

#[test]
fn stop_on_an_idle_waiting_session_drops_continuations_and_keeps_other_prompts_paused() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        *inner.delegation_waits = vec![seeded_delegation_wait("wait-mixed", &session_id)];
        let index = inner.find_session_index(&session_id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        queue_seeded_prompt(record, "from-workflow", QueuedPromptSource::Orchestrator);
        queue_seeded_prompt(record, "from-user", QueuedPromptSource::User);
        queue_seeded_prompt(record, "from-mailbox", QueuedPromptSource::Mailbox);
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .request_stop_session(&session_id)
        .expect("Stop on a waiting Idle session should succeed");

    let record = session_record(&state, &session_id);
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(record.session.queue_paused);
    let mut kept = record
        .queued_prompts
        .iter()
        .map(|queued| (queued.pending_prompt.id.clone(), queued.source))
        .collect::<Vec<_>>();
    kept.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        kept,
        vec![
            ("from-mailbox".to_owned(), QueuedPromptSource::Mailbox),
            ("from-user".to_owned(), QueuedPromptSource::User),
        ],
        "Stop drops only the workflow continuation"
    );
    assert!(
        state
            .dispatch_next_queued_turn(&session_id, false)
            .expect("paused queue inspection should succeed")
            .is_none(),
        "the kept prompts wait behind the latch"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn idle_stop_preserves_delegation_wait_and_queued_result_but_consumes_test_run_wait() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        *inner.delegation_waits = vec![seeded_delegation_wait("wait-still-pending", &session_id)];
        *inner.test_run_waits = vec![test_run_wait_record("run-wait-still-pending", &session_id)];
        let index = inner.find_session_index(&session_id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        // Ordinary workflows are dropped. A fan-in already queued by a
        // different delegation wait is held together with human work.
        queue_seeded_prompt(record, "first-workflow", QueuedPromptSource::Orchestrator);
        queue_seeded_prompt(record, "later-workflow", QueuedPromptSource::Orchestrator);
        queue_seeded_prompt(record, "from-user", QueuedPromptSource::User);
        let completed_wait = seeded_delegation_wait(
            "delegation-wait-36e423bf-cc8c-4f39-a4f5-23771ea551e7",
            &session_id,
        );
        let mut completed_wait = completed_wait;
        completed_wait.title = Some(
            "Delegation wait `not-a-wait-id` ended because one or more delegation records disappeared.\n\nRequested delegations:\nQuoted title."
                .to_owned(),
        );
        let fan_in = build_delegation_wait_resume_prompt(&completed_wait, &[], &[], &[]);
        queue_orchestrator_prompt_on_record(
            record,
            PendingPrompt {
                engram_interrupted: false,
                is_engram_retained: false,
                attachments: Vec::new(),
                id: "workflow-quoting-result".to_owned(),
                timestamp: stamp_now(),
                text: fan_in.clone(),
                expanded_text: None,
                source: None,
            },
            Vec::new(),
        );
        let queued_result = queue_delegation_wait_resume_locked(&mut inner, &session_id, fan_in);
        assert!(queued_result.prompt_queued);
        assert!(!queued_result.dispatch_requested);
        state.commit_locked(&mut inner).unwrap();
    }
    let messages_before = session_record(&state, &session_id).session.messages.len();
    let mut delta_events = state.subscribe_delta_events();

    state
        .request_stop_session(&session_id)
        .expect("Stop of an idle waiting session should succeed");

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert_eq!(inner.delegation_waits.len(), 1);
        assert_eq!(inner.delegation_waits[0].id, "wait-still-pending");
        assert!(inner.test_run_waits.is_empty());
    }
    let record = session_record(&state, &session_id);
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(record.engram.operator_paused);
    assert_eq!(record.queued_prompts.len(), 2);
    assert_eq!(record.queued_prompts[0].pending_prompt.id, "from-user");
    assert!(matches!(
        record.queued_prompts[1].workflow,
        Some(QueuedWorkflow::DelegationFanIn { .. })
    ));
    assert!(
        record.queued_prompts[1]
            .pending_prompt
            .text
            .contains("delegation-wait-36e423bf-cc8c-4f39-a4f5-23771ea551e7")
    );
    assert_eq!(record.session.preview, SESSION_STOPPED_BY_USER_MESSAGE);
    assert_eq!(
        record.session.messages.len(),
        messages_before + 1,
        "one Stop records exactly one stopped message"
    );
    let mut consumed_reasons = Vec::new();
    while let Ok(payload) = delta_events.try_recv() {
        match serde_json::from_str::<DeltaEvent>(&payload) {
            Ok(DeltaEvent::DelegationWaitConsumed { reason, .. }) => {
                consumed_reasons.push(format!("{reason:?}"))
            }
            Ok(DeltaEvent::TestRunWaitConsumed { reason, .. }) => {
                consumed_reasons.push(format!("{reason:?}"))
            }
            _ => {}
        }
    }
    consumed_reasons.sort();
    assert_eq!(consumed_reasons, vec!["SessionStopped".to_owned()]);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn stop_on_an_idle_waiting_session_restores_everything_when_its_commit_fails() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let baseline_waits = vec![
        seeded_delegation_wait("wait-other-parent", "session-other-parent"),
        seeded_delegation_wait("wait-stopped-first", &session_id),
        seeded_delegation_wait("wait-stopped-second", &session_id),
    ];
    let baseline_run_waits = vec![
        test_run_wait_record("run-wait-stopped", &session_id),
        test_run_wait_record("run-wait-other", "session-other-parent"),
    ];
    let baseline_record = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        *inner.delegation_waits = baseline_waits.clone();
        *inner.test_run_waits = baseline_run_waits.clone();
        let index = inner.find_session_index(&session_id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        queue_seeded_prompt(record, "from-workflow", QueuedPromptSource::Orchestrator);
        queue_seeded_prompt(record, "from-user", QueuedPromptSource::User);
        let baseline_record = record.clone();
        state.commit_locked(&mut inner).unwrap();
        baseline_record
    };

    let failing_persistence_path =
        test_temp_dir().join(format!("termal-idle-stop-rollback-{}", Uuid::new_v4()));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");
    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    let mut delta_events = state.subscribe_delta_events();

    let error = match state.request_stop_session(&session_id) {
        Ok(_) => panic!("a persistence failure should reject the Stop"),
        Err(error) => error,
    };

    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        error
            .message
            .contains("failed to persist the stopped waits"),
        "{}",
        error.message
    );
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert_eq!(
            inner.delegation_waits, baseline_waits,
            "a failed Stop commit restores every delegation wait in its order"
        );
        assert_eq!(
            inner.test_run_waits, baseline_run_waits,
            "and every run wait in its order"
        );
    }
    let record = session_record(&state, &session_id);
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(!record.orchestrator_auto_dispatch_blocked);
    assert!(!record.engram.operator_paused);
    assert!(!record.session.queue_paused);
    assert_eq!(record.session.preview, baseline_record.session.preview);
    assert_eq!(
        record.session.messages.len(),
        baseline_record.session.messages.len(),
        "no stopped message survives a failed commit"
    );
    let queued_ids = |record: &SessionRecord| {
        record
            .queued_prompts
            .iter()
            .map(|queued| queued.pending_prompt.id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        queued_ids(&record),
        queued_ids(&baseline_record),
        "the dropped continuation comes back, in its place"
    );
    assert_eq!(record.queued_prompts.len(), 2);
    assert!(
        matches!(
            delta_events.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ),
        "nothing is published for an uncommitted Stop"
    );

    remove_test_directory(failing_persistence_path);
}

#[tokio::test]
async fn idle_stop_refuses_an_isolated_parent_row_rejection() {
    let (state, provider) =
        test_app_state_with_structured_reviewer_runtime("idle-stop-parent-write-rejected");
    let session_id = test_session_id(&state, Agent::Codex);
    let wait_id = settle_final_wait_before_dispatch(&state, &session_id);
    let run_wait = test_run_wait_record("idle-stop-run-wait", &session_id);
    let (valid_parent, saved_revision) = {
        let mut inner = state.inner.lock().unwrap();
        inner.test_run_waits.push(run_wait.clone());
        let saved_revision = state.commit_locked(&mut inner).unwrap();
        let message = Message::Text {
            attachments: Vec::new(),
            id: inner.next_message_id(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: "duplicate parent transcript fixture".to_owned(),
            expanded_text: None,
            source: None,
        };
        let parent = inner.session_mut(&session_id).unwrap();
        let valid_parent = parent.clone();
        parent.session.messages.push(message.clone());
        parent.session.messages.push(message);
        (valid_parent, saved_revision)
    };

    let (status, reply) = post_stop(&state, &session_id).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{reply}");
    let error = reply.to_string();
    assert!(error.contains("was not saved") && error.contains("duplicate transcript identity"));
    assert!(!session_record(&state, &session_id).session.queue_paused);

    let saved = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(
        saved.revision > saved_revision,
        "the batch committed, except for the parent row"
    );
    let parent = &saved.sessions[saved.find_session_index(&session_id).unwrap()];
    assert!(!parent.session.queue_paused);
    assert_eq!(parent.queued_prompts.len(), 1);
    assert!(
        parent.queued_prompts[0]
            .pending_prompt
            .text
            .contains(&wait_id)
    );
    assert_eq!(saved.test_run_waits.as_slice(), &[run_wait]);
    assert!(matches!(
        provider.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    {
        let mut inner = state.inner.lock().unwrap();
        *inner.session_mut(&session_id).unwrap() = valid_parent;
    }
    let (status, reply) = post_stop(&state, &session_id).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(wire_session(&reply, &session_id)["queuePaused"], true);
    let saved = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let parent = &saved.sessions[saved.find_session_index(&session_id).unwrap()];
    assert!(parent.session.queue_paused);
    assert_eq!(parent.queued_prompts.len(), 1);
    assert!(
        parent.queued_prompts[0]
            .pending_prompt
            .text
            .contains(&wait_id)
    );
    assert!(saved.test_run_waits.is_empty());
    assert!(matches!(
        provider.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

#[test]
fn repeated_idle_stop_waits_for_its_background_pause_row() {
    check_repeated_idle_stop_with_pending_retirement(true);
}

#[test]
fn test_run_only_idle_stop_can_reconfirm_its_pending_retirement() {
    check_repeated_idle_stop_with_pending_retirement(false);
}

fn check_repeated_idle_stop_with_pending_retirement(with_delegation_result: bool) {
    let (mut state, provider) =
        test_app_state_with_structured_reviewer_runtime("idle-stop-background-pause");
    let session_id = test_session_id(&state, Agent::Codex);
    let wait_id =
        with_delegation_result.then(|| settle_final_wait_before_dispatch(&state, &session_id));
    let run_wait = test_run_wait_record("idle-stop-background-run-wait", &session_id);
    {
        let mut inner = state.inner.lock().unwrap();
        inner.test_run_waits.push(run_wait.clone());
        state.commit_locked(&mut inner).unwrap();
    }
    let (tx, rx) = mpsc::channel();
    state.persist_tx = tx;
    let mut batch = PersistFenceBatch::default();
    let mut cache = SqlitePersistConnectionCache::new();

    std::thread::scope(|scope| {
        let next_pause_fence = |batch: &mut PersistFenceBatch| loop {
            let request =
                phase_sync::receive(&rx, "Stop should request its own row acknowledgement");
            let waiter = match &request {
                PersistRequest::Fence(fence)
                    if matches!(&fence.target,
                        PersistFenceTarget::SessionRowAtOrAfter { session_id: target, .. }
                            if target == &session_id) =>
                {
                    Some(PersistFenceWaiter {
                        completion: fence.completion.clone(),
                    })
                }
                _ => None,
            };
            batch.accept(request);
            if let Some(waiter) = waiter {
                break waiter;
            }
        };
        let first = scope.spawn(|| state.request_stop_session(&session_id));
        let first_ack = next_pause_fence(&mut batch);
        let repeated = scope.spawn(|| state.request_stop_session(&session_id));
        let repeated_ack = next_pause_fence(&mut batch);

        let mut cut = collect_persist_delta_from_shared_state(&state.inner, 0);
        let parent = cut
            .changed_sessions
            .iter_mut()
            .find(|row| row.session.id == session_id)
            .unwrap();
        // Only this captured row is invalid. The live pause can still be
        // collected on the next tick after the isolated rejection.
        let stopped_message = parent.session.messages.last().unwrap().clone();
        parent.session.messages.push(stopped_message);
        let written = persist_delta_with_fences(
            &mut cache,
            state.persistence_path.as_path(),
            &cut,
            &mut batch,
        )
        .unwrap();
        assert!(!written.contains(&session_id));
        assert_eq!(first_ack.completion.poll_at(first_ack.now()), None);
        assert_eq!(repeated_ack.completion.poll_at(repeated_ack.now()), None);
        retain_unpersisted_session_changes(&state.inner, &cut, &written);

        let saved = load_state(state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        let parent = &saved.sessions[saved.find_session_index(&session_id).unwrap()];
        assert!(!parent.session.queue_paused);
        assert_eq!(saved.test_run_waits.as_slice(), &[run_wait]);

        let pending_stops = if with_delegation_result {
            vec![first, repeated]
        } else {
            batch.fail(PersistFenceError::WriteFailed(
                "pause write failed".to_owned(),
            ));
            for stop in [first, repeated] {
                let Err(error) = stop.join().unwrap() else {
                    panic!("Stop must report the write failure");
                };
                assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
            }
            let retried = scope.spawn(|| state.request_stop_session(&session_id));
            next_pause_fence(&mut batch);
            vec![retried]
        };
        let retry = collect_persist_delta_from_shared_state(&state.inner, cut.watermark);
        let written = persist_delta_with_fences(
            &mut cache,
            state.persistence_path.as_path(),
            &retry,
            &mut batch,
        )
        .unwrap();
        assert!(written.contains(&session_id));
        for stop in pending_stops {
            assert!(stop.join().unwrap().is_ok());
        }
    });

    let saved = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let parent = &saved.sessions[saved.find_session_index(&session_id).unwrap()];
    assert!(parent.session.queue_paused);
    assert!(saved.test_run_waits.is_empty());
    assert_eq!(
        parent.queued_prompts.len(),
        usize::from(with_delegation_result)
    );
    if let Some(wait_id) = wait_id {
        assert!(
            parent.queued_prompts[0]
                .pending_prompt
                .text
                .contains(&wait_id)
        );
    }
    let stopped = parent.session.messages.iter().filter(|message| {
        matches!(message, Message::Text { text, .. } if text == SESSION_STOPPED_BY_USER_MESSAGE)
    });
    assert_eq!(stopped.count(), 1);
    assert!(matches!(
        provider.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}
