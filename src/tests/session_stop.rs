// sharedcodexruntime stop lifecycle + deferred-callback replay. one codex
// process hosts many sessions, so stopping a single session cannot kill the
// process — instead the per-session stop-in-progress guard on
// sharedcodexsessionstate serializes shutdown while the other sessions keep
// streaming. while that guard is set, incoming runtime signals for the
// stopping session (turn_completed, runtime_exit, ...) arriving through
// handle_shared_codex_turn_completed / handle_shared_codex_runtime_exit are
// deferred — buffered onto deferred_stop_callbacks — rather than applied
// immediately, because applying them mid-stop would race the stop machinery
// finalizing session state. on a clean stop the buffer is discarded (the
// session is gone, nothing left to do). on a FAILED dedicated stop the buffer
// is replayed in arrival order, with one fixup: runtimeexited is always
// replayed LAST even if it arrived first, otherwise it would tear down the
// runtime handle before a still-buffered turncompleted could use it. the
// shared stdin watchdog is cruder — a stalled writer wedges the whole codex
// process, so the watchdog clears the entire runtime, not just one session.

use super::*;

fn push_pending_approval_message(state: &AppState, session_id: &str) -> String {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(session_id)
        .expect("session should exist");
    let message_id = inner.next_message_id();
    let record = inner
        .session_mut_by_index(index)
        .expect("session index should be valid");
    push_message_on_record(
        record,
        Message::Approval {
            id: message_id.clone(),
            timestamp: stamp_now(),
            author: Author::Assistant,
            title: "Approve command".to_owned(),
            command: "echo pending".to_owned(),
            command_language: None,
            detail: "Waiting for approval".to_owned(),
            decision: ApprovalDecision::Pending,
        },
    );
    message_id
}

fn delta_stream_has_rejected_approval_update(
    delta_events: &mut broadcast::Receiver<String>,
    session_id: &str,
    message_id: &str,
) -> bool {
    let mut saw_update = false;
    while let Ok(payload) = delta_events.try_recv() {
        let event: DeltaEvent = serde_json::from_str(&payload).expect("delta should deserialize");
        saw_update |= matches!(
            event,
            DeltaEvent::MessageUpdated {
                session_id: delta_session_id,
                message_id: delta_message_id,
                message: Message::Approval {
                    decision: ApprovalDecision::Rejected,
                    ..
                },
                session_mutation_stamp: Some(_),
                ..
            } if delta_session_id == session_id && delta_message_id == message_id
        );
    }
    saw_update
}

fn drain_delta_events(delta_events: &mut broadcast::Receiver<String>) -> Vec<DeltaEvent> {
    let mut events = Vec::new();
    while let Ok(payload) = delta_events.try_recv() {
        events.push(serde_json::from_str(&payload).expect("delta should deserialize"));
    }
    events
}

// pins the coarse scope of the stdin watchdog: a stall on the shared codex
// writer clears the whole runtime (not just the stalled session) and marks
// the affected session as error with the generic "agent communication timed
// out" preview. guards against a future narrower fix that leaves the wedged
// process alive while only failing one session.
#[test]
fn shared_codex_stdin_watchdog_times_out_stalled_writer_and_clears_runtime() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = SharedCodexRuntime {
        runtime_id: "shared-codex-stdin-watchdog".to_owned(),
        input_tx,
        process: process.clone(),
        sessions: SharedCodexSessions::new(),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    let handle = CodexRuntimeHandle {
        runtime_id: runtime.runtime_id.clone(),
        input_tx: runtime.input_tx.clone(),
        process: process.clone(),
        shared_session: Some(SharedCodexSessionHandle {
            runtime: runtime.clone(),
            session_id: session_id.clone(),
        }),
    };

    {
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(handle);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }

    let activity: SharedCodexStdinActivityState =
        Arc::new(Mutex::new(Some(SharedCodexStdinActivity {
            operation: "flush",
            started_at: std::time::Instant::now() - Duration::from_millis(50),
            timed_out: false,
        })));
    let (_stop_tx, stop_rx) = mpsc::channel();
    spawn_shared_codex_stdin_watchdog(
        &state,
        &runtime.runtime_id,
        process.clone(),
        &activity,
        stop_rx,
        Duration::from_millis(10),
        Duration::from_millis(5),
    )
    .expect("shared Codex stdin watchdog should spawn");

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let cleared = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned")
            .is_none();
        if cleared {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "shared Codex stdin watchdog should tear down the stalled runtime"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Codex session should remain present");
    assert_eq!(session.status, SessionStatus::Error);
    assert!(
        session.preview.contains("Agent communication timed out"),
        "watchdog timeout should use generic message, got: {}",
        session.preview,
    );

    let _ = process.kill();
    let _ = wait_for_shared_child_exit_timeout(
        &process,
        Duration::from_secs(3),
        "shared Codex runtime",
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// pins the deferral side of the stop-in-progress guard: a runtime-exit
// arriving while runtime_stop_in_progress is set must not mutate the session,
// must not bump state revision, must not emit a broadcast event, and must be
// buffered onto deferred_stop_callbacks. guards against a regression where
// runtime-exit races the stop path and prematurely flips status to error.
#[test]
fn runtime_exit_is_suppressed_while_stop_is_in_progress() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, _input_rx) = test_claude_runtime_handle("claude-stop-exit-guard");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
        inner.sessions[index].runtime_stop_in_progress = true;
    }

    let baseline_revision = state.snapshot().revision;
    let mut state_events = state.subscribe_events();

    state
        .handle_runtime_exit_if_matches(&session_id, &runtime_token, Some("runtime exited"))
        .expect("handle_runtime_exit_if_matches should succeed");

    assert_eq!(state.snapshot().revision, baseline_revision);
    assert!(matches!(
        state_events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, SessionStatus::Active);
    assert_eq!(record.session.preview, "Streaming reply...");
    assert!(record.session.messages.is_empty());
    assert!(matches!(record.runtime, SessionRuntime::Claude(_)));
    assert!(record.runtime_stop_in_progress);
    assert_eq!(
        record.deferred_stop_callbacks,
        vec![DeferredStopCallback::RuntimeExited(Some(
            "runtime exited".to_owned()
        ))]
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn runtime_exit_publishes_message_updated_for_canceled_pending_interactions() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-runtime-exit-pending-update".to_owned(),
        input_tx,
        process: process.clone(),
    };
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Approval;
        inner.sessions[index].session.preview = "Waiting for approval...".to_owned();
    }
    let message_id = push_pending_approval_message(&state, &session_id);
    let mut delta_events = state.subscribe_delta_events();

    state
        .handle_runtime_exit_if_matches(&session_id, &runtime_token, Some("runtime exited"))
        .expect("handle_runtime_exit_if_matches should succeed");

    assert!(
        delta_stream_has_rejected_approval_update(&mut delta_events, &session_id, &message_id),
        "runtime exit should publish MessageUpdated for canceled pending approval"
    );

    let _ = process.kill();
    let _ = process.wait();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn runtime_exit_publishes_message_created_for_terminal_failure_after_pending_cancel() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-runtime-exit-terminal-created".to_owned(),
        input_tx,
        process: process.clone(),
    };
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Approval;
        inner.sessions[index].session.preview = "Waiting for approval...".to_owned();
    }
    let approval_message_id = push_pending_approval_message(&state, &session_id);
    let mut delta_events = state.subscribe_delta_events();

    state
        .handle_runtime_exit_if_matches(&session_id, &runtime_token, Some("runtime exited"))
        .expect("handle_runtime_exit_if_matches should succeed");

    let revision = state.snapshot().revision;
    let events = drain_delta_events(&mut delta_events);
    assert!(
        events.iter().any(|event| {
            matches!(
                event,
                DeltaEvent::MessageUpdated {
                    revision: event_revision,
                    session_id: delta_session_id,
                    message_id: delta_message_id,
                    message: Message::Approval {
                        decision: ApprovalDecision::Rejected,
                        ..
                    },
                    session_mutation_stamp: Some(_),
                    ..
                } if *event_revision == revision
                    && delta_session_id == &session_id
                    && delta_message_id == &approval_message_id
            )
        }),
        "runtime exit should still publish the canceled pending approval update"
    );
    assert!(
        events.iter().any(|event| {
            matches!(
                event,
                DeltaEvent::MessageCreated {
                    revision: event_revision,
                    session_id: delta_session_id,
                    message: Message::Text { text, .. },
                    session_mutation_stamp: Some(_),
                    ..
                } if *event_revision == revision
                    && delta_session_id == &session_id
                    && text == "Turn failed: runtime exited"
            )
        }),
        "runtime exit must delta-encode the appended terminal failure message"
    );

    let _ = process.kill();
    let _ = process.wait();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// pins the successful-stop path: when stop_session drives the dedicated kill
// to completion, any buffered deferred_stop_callbacks are dropped, the
// runtime detaches, and the session settles to idle with "turn stopped by
// user." guards against a replay-on-success bug that would double-apply
// turncompleted or runtimeexited after the stop already finalized state.
#[test]
fn successful_stop_discards_deferred_callbacks() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-discard-deferred".to_owned(),
        input_tx,
        process: process.clone(),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
        inner.sessions[index].deferred_stop_callbacks = vec![DeferredStopCallback::TurnCompleted];
    }

    let snapshot = state.stop_session(&session_id).unwrap();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("stopped session should remain present");
    assert_eq!(session.status, SessionStatus::Idle);
    assert_eq!(session.preview, "Turn stopped by user.");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert_eq!(record.session.preview, "Turn stopped by user.");
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.runtime_stop_in_progress);
    assert!(record.deferred_stop_callbacks.is_empty());
    drop(inner);

    assert!(input_rx.recv_timeout(Duration::from_millis(100)).is_err());
    process.wait().unwrap();

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn stop_session_publishes_message_updated_for_canceled_pending_interactions() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-pending-update".to_owned(),
        input_tx,
        process: process.clone(),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Approval;
        inner.sessions[index].session.preview = "Waiting for approval...".to_owned();
    }
    let message_id = push_pending_approval_message(&state, &session_id);
    let mut delta_events = state.subscribe_delta_events();

    state
        .stop_session(&session_id)
        .expect("stop_session should succeed");

    assert!(
        delta_stream_has_rejected_approval_update(&mut delta_events, &session_id, &message_id),
        "stop_session should publish MessageUpdated for canceled pending approval"
    );

    let _ = process.wait();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn stop_session_publishes_message_created_for_terminal_stop_after_pending_cancel() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-terminal-created".to_owned(),
        input_tx,
        process: process.clone(),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Approval;
        inner.sessions[index].session.preview = "Waiting for approval...".to_owned();
    }
    let approval_message_id = push_pending_approval_message(&state, &session_id);
    let mut delta_events = state.subscribe_delta_events();

    state
        .stop_session(&session_id)
        .expect("stop_session should succeed");

    let revision = state.snapshot().revision;
    let events = drain_delta_events(&mut delta_events);
    assert!(
        events.iter().any(|event| {
            matches!(
                event,
                DeltaEvent::MessageUpdated {
                    revision: event_revision,
                    session_id: delta_session_id,
                    message_id: delta_message_id,
                    message: Message::Approval {
                        decision: ApprovalDecision::Rejected,
                        ..
                    },
                    session_mutation_stamp: Some(_),
                    ..
                } if *event_revision == revision
                    && delta_session_id == &session_id
                    && delta_message_id == &approval_message_id
            )
        }),
        "stop_session should still publish the canceled pending approval update"
    );
    assert!(
        events.iter().any(|event| {
            matches!(
                event,
                DeltaEvent::MessageCreated {
                    revision: event_revision,
                    session_id: delta_session_id,
                    message: Message::Text { text, .. },
                    session_mutation_stamp: Some(_),
                    ..
                } if *event_revision == revision
                    && delta_session_id == &session_id
                    && text == "Turn stopped by user."
            )
        }),
        "stop_session must delta-encode the appended terminal stop message"
    );

    let _ = process.wait();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// pins failed-stop replay for turncompleted: when the dedicated kill fails
// and a turncompleted was buffered, replaying it must transition the session
// to idle and detach the runtime exactly as finish_turn_ok_if_runtime_matches
// would on the happy path. guards against silently swallowing the buffered
// callback when stop returns an error to the caller.
#[test]
fn failed_dedicated_stop_replays_deferred_turn_completion() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-replay".to_owned(),
        input_tx,
        process: process.clone(),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }

    // Pre-stage a deferred completion callback. In production this would be stored by
    // `finish_turn_ok_if_runtime_matches` arriving during the shutdown window; here we set it
    // directly because the forced kill failure completes synchronously with no observable window.
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].deferred_stop_callbacks = vec![DeferredStopCallback::TurnCompleted];
    }

    let _failure_guard = force_test_kill_child_process_failure(&process, "Claude");
    let error = match state.stop_session(&session_id) {
        Ok(_) => panic!("failed dedicated runtime kills should not be treated as clean stops"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to stop session `"));

    // The deferred callback should have been replayed: session should now be Idle with the
    // runtime detached, just as if `finish_turn_ok_if_runtime_matches` had run normally.
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(!record.runtime_stop_in_progress);
    assert!(record.deferred_stop_callbacks.is_empty());
    drop(inner);

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// pins failed-stop replay for runtimeexited with an error detail: the
// buffered exit callback must drive the session to status error with the
// original detail surfaced in the preview, clear the guard, and detach the
// runtime. guards against losing the recorded failure cause when stop itself
// fails and the exit signal was deferred behind the guard.
#[test]
fn failed_dedicated_stop_replays_deferred_runtime_exit() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-exit-replay".to_owned(),
        input_tx,
        process: process.clone(),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }

    // Pre-stage a deferred exit callback with an error message.
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].deferred_stop_callbacks = vec![DeferredStopCallback::RuntimeExited(
            Some("process crashed".to_owned()),
        )];
    }

    let _failure_guard = force_test_kill_child_process_failure(&process, "Claude");
    let error = match state.stop_session(&session_id) {
        Ok(_) => panic!("failed dedicated runtime kills should not be treated as clean stops"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);

    // The replayed exit callback should have transitioned the session to Error.
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, SessionStatus::Error);
    assert!(record.session.preview.contains("process crashed"));
    assert!(!record.runtime_stop_in_progress);
    assert!(record.deferred_stop_callbacks.is_empty());
    assert!(matches!(record.runtime, SessionRuntime::None));
    drop(inner);

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// pins the in-order replay invariant by building an "expected" reference
// state from a normal finish-then-exit sequence and requiring the
// failed-stop replay of [turncompleted, runtimeexited] to reach byte-for-byte
// the same status, preview, and message tail. guards against the replay path
// diverging from the canonical callback-order semantics over time.
#[test]
fn failed_dedicated_stop_replays_multiple_deferred_callbacks_in_order() {
    let expected_state = test_app_state();
    let expected_session_id = test_session_id(&expected_state, Agent::Claude);
    let expected_process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (expected_input_tx, _expected_input_rx) = mpsc::channel();
    let expected_runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-replay-order-expected".to_owned(),
        input_tx: expected_input_tx,
        process: expected_process.clone(),
    };
    let expected_token = RuntimeToken::Claude(expected_runtime.runtime_id.clone());

    let initial_message_count = {
        let mut inner = expected_state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&expected_session_id)
            .expect("Claude session should exist");
        let message_id = inner.next_message_id();
        inner.sessions[index].runtime = SessionRuntime::Claude(expected_runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Partial reply".to_owned();
        inner.sessions[index].session.messages.push(Message::Text {
            attachments: Vec::new(),
            id: message_id,
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: "Partial reply".to_owned(),
            expanded_text: None,
        });
        inner.sessions[index].session.messages.len()
    };

    expected_state
        .finish_turn_ok_if_runtime_matches(&expected_session_id, &expected_token)
        .expect("finish_turn_ok_if_runtime_matches should succeed");
    expected_state
        .handle_runtime_exit_if_matches(&expected_session_id, &expected_token, None)
        .expect("handle_runtime_exit_if_matches should succeed");

    let (expected_status, expected_preview, expected_message_count, expected_message_text) = {
        let inner = expected_state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == expected_session_id)
            .expect("Claude session should exist");
        let expected_message_text = match record.session.messages.last() {
            Some(Message::Text { text, .. }) => text.clone(),
            _ => panic!("Claude session should end with a text message"),
        };
        (
            record.session.status,
            record.session.preview.clone(),
            record.session.messages.len(),
            expected_message_text,
        )
    };
    assert_eq!(expected_status, SessionStatus::Idle);
    assert_eq!(expected_preview, "Partial reply");
    assert_eq!(expected_message_count, initial_message_count);
    assert_eq!(expected_message_text, "Partial reply");

    expected_process.kill().unwrap();
    expected_process.wait().unwrap();
    let _ = fs::remove_file(expected_state.persistence_path.as_path());

    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-replay-order".to_owned(),
        input_tx,
        process: process.clone(),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        let message_id = inner.next_message_id();
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Partial reply".to_owned();
        inner.sessions[index].session.messages.push(Message::Text {
            attachments: Vec::new(),
            id: message_id,
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: "Partial reply".to_owned(),
            expanded_text: None,
        });
        inner.sessions[index].deferred_stop_callbacks = vec![
            DeferredStopCallback::TurnCompleted,
            DeferredStopCallback::RuntimeExited(None),
        ];
    }

    let _failure_guard = force_test_kill_child_process_failure(&process, "Claude");
    let error = match state.stop_session(&session_id) {
        Ok(_) => panic!("failed dedicated runtime kills should not be treated as clean stops"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, expected_status);
    assert_eq!(record.session.preview, expected_preview);
    assert_eq!(record.session.messages.len(), expected_message_count);
    assert!(matches!(
        record.session.messages.last(),
        Some(Message::Text { text, .. }) if text == &expected_message_text
    ));
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.runtime_stop_in_progress);
    assert!(record.deferred_stop_callbacks.is_empty());
    drop(inner);

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// pins the runtime-exit-last reordering rule: even when the buffer is
// [runtimeexited, turncompleted] (exit arrived first), the replay must defer
// the exit to the end so turncompleted still has a live runtime handle to
// resolve against. compares against the same finish-then-exit expected state
// to catch any eager reordering that tears down the handle too early.
#[test]
fn failed_dedicated_stop_replays_runtime_exit_last_even_when_it_arrives_first() {
    let expected_state = test_app_state();
    let expected_session_id = test_session_id(&expected_state, Agent::Claude);
    let expected_process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (expected_input_tx, _expected_input_rx) = mpsc::channel();
    let expected_runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-replay-reversed-expected".to_owned(),
        input_tx: expected_input_tx,
        process: expected_process.clone(),
    };
    let expected_token = RuntimeToken::Claude(expected_runtime.runtime_id.clone());

    let initial_message_count = {
        let mut inner = expected_state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&expected_session_id)
            .expect("Claude session should exist");
        let message_id = inner.next_message_id();
        inner.sessions[index].runtime = SessionRuntime::Claude(expected_runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Partial reply".to_owned();
        inner.sessions[index].session.messages.push(Message::Text {
            attachments: Vec::new(),
            id: message_id,
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: "Partial reply".to_owned(),
            expanded_text: None,
        });
        inner.sessions[index].session.messages.len()
    };

    expected_state
        .finish_turn_ok_if_runtime_matches(&expected_session_id, &expected_token)
        .expect("finish_turn_ok_if_runtime_matches should succeed");
    expected_state
        .handle_runtime_exit_if_matches(&expected_session_id, &expected_token, None)
        .expect("handle_runtime_exit_if_matches should succeed");

    let (expected_status, expected_preview, expected_message_count, expected_message_text) = {
        let inner = expected_state.inner.lock().expect("state mutex poisoned");
        let record = inner
            .sessions
            .iter()
            .find(|record| record.session.id == expected_session_id)
            .expect("Claude session should exist");
        let expected_message_text = match record.session.messages.last() {
            Some(Message::Text { text, .. }) => text.clone(),
            _ => panic!("Claude session should end with a text message"),
        };
        (
            record.session.status,
            record.session.preview.clone(),
            record.session.messages.len(),
            expected_message_text,
        )
    };
    assert_eq!(expected_status, SessionStatus::Idle);
    assert_eq!(expected_preview, "Partial reply");
    assert_eq!(expected_message_count, initial_message_count);
    assert_eq!(expected_message_text, "Partial reply");

    expected_process.kill().unwrap();
    expected_process.wait().unwrap();
    let _ = fs::remove_file(expected_state.persistence_path.as_path());

    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-replay-reversed".to_owned(),
        input_tx,
        process: process.clone(),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        let message_id = inner.next_message_id();
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Partial reply".to_owned();
        inner.sessions[index].session.messages.push(Message::Text {
            attachments: Vec::new(),
            id: message_id,
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: "Partial reply".to_owned(),
            expanded_text: None,
        });
        inner.sessions[index].deferred_stop_callbacks = vec![
            DeferredStopCallback::RuntimeExited(None),
            DeferredStopCallback::TurnCompleted,
        ];
    }

    let _failure_guard = force_test_kill_child_process_failure(&process, "Claude");
    let error = match state.stop_session(&session_id) {
        Ok(_) => panic!("failed dedicated runtime kills should not be treated as clean stops"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, expected_status);
    assert_eq!(record.session.preview, expected_preview);
    assert_eq!(record.session.messages.len(), expected_message_count);
    assert!(matches!(
        record.session.messages.last(),
        Some(Message::Text { text, .. }) if text == &expected_message_text
    ));
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.runtime_stop_in_progress);
    assert!(record.deferred_stop_callbacks.is_empty());
    drop(inner);

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}
