//! Stop on an Idle session that is waiting for automatic work.
//!
//! A root session that registers a delegation wait or a test-run wait and then
//! ends its turn is Idle until the awaited work resumes it. The public Stop
//! route is the operator's brake on that resume: it consumes the session's
//! waits, leaves the session Idle behind the explicit-resume latch, and starts
//! nothing until an explicit Resume or a new user prompt. Idle with no pending
//! wait still has nothing to stop.
//!
//! Owns these route-level tests only. It does not own Stop of an Active
//! session (`session_stop.rs`, `session_stop_runtime.rs`,
//! `delegation_wait.rs`) or run-wait settling (`test_run_waits.rs`). New
//! module; not split from an existing file.

use super::delegation_support::{
    finish_delegation_child_with_assistant_text, test_app_state_with_delegation_codex_runtime,
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
async fn stop_on_an_idle_parent_consumes_its_delegation_wait_and_nothing_resumes_it() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("idle-wait-stop-delegation");
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
    let mut delta_events = state.subscribe_delta_events();

    let (status, response) = post_stop(&state, &parent_session_id).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "Stop on a waiting Idle parent: {response}"
    );
    let stopped = wire_session(&response, &parent_session_id);
    assert_eq!(stopped["status"], "idle");
    assert_eq!(stopped["queuePaused"], true);
    let mut saw_stopped_wait = false;
    while let Ok(payload) = delta_events.try_recv() {
        let event: DeltaEvent =
            serde_json::from_str(&payload).expect("stop delta should deserialize");
        if matches!(
            event,
            DeltaEvent::DelegationWaitConsumed {
                wait_id,
                parent_session_id: delta_parent_session_id,
                reason: DelegationWaitConsumedReason::ParentSessionStopped,
                ..
            } if wait_id == wait.wait.id && delta_parent_session_id == parent_session_id
        ) {
            saw_stopped_wait = true;
        }
    }
    assert!(
        saw_stopped_wait,
        "Stop should publish why the wait was consumed"
    );
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert!(
            inner
                .delegation_waits
                .iter()
                .all(|wait| wait.parent_session_id != parent_session_id),
            "Stop must consume the waits that would resume the parent"
        );
    }
    let record = session_record(&state, &parent_session_id);
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(record.engram.operator_paused);
    assert!(matches!(record.runtime, SessionRuntime::None));

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nLate review result.",
    );
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
    assert!(
        record
            .queued_prompts
            .iter()
            .all(|queued| queued.source != QueuedPromptSource::Orchestrator),
        "no fan-in prompt may wait to resume the stopped parent"
    );

    // An explicit Resume lifts the latch, and the session runs a new prompt
    // as usual.
    state
        .resume_session_queue(&parent_session_id)
        .expect("Resume should lift the latch");
    assert!(!session_record(&state, &parent_session_id).orchestrator_auto_dispatch_blocked);
    let dispatched = state
        .dispatch_turn(&parent_session_id, user_prompt("Carry on."))
        .expect("a user prompt after Resume should be accepted");
    assert!(
        matches!(dispatched, DispatchTurnResult::Dispatched(_)),
        "a resumed session dispatches a user prompt at once"
    );
    assert_eq!(
        session_record(&state, &parent_session_id).session.status,
        SessionStatus::Active
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
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
fn stop_on_an_idle_waiting_session_drops_continuations_and_keeps_other_prompts_paused() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.delegation_waits = vec![seeded_delegation_wait("wait-mixed", &session_id)];
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
fn one_stop_of_a_parked_admission_also_consumes_the_sessions_other_waits() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.delegation_waits = vec![seeded_delegation_wait("wait-still-pending", &session_id)];
        inner.test_run_waits = vec![test_run_wait_record("run-wait-still-pending", &session_id)];
        let index = inner.find_session_index(&session_id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        // One wait's continuation is parked in Engram admission at the head;
        // a second continuation queued behind it.
        queue_seeded_prompt(record, "parked-head", QueuedPromptSource::Orchestrator);
        queue_seeded_prompt(record, "later-workflow", QueuedPromptSource::Orchestrator);
        record.engram.admission_in_progress =
            Some(Arc::new(std::sync::atomic::AtomicBool::new(false)));
        state.commit_locked(&mut inner).unwrap();
    }
    let messages_before = session_record(&state, &session_id).session.messages.len();
    let mut delta_events = state.subscribe_delta_events();

    state
        .request_stop_session(&session_id)
        .expect("Stop of a parked admission should succeed");

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert!(
            inner.delegation_waits.is_empty() && inner.test_run_waits.is_empty(),
            "the same Stop consumes the session's other waits"
        );
    }
    let record = session_record(&state, &session_id);
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(record.engram.operator_paused);
    assert_eq!(
        record
            .queued_prompts
            .iter()
            .map(|queued| queued.pending_prompt.id.as_str())
            .collect::<Vec<_>>(),
        vec!["parked-head"],
        "the held admission head stays for its explicit cancel; later continuations go"
    );
    assert!(record.queued_prompts[0].engram_interrupted);
    assert!(
        record
            .session
            .preview
            .starts_with("Engram authorization canceled"),
        "the admission Stop's preview stands: {}",
        record.session.preview
    );
    assert_eq!(
        record.session.messages.len(),
        messages_before,
        "no second stopped message"
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
    assert_eq!(
        consumed_reasons,
        vec![
            "ParentSessionStopped".to_owned(),
            "SessionStopped".to_owned()
        ]
    );

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
        inner.delegation_waits = baseline_waits.clone();
        inner.test_run_waits = baseline_run_waits.clone();
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
