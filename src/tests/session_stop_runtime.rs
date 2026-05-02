// sharedcodexruntime + session-stop runtime-integration tests. deliberate
// split from tests/session_stop.rs: that sibling pins deferred-callback
// replay invariants in isolation (no real sharedcodexruntime, no helper
// process, no json-rpc), while THIS file pins end-to-end integration — real
// sharedcodexruntime wiring, real turn/interrupt command dispatch, real
// queued-prompt re-dispatch after a failed interrupt, real shared-process
// teardown on runtime exit. one codex app-server hosts N sessions: starting
// a session with a matching runtime-config attaches to the existing helper
// process, an incompatible config spawns a new one. stopping ONE shared
// session sends turn/interrupt over json-rpc and detaches locally rather
// than killing the os process. dedicated runtimes (claude cli = one process
// per session) behave oppositely — stop either kills the process cleanly or
// propagates the refusal as an api error, no silent detach. during shutdown
// the record keeps runtime_stop_in_progress = true so /api/state still
// renders the session with its old preview (ui spinner); meanwhile turn
// callbacks and codex thread-state updates are suppressed/deferred and a
// concurrent stop_session returns 409 conflict. interrupt failures must not
// leak: the session detaches locally, the shared process stays alive for
// its other tenants, and any queued prompt is dispatched into a fresh
// runtime so the user does not lose queued work. when the shared helper
// process itself exits, every hosted session has its runtime cleared and
// the os handle is reaped. production surfaces: AppState::stop_session,
// handle_shared_codex_stop_session, fail_turn_if_runtime_matches,
// handle_shared_codex_runtime_exit, runtime_stop_in_progress, detach_shared_codex_session.

use super::*;

fn seed_runtime_exit_active_turn_file_change(state: &AppState, session_id: &str) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(session_id)
        .expect("session should exist");
    let record = inner
        .session_mut_by_index(index)
        .expect("session index should be valid");
    record.active_turn_start_message_count = Some(record.session.messages.len());
    record.active_turn_file_changes.insert(
        "src/runtime.rs".to_owned(),
        WorkspaceFileChangeKind::Modified,
    );
}

// pins shared-runtime reuse: a second codex session started against the same
// already-running sharedcodexruntime attaches to the identical helper process
// and input channel rather than spawning a second codex app-server. guards
// against a regression where each session forks its own codex process,
// defeating the whole point of the shared app-server architecture.
#[test]
fn reuses_shared_codex_runtime_across_sessions() {
    let state = test_app_state();
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime.clone());

    let first = spawn_codex_runtime(state.clone(), "session-a".to_owned(), "/tmp".to_owned())
        .expect("first Codex handle should attach");
    let second = spawn_codex_runtime(state.clone(), "session-b".to_owned(), "/tmp".to_owned())
        .expect("second Codex handle should attach");

    assert_eq!(first.runtime_id, "shared-codex");
    assert_eq!(second.runtime_id, "shared-codex");
    assert!(Arc::ptr_eq(&first.process, &process));
    assert!(Arc::ptr_eq(&second.process, &process));
    let shared_sessions = runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    assert!(!shared_sessions.contains_key("session-a"));
    assert!(!shared_sessions.contains_key("session-b"));
    assert!(shared_sessions.is_empty());
}

// pins the happy-path shared-codex stop wire: stop_session on a shared-codex
// session emits a CodexRuntimeCommand::InterruptTurn carrying the exact
// thread_id/turn_id to the shared app-server, and on ack the session's
// runtime/thread entries are cleared while the helper process stays alive for
// its other tenants. guards against regressions that skip the turn/interrupt
// rpc or that kill the shared process instead of just detaching one session.
#[test]
fn stops_shared_codex_sessions_via_turn_interrupt() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, input_rx, process) = test_shared_codex_runtime("shared-codex-stop");
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-123".to_owned()),
                turn_id: Some("turn-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("thread-123".to_owned(), session_id.clone());

    let handle = CodexRuntimeHandle {
        runtime_id: runtime.runtime_id.clone(),
        input_tx: runtime.input_tx.clone(),
        process,
        shared_session: Some(SharedCodexSessionHandle {
            runtime: runtime.clone(),
            session_id: session_id.clone(),
        }),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(handle);
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Codex interrupt command should arrive");
        match command {
            CodexRuntimeCommand::InterruptTurn {
                thread_id,
                turn_id,
                response_tx,
            } => {
                assert_eq!(thread_id, "thread-123");
                assert_eq!(turn_id, "turn-123");
                let _ = response_tx.send(Ok(()));
            }
            _ => panic!("expected Codex turn interrupt command"),
        }
    });

    state.stop_session(&session_id).unwrap();
    let full_snapshot = state.full_snapshot();
    let session = full_snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("stopped session should remain present");
    assert_eq!(session.status, SessionStatus::Idle);
    assert!(session.external_session_id.is_none());
    assert!(session.codex_thread_state.is_none());
    assert!(session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn stopped by user."
    )));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert!(matches!(record.runtime, SessionRuntime::None));
    drop(inner);

    let shared_sessions = runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    assert!(!shared_sessions.contains_key(&session_id));
    drop(shared_sessions);
    assert!(
        !runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("thread-123")
    );
}

// pins the interrupt-failure isolation contract: when turn/interrupt cannot
// be delivered (channel closed, app-server rejected it), stop_session still
// detaches the session locally — clearing its sessions/thread_sessions entries
// and external_session_id both in-memory and on disk — while leaving the
// shared codex process alive for the other tenants. guards against a leak
// where a failed interrupt either kills the whole shared process or leaves
// the session stuck attached with a stale thread id.
#[test]
fn stop_session_detaches_shared_codex_session_when_interrupt_fails() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = SharedCodexRuntime {
        runtime_id: "shared-codex-stop-fail".to_owned(),
        input_tx,
        process: process.clone(),
        sessions: SharedCodexSessions::new(),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    drop(input_rx);

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-stop-fail".to_owned()),
                turn_id: Some("turn-stop-fail".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("thread-stop-fail".to_owned(), session_id.clone());

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
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(handle);
        inner.sessions[index].session.status = SessionStatus::Active;
        set_record_external_session_id(
            &mut inner.sessions[index],
            Some("thread-stop-fail".to_owned()),
        );
    }

    state.stop_session(&session_id).unwrap();
    let full_snapshot = state.full_snapshot();
    let session = full_snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("stopped session should remain present");
    assert_eq!(session.status, SessionStatus::Idle);
    assert!(session.external_session_id.is_none());
    assert!(session.codex_thread_state.is_none());
    assert!(session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn stopped by user."
    )));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(record.external_session_id.is_none());
    assert!(record.session.external_session_id.is_none());
    assert!(record.session.codex_thread_state.is_none());
    drop(inner);

    let reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    let reloaded = reloaded_inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should persist");
    assert!(reloaded.external_session_id.is_none());
    assert!(reloaded.session.external_session_id.is_none());
    assert!(reloaded.session.codex_thread_state.is_none());

    assert!(
        !runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned")
            .contains_key(&session_id)
    );
    assert!(
        !runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("thread-stop-fail")
    );

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// pins queued-prompt recovery after a failed shared-codex interrupt: even
// when turn/interrupt is rejected, the local detach proceeds AND any queued
// prompt on that session is dispatched as a fresh CodexRuntimeCommand::Prompt
// (with no resume_thread_id, since the old thread is gone) so the user does
// not lose the work they had queued. guards against a regression where a
// failed interrupt strands the queued prompt and the session lands idle.
#[test]
fn stop_session_dispatches_queued_prompt_after_shared_codex_interrupt_failure() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = SharedCodexRuntime {
        runtime_id: "shared-codex-stop-fail-queued".to_owned(),
        input_tx,
        process: process.clone(),
        sessions: SharedCodexSessions::new(),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
    };

    {
        let mut shared_runtime = state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        *shared_runtime = Some(runtime.clone());
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-stop-fail-queued".to_owned()),
                turn_id: Some("turn-stop-fail-queued".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("thread-stop-fail-queued".to_owned(), session_id.clone());

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
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(handle);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
        set_record_external_session_id(
            &mut inner.sessions[index],
            Some("thread-stop-fail-queued".to_owned()),
        );
        inner.sessions[index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::User,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-shared-stop-fail".to_owned(),
                    timestamp: stamp_now(),
                    text: "queued prompt after failed interrupt".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[index]);
    }

    let queued_session_id = session_id.clone();
    let command_thread = std::thread::spawn(move || {
        let interrupt = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Codex interrupt command should arrive");
        match interrupt {
            CodexRuntimeCommand::InterruptTurn {
                thread_id,
                turn_id,
                response_tx,
            } => {
                assert_eq!(thread_id, "thread-stop-fail-queued");
                assert_eq!(turn_id, "turn-stop-fail-queued");
                let _ = response_tx.send(Err("interrupt failed".to_owned()));
            }
            _ => panic!("expected Codex turn interrupt command"),
        }

        let prompt = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("queued Codex prompt should be dispatched");
        match prompt {
            CodexRuntimeCommand::Prompt {
                session_id,
                command,
            } => {
                assert_eq!(session_id, queued_session_id);
                assert_eq!(command.prompt, "queued prompt after failed interrupt");
                assert!(command.resume_thread_id.is_none());
            }
            _ => panic!("expected queued Codex prompt dispatch"),
        }
    });

    state.stop_session(&session_id).unwrap();
    command_thread
        .join()
        .expect("shared Codex command thread should join cleanly");

    let full_snapshot = state.full_snapshot();
    let session = full_snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should remain present");
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(session.preview, "queued prompt after failed interrupt");
    assert!(session.external_session_id.is_none());
    assert!(session.codex_thread_state.is_none());
    assert!(session.pending_prompts.is_empty());
    assert!(session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn stopped by user."
    )));
    assert!(session.messages.iter().any(|message| matches!(
        message,
        Message::Text {
            author: Author::You,
            text,
            ..
        } if text == "queued prompt after failed interrupt"
    )));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert_eq!(record.session.status, SessionStatus::Active);
    assert!(matches!(record.runtime, SessionRuntime::Codex(_)));
    assert!(!record.runtime_stop_in_progress);
    assert!(record.queued_prompts.is_empty());
    assert!(record.external_session_id.is_none());
    assert!(record.session.external_session_id.is_none());
    assert!(record.session.codex_thread_state.is_none());
    drop(inner);

    let reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    let reloaded = reloaded_inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should persist");
    assert!(reloaded.external_session_id.is_none());
    assert!(reloaded.session.external_session_id.is_none());
    assert!(reloaded.session.codex_thread_state.is_none());
    assert!(
        !runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("thread-stop-fail-queued")
    );

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// pins dedicated-runtime stop semantics: claude runs one process per session,
// so when the kill path fails stop_session must propagate a 500 error rather
// than silently detaching the way the shared codex path does — the session,
// its queued prompt, and its pending prompt all stay intact and no state
// event is broadcast. guards against dedicated sessions being quietly
// abandoned after a failed kill, which would leak the child process.
#[test]
fn stop_session_returns_an_error_when_a_dedicated_runtime_refuses_to_stop() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-fail".to_owned(),
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
        inner.sessions[index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::User,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-stop-follow-up".to_owned(),
                    timestamp: stamp_now(),
                    text: "queued prompt".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[index]);
    }

    let baseline_revision = state.full_snapshot().revision;
    let mut state_events = state.subscribe_events();
    let _failure_guard = force_test_kill_child_process_failure(&process, "Claude");
    let error = match state.stop_session(&session_id) {
        Ok(_) => panic!("failed dedicated runtime kills should not be treated as clean stops"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to stop session `"));
    assert_eq!(state.full_snapshot().revision, baseline_revision);
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
    assert_eq!(record.session.preview, "Ready for a prompt.");
    assert!(matches!(record.runtime, SessionRuntime::Claude(_)));
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(record.session.pending_prompts.len(), 1);
    drop(inner);

    assert!(input_rx.recv_timeout(Duration::from_millis(100)).is_err());

    process.kill().unwrap();
    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// pins the stopping-window ui contract: while stop_session is mid-flight the
// session record stays visible in /api/state with runtime_stop_in_progress =
// true and its original Active status + streaming preview intact, so the ui
// can render a stopping spinner without the row flickering or disappearing.
// once the handshake finishes status flips to idle with the stopped preview.
// guards against a regression that clears session state too eagerly.
#[test]
fn stop_session_keeps_the_previous_state_visible_until_shutdown_completes() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-concurrent-read".to_owned(),
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

    let stop_state = state.clone();
    let stop_session_id = session_id.clone();
    let stop_handle = std::thread::spawn(move || stop_state.stop_session(&stop_session_id));

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        {
            let inner = state.inner.lock().expect("state mutex poisoned");
            let record = inner
                .sessions
                .iter()
                .find(|record| record.session.id == session_id)
                .expect("Claude session should exist");
            if record.runtime_stop_in_progress {
                assert_eq!(record.session.status, SessionStatus::Active);
                assert_eq!(record.session.preview, "Streaming reply...");
                break;
            }
        }

        if std::time::Instant::now() >= deadline {
            panic!("stop_session did not enter the shutdown window in time");
        }

        std::thread::sleep(Duration::from_millis(5));
    }

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should still be visible while stopping");
    assert_eq!(session.status, SessionStatus::Active);
    assert_eq!(session.preview, "Streaming reply...");

    let stopped_snapshot = stop_handle
        .join()
        .expect("stop_session thread should join cleanly")
        .expect("stop_session should succeed");
    let stopped_session = stopped_snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("stopped session should remain present");
    assert_eq!(stopped_session.status, SessionStatus::Idle);
    assert_eq!(stopped_session.preview, "Turn stopped by user.");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert!(!record.runtime_stop_in_progress);
    assert!(matches!(record.runtime, SessionRuntime::None));
    drop(inner);

    assert!(input_rx.recv_timeout(Duration::from_millis(100)).is_err());

    process.wait().unwrap();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// pins concurrent-stop serialization: while one stop_session is holding
// runtime_stop_in_progress, a second stop_session on the same session short-
// circuits with 409 conflict and the message "session is already stopping"
// rather than racing the first stop or double-killing the child. guards
// against a regression that lets two stop flows run in parallel and corrupt
// the session state machine.
#[test]
fn stop_session_returns_conflict_when_already_stopping() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = ClaudeRuntimeHandle {
        runtime_id: "claude-stop-conflict".to_owned(),
        input_tx,
        process,
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

    let stop_state = state.clone();
    let stop_session_id = session_id.clone();
    let stop_handle = std::thread::spawn(move || stop_state.stop_session(&stop_session_id));

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let stop_in_progress = {
            let inner = state.inner.lock().expect("state mutex poisoned");
            inner
                .sessions
                .iter()
                .find(|record| record.session.id == session_id)
                .expect("Claude session should exist")
                .runtime_stop_in_progress
        };
        if stop_in_progress {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!("stop_session did not enter the shutdown window in time");
        }

        std::thread::sleep(Duration::from_millis(5));
    }

    let error = match state.stop_session(&session_id) {
        Ok(_) => panic!("a second stop should conflict while shutdown is in flight"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(error.message, "session is already stopping");

    let stopped_snapshot = stop_handle
        .join()
        .expect("stop_session thread should join cleanly")
        .expect("initial stop_session should succeed");
    let stopped_session = stopped_snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("stopped session should remain present");
    assert_eq!(stopped_session.status, SessionStatus::Idle);
    assert_eq!(stopped_session.preview, "Turn stopped by user.");

    assert!(input_rx.recv_timeout(Duration::from_millis(100)).is_err());

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// pins the suppression side of the stop-in-progress guard for turn callbacks:
// while runtime_stop_in_progress is set, fail_turn_if_runtime_matches /
// note_turn_retry_if_runtime_matches / mark_turn_error_if_runtime_matches /
// finish_turn_ok_if_runtime_matches must NOT mutate session state, bump
// revision, or broadcast — they only buffer onto deferred_stop_callbacks for
// the replay logic pinned in tests/session_stop.rs. guards against a mid-
// stop race that would flip status or leak messages as shutdown finalizes.
#[test]
fn runtime_turn_callbacks_are_suppressed_while_stop_is_in_progress() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, input_rx) = test_claude_runtime_handle("claude-stop-callback-guard");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
        inner.sessions[index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::User,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-stop-callback-guard".to_owned(),
                    timestamp: stamp_now(),
                    text: "queued prompt".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[index]);
        inner.sessions[index].runtime_stop_in_progress = true;
    }

    let baseline_revision = state.full_snapshot().revision;
    let mut state_events = state.subscribe_events();

    state
        .fail_turn_if_runtime_matches(&session_id, &runtime_token, "reader failure")
        .expect("fail_turn_if_runtime_matches should succeed");
    state
        .note_turn_retry_if_runtime_matches(&session_id, &runtime_token, "Retrying Claude...")
        .expect("note_turn_retry_if_runtime_matches should succeed");
    state
        .mark_turn_error_if_runtime_matches(&session_id, &runtime_token, "runtime error")
        .expect("mark_turn_error_if_runtime_matches should succeed");
    state
        .finish_turn_ok_if_runtime_matches(&session_id, &runtime_token)
        .expect("finish_turn_ok_if_runtime_matches should succeed");

    assert_eq!(state.full_snapshot().revision, baseline_revision);
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
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(record.session.pending_prompts.len(), 1);
    assert!(matches!(record.runtime, SessionRuntime::Claude(_)));
    assert!(record.runtime_stop_in_progress);
    assert_eq!(
        record.deferred_stop_callbacks,
        vec![
            DeferredStopCallback::TurnFailed("reader failure".to_owned()),
            DeferredStopCallback::TurnError("runtime error".to_owned()),
            DeferredStopCallback::TurnCompleted,
        ]
    );
    drop(inner);

    assert!(input_rx.recv_timeout(Duration::from_millis(100)).is_err());

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// pins the persist-fallback path for fail_turn_if_runtime_matches: even when
// the persistence directory cannot be written, the in-memory session must
// still flip to error with the failure preview and a "Turn failed: ..."
// message AND still publish a state snapshot over the event channel, so the
// ui shows the failure immediately. guards against a regression where a
// persistence failure silently swallows the turn-failure notification.
#[test]
fn fail_turn_if_runtime_matches_publishes_error_state_when_persist_fails() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, _input_rx) = test_claude_runtime_handle("claude-fail-turn-persist-fallback");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-fail-turn-persist-fallback-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }

    state.persistence_path = Arc::new(failing_persistence_path.clone());
    state.persist_tx = mpsc::channel().0;

    let baseline_revision = state.full_snapshot().revision;
    let mut state_events = state.subscribe_events();

    state
        .fail_turn_if_runtime_matches(&session_id, &runtime_token, "persist fallback failure")
        .expect("fail_turn_if_runtime_matches should publish even when persistence fails");

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(snapshot.revision, baseline_revision + 1);
    assert_eq!(session.status, SessionStatus::Error);
    assert_eq!(session.preview, "persist fallback failure");
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Turn failed: persist fallback failure"
    ));

    let published: StateResponse = serde_json::from_str(
        &state_events
            .try_recv()
            .expect("fail_turn_if_runtime_matches should publish a state snapshot"),
    )
    .expect("published state snapshot should decode");
    let published_session = published
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("published session should be present");
    assert_eq!(published.revision, snapshot.revision);
    assert_eq!(published_session.status, SessionStatus::Error);
    assert_eq!(published_session.preview, "persist fallback failure");
    assert!(!published_session.messages_loaded);
    assert!(published_session.messages.is_empty());
    assert_eq!(published_session.message_count, session.message_count);

    let _ = fs::remove_dir_all(failing_persistence_path);
}

#[test]
fn runtime_exit_restores_active_turn_file_tracking_when_persist_fails() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, _input_rx) =
        test_claude_runtime_handle("claude-runtime-exit-active-turn-rollback");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-runtime-exit-active-turn-rollback-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
    }
    seed_runtime_exit_active_turn_file_change(&state, &session_id);

    state.persistence_path = Arc::new(failing_persistence_path.clone());
    state.persist_tx = mpsc::channel().0;

    let error = state
        .handle_runtime_exit_if_matches(&session_id, &runtime_token, Some("runtime crashed"))
        .expect_err("runtime exit should report persistence failure");
    assert!(
        format!("{error:#}").contains("failed"),
        "unexpected runtime-exit error: {error:#}",
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.active_turn_start_message_count, Some(0));
    assert_eq!(
        record.active_turn_file_changes.get("src/runtime.rs"),
        Some(&WorkspaceFileChangeKind::Modified)
    );
    drop(inner);

    let _ = fs::remove_dir_all(failing_persistence_path);
}

// pins the suppression side of the stop-in-progress guard for codex thread-
// state updates: set_codex_thread_state_if_runtime_matches must NOT apply
// while runtime_stop_in_progress is set — no state mutation, no revision
// bump, no broadcast — so an archived/active thread-state signal racing the
// stop cannot overwrite the session's pre-stop thread state. guards against
// a mid-stop thread-state flip that would leak into the post-stop snapshot.
#[test]
fn codex_thread_state_updates_are_suppressed_while_stop_is_in_progress() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx) = test_codex_runtime_handle("codex-stop-thread-state-guard");
    let runtime_token = RuntimeToken::Codex(runtime.runtime_id.clone());
    state
        .set_external_session_id(&session_id, "thread-stop-guard".to_owned())
        .expect("Codex session should accept external thread ids");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
        inner.sessions[index].runtime_stop_in_progress = true;
    }

    let baseline_revision = state.full_snapshot().revision;
    let mut state_events = state.subscribe_events();

    state
        .set_codex_thread_state_if_runtime_matches(
            &session_id,
            &runtime_token,
            CodexThreadState::Archived,
        )
        .expect("set_codex_thread_state_if_runtime_matches should succeed");

    assert_eq!(state.full_snapshot().revision, baseline_revision);
    assert!(matches!(
        state_events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert_eq!(record.session.status, SessionStatus::Active);
    assert_eq!(record.session.preview, "Streaming reply...");
    assert_eq!(
        record.session.codex_thread_state,
        Some(CodexThreadState::Active)
    );
    assert!(matches!(record.runtime, SessionRuntime::Codex(_)));
    assert!(record.runtime_stop_in_progress);
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// pins the shared-process exit reaping contract: when the shared codex helper
// process exits (crash, sigterm, communication failure), every session hosted
// on that runtime has its SessionRuntime cleared and flips to error with the
// supplied diagnostic preview, the shared_codex_runtime slot is emptied so
// the next codex session spawns a fresh helper, and the os child handle is
// reaped. guards against zombie processes and sessions pointing at dead
// runtimes.
#[test]
fn shared_codex_runtime_exit_clears_state_and_kills_process() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = SharedCodexRuntime {
        runtime_id: "shared-codex-timeout".to_owned(),
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

    state
        .handle_shared_codex_runtime_exit(
            "shared-codex-timeout",
            Some("failed to communicate with shared Codex app-server"),
        )
        .expect("shared Codex runtime exit should succeed");

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Codex session should remain present");
    assert_eq!(session.status, SessionStatus::Error);
    assert!(
        session
            .preview
            .contains("failed to communicate with shared Codex app-server")
    );

    assert!(
        state
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned")
            .is_none()
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert!(matches!(record.runtime, SessionRuntime::None));
    drop(inner);

    let _ = process.kill();
    let _ = wait_for_shared_child_exit_timeout(
        &process,
        Duration::from_secs(3),
        "shared Codex runtime",
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}
