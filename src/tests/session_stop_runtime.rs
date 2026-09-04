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

// pins terminal delegation cleanup: a completed read-only delegation child is
// no longer an active stop request, so cleanup must detach its shared-codex
// bookkeeping without sending turn/interrupt. This guards the completed-child
// recycling path from racing Codex app-server finalization after a normal turn
// completion.
#[test]
fn terminal_delegation_cleanup_detaches_shared_codex_child_without_interrupt() {
    let session_id = "delegation-child-shared-codex".to_owned();
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = SharedCodexRuntime {
        runtime_id: "shared-codex-terminal-delegation".to_owned(),
        input_tx,
        process: process.clone(),
        sessions: SharedCodexSessions::new(),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
        stdout_activity: Arc::new(Mutex::new(std::time::Instant::now())),
    };

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-terminal-delegation".to_owned()),
                turn_id: Some("turn-terminal-delegation".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("thread-terminal-delegation".to_owned(), session_id.clone());

    let handle = CodexRuntimeHandle {
        runtime_id: runtime.runtime_id.clone(),
        input_tx: runtime.input_tx.clone(),
        process: process.clone(),
        shared_session: Some(SharedCodexSessionHandle {
            runtime: runtime.clone(),
            session_id: session_id.clone(),
        }),
    };

    shutdown_terminal_delegation_child_runtime(
        KillableRuntime::Codex(handle),
        "terminal read-only delegation child",
    )
    .unwrap();

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
            .contains_key("thread-terminal-delegation")
    );
    assert!(matches!(
        input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(!shared_child_has_exited(&process, "shared Codex runtime").unwrap());

    process.kill().unwrap();
    process.wait().unwrap();
}

// pins the turn/completed self-deadlock escape hatch: the shared Codex event
// router owns runtime.sessions while applying finish_turn, and finish_turn can
// refresh a completed delegation. Natural terminal cleanup must not block on
// that same mutex before publishing the parent delegation result.
#[test]
fn terminal_delegation_cleanup_defers_shared_codex_detach_when_sessions_lock_is_held() {
    let session_id = "delegation-child-shared-codex-locked".to_owned();
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = SharedCodexRuntime {
        runtime_id: "shared-codex-terminal-delegation-locked".to_owned(),
        input_tx,
        process: process.clone(),
        sessions: SharedCodexSessions::new(),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
        stdout_activity: Arc::new(Mutex::new(std::time::Instant::now())),
    };

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("thread-terminal-delegation-locked".to_owned()),
                turn_id: Some("turn-terminal-delegation-locked".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert(
            "thread-terminal-delegation-locked".to_owned(),
            session_id.clone(),
        );

    let handle = CodexRuntimeHandle {
        runtime_id: runtime.runtime_id.clone(),
        input_tx: runtime.input_tx.clone(),
        process: process.clone(),
        shared_session: Some(SharedCodexSessionHandle {
            runtime: runtime.clone(),
            session_id: session_id.clone(),
        }),
    };

    let sessions_guard = runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    shutdown_terminal_delegation_child_runtime(
        KillableRuntime::Codex(handle),
        "terminal read-only delegation child",
    )
    .unwrap();
    assert!(sessions_guard.contains_key(&session_id));
    assert!(matches!(
        input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    drop(sessions_guard);

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let detached = {
            let sessions = runtime
                .sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            !sessions.contains_key(&session_id)
        } && !runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("thread-terminal-delegation-locked");

        if detached {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!("deferred shared Codex detach did not complete");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!shared_child_has_exited(&process, "shared Codex runtime").unwrap());

    process.kill().unwrap();
    process.wait().unwrap();
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
        stdout_activity: Arc::new(Mutex::new(std::time::Instant::now())),
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

// A rejected turn/interrupt still detaches the stale shared Codex thread, but
// explicit Stop must leave queued work paused. Otherwise the best-effort
// fallback immediately starts a fresh runtime and appears to ignore Stop.
#[test]
fn stop_session_pauses_queued_prompt_after_shared_codex_interrupt_failure() {
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
        stdout_activity: Arc::new(Mutex::new(std::time::Instant::now())),
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
                    source: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[index]);
    }

    let baseline_snapshot = state.full_snapshot();
    let baseline_revision = baseline_snapshot.revision;
    let baseline_message_count = baseline_snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .map(|session| session.message_count)
        .expect("queued session should exist before stop");
    let mut state_events = state.subscribe_events();
    let mut delta_events = state.subscribe_delta_events();

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

        assert!(matches!(
            input_rx.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
    });

    let stopped_snapshot = state.stop_session(&session_id).unwrap();
    command_thread
        .join()
        .expect("shared Codex command thread should join cleanly");

    assert_eq!(stopped_snapshot.revision, baseline_revision + 1);
    let stopped_session = stopped_snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("stopped session should remain present in the action response");
    assert_eq!(stopped_session.status, SessionStatus::Idle);
    assert_eq!(stopped_session.preview, "Turn stopped by user.");

    let published: StateResponse = serde_json::from_str(
        &state_events
            .try_recv()
            .expect("Stop should publish one idle state snapshot"),
    )
    .expect("published stop snapshot should decode");
    let published_session = published
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("published stop snapshot should retain the session");
    assert_eq!(published.revision, baseline_revision + 1);
    assert_eq!(published_session.status, SessionStatus::Idle);
    assert_eq!(published_session.preview, "Turn stopped by user.");
    assert!(matches!(
        state_events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    let mut published_deltas = Vec::new();
    while let Ok(payload) = delta_events.try_recv() {
        published_deltas
            .push(serde_json::from_str::<DeltaEvent>(&payload).expect("stop delta should decode"));
    }
    let created_deltas = published_deltas
        .iter()
        .filter_map(|event| match event {
            DeltaEvent::MessageCreated {
                revision,
                session_id: delta_session_id,
                message_id,
                message_count,
                message,
                preview,
                status,
                session_mutation_stamp,
                ..
            } if delta_session_id == &session_id => Some((
                *revision,
                message_id,
                *message_count,
                message,
                preview,
                *status,
                *session_mutation_stamp,
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(created_deltas.len(), 1, "only the stop message is appended");
    assert!(matches!(
        created_deltas[0].3,
        Message::Text {
            author: Author::Assistant,
            text,
            ..
        } if text == "Turn stopped by user."
    ));
    assert_eq!(created_deltas[0].2, baseline_message_count + 1);
    for (revision, _, _, _, preview, status, mutation_stamp) in created_deltas {
        assert_eq!(revision, stopped_snapshot.revision);
        assert_eq!(preview, &stopped_session.preview);
        assert_eq!(status, stopped_session.status);
        assert_eq!(mutation_stamp, stopped_session.session_mutation_stamp);
    }

    let full_snapshot = state.full_snapshot();
    let session = full_snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("session should remain present");
    assert_eq!(session.status, SessionStatus::Idle);
    assert_eq!(session.preview, "Turn stopped by user.");
    assert!(session.external_session_id.is_none());
    assert!(session.codex_thread_state.is_none());
    assert_eq!(
        session
            .pending_prompts
            .iter()
            .map(|prompt| prompt.text.as_str())
            .collect::<Vec<_>>(),
        vec!["queued prompt after failed interrupt"]
    );
    assert!(session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn stopped by user."
    )));
    assert!(!session.messages.iter().any(|message| matches!(
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
    assert_eq!(record.session.status, SessionStatus::Idle);
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.runtime_stop_in_progress);
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert_eq!(record.queued_prompts.len(), 1);
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
    assert!(reloaded.orchestrator_auto_dispatch_blocked);
    assert_eq!(reloaded.queued_prompts.len(), 1);
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

// A Stop owns a token-scoped fence while process shutdown runs off-lock. If a
// lower-level teardown clears that owner and a successor starts before Stop
// reacquires state, the stale Stop must not overwrite the successor runtime or
// consume its durable queue.
#[test]
fn stop_session_losing_ownership_preserves_successor_runtime_and_queue() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Cursor);
    let original_process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (original_input_tx, _original_input_rx) = mpsc::channel();
    let original_runtime = AcpRuntimeHandle {
        agent: AcpAgent::Cursor,
        runtime_id: "cursor-stop-owner-original".to_owned(),
        input_tx: original_input_tx,
        process: original_process.clone(),
        turn_lifecycle: Arc::new((Mutex::new(false), Condvar::new())),
    };
    let successor_process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (successor_input_tx, successor_input_rx) = mpsc::channel();
    let successor_runtime = AcpRuntimeHandle {
        agent: AcpAgent::Cursor,
        runtime_id: "cursor-stop-owner-successor".to_owned(),
        input_tx: successor_input_tx,
        process: successor_process.clone(),
        turn_lifecycle: Arc::new((Mutex::new(false), Condvar::new())),
    };
    let successor_token = RuntimeToken::Acp(successor_runtime.runtime_id.clone());
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Cursor session should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("Cursor session index should be valid");
        record.runtime = SessionRuntime::Acp(original_runtime);
        record.session.status = SessionStatus::Active;
        record.queued_prompts.push_back(QueuedPromptRecord {
            source: QueuedPromptSource::User,
            attachments: Vec::new(),
            pending_prompt: PendingPrompt {
                attachments: Vec::new(),
                id: "queued-after-stale-stop".to_owned(),
                timestamp: stamp_now(),
                text: "keep this queued prompt".to_owned(),
                expanded_text: None,
                source: None,
            },
        });
        sync_pending_prompts(record);
        state
            .commit_locked(&mut inner)
            .expect("original runtime should persist");
    }

    let stop_gate = install_test_stop_fence_gate(&state, &session_id);
    let stop_state = state.clone();
    let stop_session_id = session_id.clone();
    let stop_thread = std::thread::spawn(move || {
        stop_state.stop_session_with_options(
            &stop_session_id,
            StopSessionOptions {
                dispatch_queued_prompts_on_success: true,
                pause_automatic_resumes_on_success: false,
                orchestrator_stop_instance_id: None,
            },
        )
    });
    stop_gate.wait_until_claimed();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Cursor session should remain");
        let record = inner
            .session_mut_by_index(index)
            .expect("Cursor session index should be valid");
        record.clear_runtime_stop();
        record.runtime = SessionRuntime::Acp(successor_runtime);
        record.session.status = SessionStatus::Active;
        state
            .commit_locked(&mut inner)
            .expect("successor runtime should persist");
    }
    stop_gate.release();
    stop_thread
        .join()
        .expect("Stop thread should finish")
        .expect("stale Stop should return the current snapshot");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .find_session_index(&session_id)
        .and_then(|index| inner.sessions.get(index))
        .expect("successor session should remain");
    assert!(record.runtime.matches_runtime_token(&successor_token));
    assert_eq!(record.session.status, SessionStatus::Active);
    assert_eq!(record.queued_prompts.len(), 1);
    assert!(!record.runtime_stop_in_progress);
    assert!(matches!(
        successor_input_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    drop(inner);

    original_process
        .wait()
        .expect("the original process should be reaped");
    successor_process
        .kill()
        .expect("the successor process should clean up");
    successor_process
        .wait()
        .expect("the successor process should be reaped");
}

// A failed persistence commit happens after the queued successor was prepared
// but before its dispatch was delivered. The speculative runtime and user
// message must be removed while the coherent post-stop Idle record, including
// the original FIFO queue, stays available for explicit recovery.
#[test]
fn stop_session_rolls_back_queued_successor_when_persist_fails() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Cursor);
    let original_process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (original_input_tx, original_input_rx) = mpsc::channel();
    let original_runtime = AcpRuntimeHandle {
        agent: AcpAgent::Cursor,
        runtime_id: "cursor-stop-persist-original".to_owned(),
        input_tx: original_input_tx,
        process: original_process.clone(),
        turn_lifecycle: Arc::new((Mutex::new(false), Condvar::new())),
    };
    let successor_process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (successor_input_tx, successor_input_rx) = mpsc::channel();
    let successor_runtime = AcpRuntimeHandle {
        agent: AcpAgent::Cursor,
        runtime_id: "cursor-stop-persist-successor".to_owned(),
        input_tx: successor_input_tx,
        process: successor_process.clone(),
        turn_lifecycle: Arc::new((Mutex::new(false), Condvar::new())),
    };
    state.install_test_acp_runtime_override(AcpAgent::Cursor, successor_runtime);

    let queued = [
        ("queued-persist-first", "first queued prompt"),
        ("queued-persist-second", "second queued prompt"),
    ];
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Cursor session should exist");
        inner.sessions[index].runtime = SessionRuntime::Acp(original_runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
        for (id, text) in queued {
            inner.sessions[index]
                .queued_prompts
                .push_back(QueuedPromptRecord {
                    source: QueuedPromptSource::User,
                    attachments: Vec::new(),
                    pending_prompt: PendingPrompt {
                        attachments: Vec::new(),
                        id: id.to_owned(),
                        timestamp: stamp_now(),
                        text: text.to_owned(),
                        expanded_text: None,
                        source: None,
                    },
                });
        }
        sync_pending_prompts(&mut inner.sessions[index]);
    }

    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-stop-queued-successor-rollback-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");
    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(failing_persistence_path.clone());

    let baseline_revision = state.full_snapshot().revision;
    let mut state_events = state.subscribe_events();
    let mut delta_events = state.subscribe_delta_events();
    let error = match state.stop_session_with_options(
        &session_id,
        StopSessionOptions {
            dispatch_queued_prompts_on_success: true,
            pause_automatic_resumes_on_success: false,
            orchestrator_stop_instance_id: None,
        },
    ) {
        Ok(_) => panic!("persistence failure should reject stop"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to persist session state"));

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("stopped session should remain present");
    assert_eq!(snapshot.revision, baseline_revision + 1);
    assert_eq!(session.status, SessionStatus::Idle);
    assert_eq!(session.preview, "Turn stopped by user.");
    assert_eq!(
        session
            .pending_prompts
            .iter()
            .map(|pending| pending.text.as_str())
            .collect::<Vec<_>>(),
        vec!["first queued prompt", "second queued prompt"]
    );
    assert!(session.messages.iter().any(|message| {
        matches!(message, Message::Text { text, .. } if text == "Turn stopped by user.")
    }));
    assert!(!session.messages.iter().any(|message| {
        matches!(
            message,
            Message::Text {
                author: Author::You,
                text,
                ..
            } if text == "first queued prompt" || text == "second queued prompt"
        )
    }));
    assert!(matches!(
        state_events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    assert!(matches!(
        delta_events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Cursor session should remain present");
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert_eq!(record.active_turn_start_message_count, None);
    assert_eq!(
        record
            .queued_prompts
            .iter()
            .map(|queued| queued.pending_prompt.text.as_str())
            .collect::<Vec<_>>(),
        vec!["first queued prompt", "second queued prompt"]
    );
    drop(inner);

    assert!(matches!(
        original_input_rx.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    ));
    assert!(matches!(
        successor_input_rx.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    ));
    original_process
        .wait()
        .expect("original Cursor runtime should be stopped");
    successor_process
        .wait()
        .expect("uncommitted successor runtime should be stopped");
    let _ = fs::remove_dir_all(failing_persistence_path);
}

// A failed Stop commit must not silently consume the parent's pending
// delegation waits. The runtime cannot be resurrected after it has been
// interrupted, but every durable coordination record must remain available for
// a later successful retry.
#[test]
fn stop_session_restores_parent_delegation_wait_when_persist_fails() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Cursor);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = AcpRuntimeHandle {
        agent: AcpAgent::Cursor,
        runtime_id: "cursor-stop-wait-persist-failure".to_owned(),
        input_tx,
        process: process.clone(),
        turn_lifecycle: Arc::new((Mutex::new(false), Condvar::new())),
    };
    let stopped_parent_wait_id = "delegation-wait-stop-persist-failure".to_owned();
    let baseline_waits = vec![
        DelegationWaitRecord {
            id: "delegation-wait-other-parent".to_owned(),
            parent_session_id: "session-other-parent".to_owned(),
            delegation_ids: vec!["delegation-other-parent".to_owned()],
            mode: DelegationWaitMode::All,
            created_at: stamp_now(),
            title: Some("Other parent wait".to_owned()),
        },
        DelegationWaitRecord {
            id: stopped_parent_wait_id.clone(),
            parent_session_id: session_id.clone(),
            delegation_ids: vec!["delegation-stop-persist-failure".to_owned()],
            mode: DelegationWaitMode::All,
            created_at: stamp_now(),
            title: Some("Stop persistence rollback".to_owned()),
        },
        DelegationWaitRecord {
            id: "delegation-wait-stopped-parent-second".to_owned(),
            parent_session_id: session_id.clone(),
            delegation_ids: vec!["delegation-stop-persist-failure-second".to_owned()],
            mode: DelegationWaitMode::Any,
            created_at: stamp_now(),
            title: Some("Second stopped-parent wait".to_owned()),
        },
    ];
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Cursor session should exist");
        inner.sessions[index].runtime = SessionRuntime::Acp(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.delegation_waits = baseline_waits.clone();
        state.commit_locked(&mut inner).unwrap();
    }

    let failing_persistence_path =
        std::env::temp_dir().join(format!("termal-stop-wait-rollback-{}", Uuid::new_v4()));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");
    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(failing_persistence_path.clone());

    let mut delta_events = state.subscribe_delta_events();
    let error = match state.stop_session(&session_id) {
        Ok(_) => panic!("persistence failure should reject stop"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("failed to persist session state"));

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(
        inner.delegation_waits, baseline_waits,
        "a failed Stop commit must restore every pending wait in its original order"
    );
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("stopped parent should remain present");
    assert_eq!(parent.session.status, SessionStatus::Idle);
    assert!(parent.orchestrator_auto_dispatch_blocked);
    drop(inner);
    assert!(matches!(
        delta_events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    assert!(matches!(
        input_rx.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    ));
    process
        .wait()
        .expect("original Cursor runtime should be stopped");
    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Pins the failure half of the atomic stop-plus-queue transition. If the
// queued successor cannot start, Stop still publishes one coherent Idle
// snapshot, keeps the prompt queued, and clears the prospective turn boundary
// that start_turn_on_record records before fallible validation.
#[test]
fn stop_session_keeps_queued_prompt_idle_when_successor_start_fails() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Cursor);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = AcpRuntimeHandle {
        agent: AcpAgent::Cursor,
        runtime_id: "cursor-stop-queued-successor-failure".to_owned(),
        input_tx,
        process,
        turn_lifecycle: Arc::new((Mutex::new(false), Condvar::new())),
    };
    let image = MessageImageAttachment {
        byte_size: 4,
        file_name: "queued.png".to_owned(),
        media_type: "image/png".to_owned(),
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Cursor session should exist");
        inner.sessions[index].runtime = SessionRuntime::Acp(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Streaming reply...".to_owned();
        inner.sessions[index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::User,
                attachments: vec![PromptImageAttachment {
                    data: "data".to_owned(),
                    metadata: image.clone(),
                }],
                pending_prompt: PendingPrompt {
                    attachments: vec![image],
                    id: "queued-cursor-stop-failure".to_owned(),
                    timestamp: stamp_now(),
                    text: "queued prompt with unsupported image".to_owned(),
                    expanded_text: None,
                    source: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[index]);
    }

    let baseline_revision = state.full_snapshot().revision;
    let mut state_events = state.subscribe_events();
    let error = match state.stop_session_with_options(
        &session_id,
        StopSessionOptions {
            dispatch_queued_prompts_on_success: true,
            pause_automatic_resumes_on_success: false,
            orchestrator_stop_instance_id: None,
        },
    ) {
        Ok(_) => panic!("unsupported queued attachments should fail successor start"),
        Err(error) => error,
    };
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("do not support image attachments"));

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("failed queued successor should retain the session");
    assert_eq!(snapshot.revision, baseline_revision + 1);
    assert_eq!(session.status, SessionStatus::Idle);
    assert_eq!(session.preview, "Turn stopped by user.");

    let published: StateResponse = serde_json::from_str(
        &state_events
            .try_recv()
            .expect("failed queued successor should publish the Idle stop"),
    )
    .expect("published failed-successor snapshot should decode");
    let published_session = published
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("published failed-successor snapshot should retain the session");
    assert_eq!(published.revision, baseline_revision + 1);
    assert_eq!(published_session.status, SessionStatus::Idle);
    assert!(matches!(
        state_events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Cursor session should remain present");
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(record.session.pending_prompts.len(), 1);
    assert_eq!(record.active_turn_start_message_count, None);
    drop(inner);

    assert!(matches!(
        input_rx.try_recv(),
        Err(mpsc::TryRecvError::Disconnected)
    ));
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
                    source: None,
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
                    source: None,
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
    let retry_admitted = state
        .note_turn_retry_if_runtime_matches(&session_id, &runtime_token, "Retrying Claude...")
        .expect("note_turn_retry_if_runtime_matches should succeed");
    assert!(!retry_admitted);
    assert!(!state.turn_retry_allowed_if_runtime_matches(&session_id, &runtime_token));
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
            DeferredStopCallback::TurnFailed {
                active_turn_generation: 0,
                message: "reader failure".to_owned(),
            },
            DeferredStopCallback::TurnError {
                active_turn_generation: 0,
                message: "runtime error".to_owned(),
            },
            DeferredStopCallback::TurnCompleted {
                active_turn_generation: 0,
            },
        ]
    );
    drop(inner);

    assert!(input_rx.recv_timeout(Duration::from_millis(100)).is_err());

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Shared Codex acknowledges the channel handoff before its writer receives the
// turn/start response. If the old runtime handle disappears in between, the
// token guard is stale but an Active+None record still belongs to that failed
// turn and must not remain live forever.
#[test]
fn shared_codex_prompt_failure_terminalizes_active_session_after_runtime_loss() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let stale_runtime_token = RuntimeToken::Codex("lost-shared-runtime".to_owned());

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::None;
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "LIVE TURN".to_owned();
    }

    handle_shared_codex_prompt_command_result(
        &state,
        &session_id,
        &stale_runtime_token,
        0,
        Err(anyhow::Error::new(CodexResponseError::JsonRpc(
            "turn/start rejected after runtime detach".to_owned(),
        ))),
    )
    .expect("prompt failure handling should succeed");

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("failed session should remain visible");
    assert_eq!(session.status, SessionStatus::Error);
    assert_eq!(session.preview, "turn/start rejected after runtime detach");

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// A stale failure from an old shared runtime must never terminalize a live
// successor. The missing-runtime fallback above is intentionally narrower
// than the ordinary token guard.
#[test]
fn shared_codex_prompt_failure_does_not_touch_a_successor_runtime() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let stale_runtime_token = RuntimeToken::Codex("stale-shared-runtime".to_owned());
    let (replacement_runtime, _input_rx, process) =
        test_shared_codex_runtime("replacement-shared-runtime");
    let replacement_handle = CodexRuntimeHandle {
        runtime_id: replacement_runtime.runtime_id.clone(),
        input_tx: replacement_runtime.input_tx.clone(),
        process: process.clone(),
        shared_session: None,
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(replacement_handle);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "successor turn".to_owned();
    }

    handle_shared_codex_prompt_command_result(
        &state,
        &session_id,
        &stale_runtime_token,
        0,
        Err(anyhow::Error::new(CodexResponseError::JsonRpc(
            "stale turn/start rejection".to_owned(),
        ))),
    )
    .expect("stale prompt failure handling should no-op");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert_eq!(record.session.status, SessionStatus::Active);
    assert_eq!(record.session.preview, "successor turn");
    assert!(record.runtime.matches_runtime_token(&RuntimeToken::Codex(
        "replacement-shared-runtime".to_owned()
    )));
    drop(inner);

    process.kill().ok();
    process.wait().ok();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Shared Codex keeps one process/runtime id across turns. Token equality alone
// therefore cannot distinguish a late turn/start rejection from a successor
// turn on that same app-server; the process-local turn generation must also
// match.
#[test]
fn shared_codex_prompt_failure_does_not_touch_a_same_runtime_successor_generation() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) = test_shared_codex_runtime("same-shared-runtime-successor");
    let runtime_token = RuntimeToken::Codex(runtime.runtime_id.clone());
    let runtime_handle = CodexRuntimeHandle {
        runtime_id: runtime.runtime_id.clone(),
        input_tx: runtime.input_tx.clone(),
        process: process.clone(),
        shared_session: None,
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        let record = inner
            .session_mut_by_index(index)
            .expect("Codex session index should be valid");
        record.runtime = SessionRuntime::Codex(runtime_handle);
        record.active_turn_generation = 2;
        record.session.status = SessionStatus::Active;
        record.session.preview = "same-runtime successor".to_owned();
    }
    let baseline_revision = state.full_snapshot().revision;

    handle_shared_codex_prompt_command_result(
        &state,
        &session_id,
        &runtime_token,
        1,
        Err(anyhow::Error::new(CodexResponseError::JsonRpc(
            "stale rejection from prior generation".to_owned(),
        ))),
    )
    .expect("stale prompt failure should no-op");

    assert_eq!(state.full_snapshot().revision, baseline_revision);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should remain present");
    assert!(record.runtime.matches_runtime_token(&runtime_token));
    assert_eq!(record.active_turn_generation, 2);
    assert_eq!(record.session.status, SessionStatus::Active);
    assert_eq!(record.session.preview, "same-runtime successor");
    assert!(!record.runtime_stop_in_progress);
    drop(inner);

    process.kill().ok();
    process.wait().ok();
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delayed_claude_retry_is_dropped_during_stop_and_after_runtime_replacement() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, _runtime_input_rx) = test_claude_runtime_handle("claude-delayed-retry-original");
    let stale_runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());
    let (retry_tx, retry_rx) = mpsc::channel();
    let replay_prompt = Arc::new(Mutex::new(Some(ClaudePromptCommand {
        attachments: Vec::new(),
        replay_generation: "retry-generation-stop".to_owned(),
        text: "retry me".to_owned(),
    })));

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].runtime_stop_in_progress = true;
    }

    assert!(!dispatch_claude_retry_if_current(
        &state,
        &session_id,
        &stale_runtime_token,
        &retry_tx,
        &replay_prompt,
        "retry-generation-stop",
        "Retrying Claude automatically.",
    ));
    assert!(matches!(
        retry_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert_eq!(claude_replay_generation(&replay_prompt), None);

    let (replacement_runtime, _replacement_input_rx) =
        test_claude_runtime_handle("claude-delayed-retry-replacement");
    let replacement_runtime_token = RuntimeToken::Claude(replacement_runtime.runtime_id.clone());
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(replacement_runtime);
        inner.sessions[index].runtime_stop_in_progress = false;
    }
    *replay_prompt
        .lock()
        .expect("Claude replay prompt mutex poisoned") = Some(ClaudePromptCommand {
        attachments: Vec::new(),
        replay_generation: "retry-generation-replaced".to_owned(),
        text: "stale retry".to_owned(),
    });

    assert!(!dispatch_claude_retry_if_current(
        &state,
        &session_id,
        &stale_runtime_token,
        &retry_tx,
        &replay_prompt,
        "retry-generation-replaced",
        "Retrying Claude automatically.",
    ));
    assert!(matches!(
        retry_rx.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert_eq!(claude_replay_generation(&replay_prompt), None);

    let messages_before_idle_retry = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].session.status = SessionStatus::Idle;
        inner.sessions[index].session.messages.len()
    };
    *replay_prompt
        .lock()
        .expect("Claude replay prompt mutex poisoned") = Some(ClaudePromptCommand {
        attachments: Vec::new(),
        replay_generation: "retry-generation-idle".to_owned(),
        text: "do not resurrect this turn".to_owned(),
    });
    assert!(!dispatch_claude_retry_if_current(
        &state,
        &session_id,
        &replacement_runtime_token,
        &retry_tx,
        &replay_prompt,
        "retry-generation-idle",
        "This message must not be recorded.",
    ));
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        assert_eq!(inner.sessions[index].session.status, SessionStatus::Idle);
        assert_eq!(
            inner.sessions[index].session.messages.len(),
            messages_before_idle_retry
        );
    }
    assert_eq!(claude_replay_generation(&replay_prompt), None);

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

    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(failing_persistence_path.clone());

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
    assert_eq!(published_session.message_count, session.message_count);

    let _ = fs::remove_dir_all(failing_persistence_path);
}

#[test]
fn runtime_exit_clears_active_turn_file_tracking_when_persist_fails() {
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

    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(failing_persistence_path.clone());

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
    assert_eq!(record.session.status, SessionStatus::Error);
    assert!(record.session.preview.contains("runtime crashed"));
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.runtime_reset_required);
    assert!(!record.runtime_stop_in_progress);
    assert!(record.deferred_stop_callbacks.is_empty());
    assert!(record.session.messages.iter().any(|message| {
        matches!(message, Message::Text { text, .. } if text == "Turn failed: runtime crashed")
    }));
    assert!(record.active_turn_start_message_count.is_none());
    assert!(record.active_turn_file_changes.is_empty());
    assert!(record.active_turn_file_change_grace_deadline.is_none());
    drop(inner);

    let _ = fs::remove_dir_all(failing_persistence_path);
}

#[test]
fn engram_mcp_revocation_publishes_terminal_state_when_persist_fails() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, _input_rx) =
        test_claude_runtime_handle("claude-engram-revocation-persist-failure");
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-engram-revocation-persist-failure-{}",
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
    let mut batch = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        claim_engram_mcp_runtime_revocations_locked(&mut inner, std::slice::from_ref(&session_id))
    };
    assert!(batch.pending_session_ids.is_empty());
    let target = batch
        .targets
        .pop()
        .expect("Claude runtime should be claimable");

    state.shutdown_persist_blocking();
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    let mut state_events = state.subscribe_events();

    let finalization = state.finish_revoked_engram_mcp_runtime_if_matches(
        &session_id,
        &target.token,
        target.owner_generation,
        target.stop_options.as_ref(),
        false,
        false,
        None,
    );
    let error = finalization.failures.join("; ");
    assert!(
        error.contains("failed to persist revocation state"),
        "unexpected revocation error: {error}"
    );

    let published: StateResponse = serde_json::from_str(
        &state_events
            .try_recv()
            .expect("persist failure should publish the in-memory terminal state"),
    )
    .expect("published revocation snapshot should decode");
    let published_session = published
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("published session should remain");
    assert_eq!(published_session.status, SessionStatus::Idle);
    assert_eq!(
        published_session.preview,
        "Turn stopped: Engram MCP configuration was revoked."
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should remain");
    assert!(matches!(record.runtime, SessionRuntime::None));
    assert!(!record.runtime_stop_in_progress);
    assert!(record.orchestrator_auto_dispatch_blocked);
    drop(inner);

    let _ = fs::remove_dir_all(failing_persistence_path);
}

#[test]
fn message_less_atomic_terminalization_publishes_the_error_state() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::None;
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "accepted turn".to_owned();
    }
    let mut state_events = state.subscribe_events();

    assert!(
        state
            .fail_active_turn_if_runtime_missing(&session_id, 0, "   ")
            .expect("missing-runtime recovery should succeed")
    );

    let published: StateResponse = serde_json::from_str(
        &state_events
            .try_recv()
            .expect("message-less terminal transition should publish full state"),
    )
    .expect("published state should decode");
    let session = published
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("terminalized session should be published");
    assert_eq!(session.status, SessionStatus::Error);
}

#[test]
fn stale_matching_terminalization_does_not_replay_into_same_token_successor() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, _input_rx) = test_claude_runtime_handle("stale-terminalization-owner");
    let token = RuntimeToken::Claude(runtime.runtime_id.clone());
    let (successor_runtime, _successor_input_rx) =
        test_claude_runtime_handle("stale-terminalization-owner");
    let successor_token = RuntimeToken::Claude(successor_runtime.runtime_id.clone());
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should exist");
        inner.sessions[index].runtime = SessionRuntime::Claude(runtime);
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].active_turn_generation = 7;
    }
    let owner_generation = state
        .claim_turn_terminalization_if_runtime_matches(&session_id, &token, 7)
        .expect("terminalization claim should succeed")
        .expect("matching runtime should be claimed");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Claude session should remain");
        inner.sessions[index].runtime = SessionRuntime::Claude(successor_runtime);
        inner.sessions[index].active_turn_generation = 8;
        inner.sessions[index]
            .deferred_stop_callbacks
            .push(DeferredStopCallback::TurnCompleted {
                active_turn_generation: 7,
            });
    }

    assert!(
        !state
            .fail_turn_and_clear_runtime_if_owned(
                &session_id,
                &token,
                7,
                owner_generation,
                "stale terminalization",
            )
            .expect("stale terminalization should no-op")
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should remain");
    assert!(!record.runtime_stop_in_progress);
    assert!(record.runtime_stop_owner.is_none());
    assert!(record.runtime.matches_runtime_token(&successor_token));
    assert_eq!(record.active_turn_generation, 8);
    assert_eq!(record.session.status, SessionStatus::Active);
    assert!(record.deferred_stop_callbacks.is_empty());
}

#[test]
fn runtime_exit_success_keeps_active_turn_file_grace_window() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let (runtime, _input_rx) =
        test_claude_runtime_handle("claude-runtime-exit-active-turn-success");
    let runtime_token = RuntimeToken::Claude(runtime.runtime_id.clone());

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

    state
        .handle_runtime_exit_if_matches(&session_id, &runtime_token, Some("runtime crashed"))
        .expect("runtime exit should persist successfully");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Claude session should exist");
    assert_eq!(record.session.status, SessionStatus::Error);
    assert!(record.session.preview.contains("runtime crashed"));
    assert!(record.active_turn_start_message_count.is_none());
    assert!(record.active_turn_file_changes.is_empty());
    assert!(record.active_turn_file_change_grace_deadline.is_some());
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
// on that runtime has its SessionRuntime cleared. Active/approval sessions flip
// to error with the supplied diagnostic preview, idle sessions remain idle so
// their next prompt can spawn a fresh helper. The shared_codex_runtime slot is
// emptied and the os child handle is reaped. Guards against zombie processes,
// sessions pointing at dead runtimes, and idle Codex tabs being poisoned by an
// unrelated active turn's shared-process failure.
#[test]
fn shared_codex_runtime_exit_clears_state_and_kills_process() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let idle_session_id = test_session_id(&state, Agent::Codex);
    let process = Arc::new(SharedChild::new(test_sleep_child()).unwrap());
    let (input_tx, _input_rx) = mpsc::channel();
    let runtime = SharedCodexRuntime {
        runtime_id: "shared-codex-timeout".to_owned(),
        input_tx,
        process: process.clone(),
        sessions: SharedCodexSessions::new(),
        thread_sessions: Arc::new(Mutex::new(HashMap::new())),
        stdout_activity: Arc::new(Mutex::new(std::time::Instant::now())),
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
    let idle_handle = CodexRuntimeHandle {
        runtime_id: runtime.runtime_id.clone(),
        input_tx: runtime.input_tx.clone(),
        process: process.clone(),
        shared_session: Some(SharedCodexSessionHandle {
            runtime: runtime.clone(),
            session_id: idle_session_id.clone(),
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

        let idle_index = inner
            .find_session_index(&idle_session_id)
            .expect("idle Codex session should exist");
        inner.sessions[idle_index].runtime = SessionRuntime::Codex(idle_handle);
        inner.sessions[idle_index].session.status = SessionStatus::Idle;
        inner.sessions[idle_index].session.preview = "Idle Codex tab".to_owned();
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
    let idle_session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == idle_session_id)
        .expect("idle Codex session should remain present");
    assert_eq!(idle_session.status, SessionStatus::Idle);
    assert_eq!(idle_session.preview, "Idle Codex tab");

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
    let idle_record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == idle_session_id)
        .expect("idle Codex session should exist");
    assert!(matches!(idle_record.runtime, SessionRuntime::None));
    drop(inner);

    let _ = process.kill();
    let _ = wait_for_shared_child_exit_timeout(
        &process,
        Duration::from_secs(3),
        "shared Codex runtime",
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}
