// codex threads are the native unit of Codex CLI sessions: each Codex
// conversation is a thread identified by an id, and TermAl exposes
// thread-level actions on top of them — fork (branch a new local
// session off the thread's history), archive / unarchive (hide from
// the UI without killing the runtime), rollback, and compact.
// model options are paginated by the shared Codex app server, so the
// model picker walks pages with a max-page cap to keep huge model
// catalogs from running memory away. rerouted-model notifications
// fire when Codex redirects a request to a different model (rate
// limits, context overflow, safety) and must surface as a
// user-visible notice in the transcript. compaction notices, which
// tell the user their context was compacted, must appear BEFORE the
// assistant output they summarize so the narrative ordering in the
// transcript stays coherent. thread actions require a live idle
// thread — archiving a running session or a non-existent thread is
// rejected by a 400 guard in the API. production surfaces:
// `fork_codex_thread`, `archive_codex_thread`,
// `refresh_codex_model_options`, `shared_codex_model_list_paginated`,
// handlers in `src/state.rs` + `src/runtime.rs`.

use super::*;

// pins that `refresh_session_model_options` round-trips a
// `RefreshModelList` command through the Codex runtime and persists
// the returned options onto the session. guards against regressions
// where the refresh ignores runtime responses or drops model entries.
#[test]
fn refreshes_codex_model_options_from_runtime() {
    let state = test_app_state();

    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Refresh".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5.4".to_owned()),
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let child = test_exit_success_child();
    let (input_tx, input_rx) = mpsc::channel();
    let runtime = CodexRuntimeHandle {
        runtime_id: "codex-model-refresh".to_owned(),
        input_tx,
        process: Arc::new(SharedChild::new(child).unwrap()),
        shared_session: None,
    };

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }

    std::thread::spawn(move || {
        let command = input_rx
            .recv()
            .expect("Codex refresh command should arrive");
        match command {
            CodexRuntimeCommand::RefreshModelList { response_tx } => {
                let _ = response_tx.send(Ok(vec![
                    SessionModelOption::plain("gpt-5.4", "gpt-5.4"),
                    SessionModelOption::plain("gpt-5.3-codex", "gpt-5.3-codex"),
                ]));
            }
            _ => panic!("expected Codex model refresh command"),
        }
    });

    let refreshed = state
        .refresh_session_model_options(&created.session_id)
        .expect("Codex model refresh should succeed");
    let session = refreshed
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("refreshed Codex session should be present");

    assert_eq!(
        session.model_options,
        vec![
            SessionModelOption::plain("gpt-5.4", "gpt-5.4"),
            SessionModelOption::plain("gpt-5.3-codex", "gpt-5.3-codex"),
        ]
    );
}

// pins that pagination surfaces an error and stops queueing new
// pages once `SHARED_CODEX_MODEL_LIST_MAX_PAGES` is reached, even
// when the server keeps returning `nextCursor`. guards against
// unbounded memory growth from a misbehaving or hostile app server.
#[test]
fn shared_codex_model_list_pagination_stops_after_max_pages() {
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let (input_tx, input_rx) = mpsc::channel();
    let (response_tx, response_rx) = mpsc::channel();
    let mut writer = Vec::new();

    fire_codex_model_list_page(
        &mut writer,
        &pending_requests,
        &input_tx,
        Some("cursor-50".to_owned()),
        Vec::new(),
        SHARED_CODEX_MODEL_LIST_MAX_PAGES,
        response_tx,
    )
    .unwrap();

    let (_request_id, sender) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));
    sender
        .send(Ok(json!({
            "data": [],
            "nextCursor": "cursor-51"
        })))
        .unwrap();

    let result = response_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("model list response should arrive");
    assert_eq!(
        result,
        Err(format!(
            "Codex model list pagination exceeded {} pages.",
            SHARED_CODEX_MODEL_LIST_MAX_PAGES
        ))
    );
    match input_rx.recv_timeout(Duration::from_millis(100)) {
        Err(mpsc::RecvTimeoutError::Timeout) => {}
        Ok(_) => panic!("model list pagination should not queue another page past the cap"),
        Err(err) => panic!("unexpected model list pagination channel error: {err}"),
    }
}

// pins that a closed runtime input channel during continuation is
// reported immediately as a pagination error rather than leaving the
// caller waiting forever. guards against silent hangs when the
// shared Codex runtime has already shut down mid-walk.
#[test]
fn shared_codex_model_list_pagination_queue_failure_returns_error() {
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let (input_tx, input_rx) = mpsc::channel::<CodexRuntimeCommand>();
    drop(input_rx);
    let (response_tx, response_rx) = mpsc::channel();
    let mut writer = Vec::new();

    fire_codex_model_list_page(
        &mut writer,
        &pending_requests,
        &input_tx,
        Some("cursor-1".to_owned()),
        Vec::new(),
        1,
        response_tx,
    )
    .unwrap();

    let (_request_id, sender) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));
    sender
        .send(Ok(json!({
            "data": [],
            "nextCursor": "cursor-2"
        })))
        .unwrap();

    let result = response_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("model list response should arrive");
    assert_eq!(
        result,
        Err("failed to queue next Codex model list page: sending on a closed channel".to_owned())
    );
}

// pins that `fork_codex_thread` mints a distinct local session from
// the `thread/fork` response, replaying turn items into typed
// messages (user, reasoning, assistant, command, diff) and
// inheriting model / approval / sandbox / workdir from the reply.
// guards against fork losing history fidelity or reusing the origin id.
#[test]
fn fork_codex_thread_creates_a_new_local_session() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Review".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5.4".to_owned()),
            approval_policy: Some(CodexApprovalPolicy::Never),
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    state
        .set_external_session_id(&created.session_id, "thread-origin".to_owned())
        .unwrap();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&created.session_id)
            .expect("source Codex session should exist");
        inner.sessions[index].session.model_options =
            vec![SessionModelOption::plain("gpt-5.4", "gpt-5.4")];
    }

    let (runtime, input_rx, _process) = test_shared_codex_runtime("shared-codex-fork");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Codex fork command should arrive");
        match command {
            CodexRuntimeCommand::JsonRpcRequest {
                method,
                params,
                response_tx,
                ..
            } => {
                assert_eq!(method, "thread/fork");
                assert_eq!(params["threadId"], "thread-origin");
                let _ = response_tx.send(Ok(json!({
                    "thread": {
                        "id": "thread-forked",
                        "name": "Forked Review",
                        "preview": "Forked preview",
                        "turns": [
                            {
                                "id": "turn-1",
                                "status": "completed",
                                "items": [
                                    {
                                        "id": "item-user-1",
                                        "type": "userMessage",
                                        "content": [
                                            {
                                                "type": "text",
                                                "text": "Review src/state.rs"
                                            },
                                            {
                                                "type": "mention",
                                                "name": "docs/bugs.md",
                                                "path": "docs/bugs.md"
                                            }
                                        ]
                                    },
                                    {
                                        "id": "item-reasoning-1",
                                        "type": "reasoning",
                                        "summary": ["Inspect session state."],
                                        "content": ["Watch archive transitions."]
                                    },
                                    {
                                        "id": "item-agent-1",
                                        "type": "agentMessage",
                                        "text": "I found the bug."
                                    },
                                    {
                                        "id": "item-command-1",
                                        "type": "commandExecution",
                                        "command": "git diff --stat",
                                        "commandActions": [],
                                        "cwd": "/tmp/forked",
                                        "status": "completed",
                                        "aggregatedOutput": "1 file changed",
                                        "exitCode": 0
                                    },
                                    {
                                        "id": "item-file-1",
                                        "type": "fileChange",
                                        "status": "completed",
                                        "changes": [
                                            {
                                                "path": "src/state.rs",
                                                "diff": "@@ -1 +1 @@\n-old\n+new",
                                                "kind": {
                                                    "type": "modify"
                                                }
                                            }
                                        ]
                                    }
                                ]
                            }
                        ]
                    },
                    "model": "gpt-5.5",
                    "approvalPolicy": "on-request",
                    "sandbox": {
                        "type": "workspaceWrite"
                    },
                    "reasoningEffort": "high",
                    "cwd": "/tmp/forked",
                })));
            }
            _ => panic!("expected shared Codex JSON-RPC request"),
        }
    });

    let mut delta_events = state.subscribe_delta_events();
    let forked = state.fork_codex_thread(&created.session_id).unwrap();
    assert_ne!(forked.session_id, created.session_id);

    let forked_session = &forked.session;
    assert_eq!(forked_session.name, "Forked Review Fork");
    assert_eq!(forked_session.model, "gpt-5.5");
    assert_eq!(
        forked_session.approval_policy,
        Some(CodexApprovalPolicy::OnRequest)
    );
    assert_eq!(
        forked_session.reasoning_effort,
        Some(CodexReasoningEffort::High)
    );
    assert_eq!(
        forked_session.sandbox_mode,
        Some(CodexSandboxMode::WorkspaceWrite)
    );
    assert_eq!(
        forked_session.external_session_id.as_deref(),
        Some("thread-forked")
    );
    assert_eq!(
        forked_session.codex_thread_state,
        Some(CodexThreadState::Active)
    );
    assert_eq!(forked_session.workdir, "/tmp/forked");
    assert_eq!(
        forked_session.model_options,
        vec![SessionModelOption::plain("gpt-5.4", "gpt-5.4")]
    );
    assert!(matches!(
        forked_session.messages.first(),
        Some(Message::Text { author: Author::You, text, .. })
            if text.contains("Review src/state.rs")
                && text.contains("Mention: docs/bugs.md (docs/bugs.md)")
    ));
    assert!(matches!(
        forked_session.messages.get(1),
        Some(Message::Thinking { title, lines, .. })
            if title == "Codex reasoning"
                && lines == &[
                    "Inspect session state.".to_owned(),
                    "Watch archive transitions.".to_owned(),
                ]
    ));
    assert!(matches!(
        forked_session.messages.get(2),
        Some(Message::Text { author: Author::Assistant, text, .. }) if text == "I found the bug."
    ));
    assert!(matches!(
        forked_session.messages.get(3),
        Some(Message::Command {
            command,
            output,
            status,
            ..
        }) if command == "git diff --stat"
            && output == "1 file changed"
            && *status == CommandStatus::Success
    ));
    assert!(matches!(
        forked_session.messages.get(4),
        Some(Message::Diff {
            file_path,
            summary,
            diff,
            change_type,
            ..
        }) if file_path == "src/state.rs"
            && summary == "Updated state.rs"
            && diff.contains("+new")
            && *change_type == ChangeType::Edit
    ));
    assert!(!forked_session.messages.iter().any(
        |message| matches!(message, Message::Markdown { title, .. } if title == "Forked Codex thread")
    ));

    let payload = delta_events
        .try_recv()
        .expect("fork should publish a sessionCreated delta");
    let delta: DeltaEvent = serde_json::from_str(&payload).expect("delta should decode");
    match delta {
        DeltaEvent::SessionCreated {
            revision,
            session_id,
            session,
        } => {
            assert_eq!(revision, forked.revision);
            assert_eq!(session_id, forked.session_id);
            assert_eq!(session.id, forked.session_id);
            assert!(!session.messages_loaded);
            assert!(session.messages.is_empty());
            assert_eq!(session.message_count, forked_session.messages.len() as u32);
        }
        _ => panic!("expected sessionCreated delta"),
    }
}

// pins that when `thread/fork` omits turn history the forked session
// still appears with a Markdown note explaining the history gap
// rather than silently looking empty. guards against users thinking
// a fork succeeded yet mysteriously lost their prior conversation.
#[test]
fn fork_codex_thread_falls_back_to_note_when_history_is_unavailable() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Review".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: Some("gpt-5.4".to_owned()),
            approval_policy: Some(CodexApprovalPolicy::Never),
            reasoning_effort: Some(CodexReasoningEffort::Medium),
            sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    state
        .set_external_session_id(&created.session_id, "thread-origin".to_owned())
        .unwrap();

    let (runtime, input_rx, _process) = test_shared_codex_runtime("shared-codex-fork-fallback");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("Codex fork command should arrive");
        match command {
            CodexRuntimeCommand::JsonRpcRequest {
                method,
                params,
                response_tx,
                ..
            } => {
                assert_eq!(method, "thread/fork");
                assert_eq!(params["threadId"], "thread-origin");
                let _ = response_tx.send(Ok(json!({
                    "thread": {
                        "id": "thread-forked",
                        "name": "Forked Review",
                        "preview": "Forked preview"
                    },
                    "model": "gpt-5.5",
                    "approvalPolicy": "on-request",
                    "sandbox": {
                        "type": "workspaceWrite"
                    },
                    "reasoningEffort": "high",
                    "cwd": "/tmp/forked"
                })));
            }
            _ => panic!("expected shared Codex JSON-RPC request"),
        }
    });

    let forked = state.fork_codex_thread(&created.session_id).unwrap();
    let forked_session = &forked.session;
    assert!(matches!(
        forked_session.messages.last(),
        Some(Message::Markdown { title, markdown, .. })
            if title == "Forked Codex thread"
                && markdown.contains("Codex did not return the earlier thread history")
    ));
}

// pins that archive / compact reject when no thread id is set, when
// the session is actively running, and when prompts are still
// queued — each with its own diagnostic message. guards the 400
// API guard against letting destructive actions land on sessions
// mid-turn or without a real Codex thread to act on.
#[test]
fn codex_thread_actions_require_a_live_idle_thread() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);

    let missing_thread_error = match state.archive_codex_thread(&session_id) {
        Ok(_) => panic!("archive should fail without a live Codex thread"),
        Err(err) => err,
    };
    assert!(
        missing_thread_error
            .message
            .contains("only available after the session has started a thread")
    );

    state
        .set_external_session_id(&session_id, "thread-live".to_owned())
        .unwrap();
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    let busy_error = match state.compact_codex_thread(&session_id) {
        Ok(_) => panic!("compact should fail while the session is active"),
        Err(err) => err,
    };
    assert!(
        busy_error
            .message
            .contains("wait for the current Codex turn to finish")
    );

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].session.status = SessionStatus::Idle;
        let queued_message_id = inner.next_message_id();
        queue_prompt_on_record(
            &mut inner.sessions[index],
            PendingPrompt {
                attachments: Vec::new(),
                id: queued_message_id,
                timestamp: stamp_now(),
                text: "queued prompt".to_owned(),
                expanded_text: None,
            },
            Vec::new(),
        );
    }

    let queued_error = match state.archive_codex_thread(&session_id) {
        Ok(_) => panic!("archive should fail while prompts are queued"),
        Err(err) => err,
    };
    assert!(
        queued_error
            .message
            .contains("wait for queued Codex prompts to finish")
    );
}

// pins archive flips `codex_thread_state` to Archived, appends an
// "Archived Codex thread" notice, and causes `dispatch_turn` to
// reject new prompts with 409; unarchive restores Active and writes
// a "Restored" notice. guards against archived threads still
// accepting turns or state flags drifting out of sync.
#[test]
fn codex_archive_and_unarchive_actions_update_thread_state_and_block_dispatch() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "thread-live".to_owned())
        .unwrap();

    let initial_session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .expect("Codex session should exist");
    assert_eq!(
        initial_session.codex_thread_state,
        Some(CodexThreadState::Active)
    );

    let (runtime, input_rx, _process) = test_shared_codex_runtime("shared-codex-archive");
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);

    std::thread::spawn(move || {
        for expected_method in ["thread/archive", "thread/unarchive"] {
            let command = input_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("Codex thread action should arrive");
            match command {
                CodexRuntimeCommand::JsonRpcRequest {
                    method,
                    params,
                    response_tx,
                    ..
                } => {
                    assert_eq!(method, expected_method);
                    assert_eq!(params["threadId"], "thread-live");
                    let _ = response_tx.send(Ok(json!({})));
                }
                _ => panic!("expected shared Codex JSON-RPC request"),
            }
        }
    });

    let archived = state.archive_codex_thread(&session_id).unwrap();
    let archived_session = archived
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated Codex session should be present");
    assert_eq!(
        archived_session.codex_thread_state,
        Some(CodexThreadState::Archived)
    );
    assert!(!archived_session.messages_loaded);
    assert!(archived_session.messages.is_empty());
    let archived_full_session = state
        .get_session(&session_id)
        .expect("archived Codex session should hydrate")
        .session;
    assert!(matches!(
        archived_full_session.messages.last(),
        Some(Message::Markdown { title, .. }) if title == "Archived Codex thread"
    ));

    let archived_error = match state.dispatch_turn(
        &session_id,
        SendMessageRequest {
            text: "resume the review".to_owned(),
            expanded_text: None,
            attachments: Vec::new(),
        },
    ) {
        Ok(_) => panic!("archived Codex thread should reject new prompts"),
        Err(err) => err,
    };
    assert_eq!(archived_error.status, StatusCode::CONFLICT);
    assert!(
        archived_error
            .message
            .contains("current Codex thread is archived")
    );

    let restored = state.unarchive_codex_thread(&session_id).unwrap();
    let restored_session = restored
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated Codex session should be present");
    assert_eq!(
        restored_session.codex_thread_state,
        Some(CodexThreadState::Active)
    );
    assert!(!restored_session.messages_loaded);
    assert!(restored_session.messages.is_empty());
    let restored_full_session = state
        .get_session(&session_id)
        .expect("restored Codex session should hydrate")
        .session;
    assert!(matches!(
        restored_full_session.messages.last(),
        Some(Message::Markdown { title, .. }) if title == "Restored Codex thread"
    ));
}

// pins that inbound `thread/archived` and `thread/unarchived`
// notifications from the shared Codex app server resolve the thread
// id to a local session and update its `codex_thread_state`
// accordingly. guards against external archive actions (other
// clients) failing to reflect in TermAl's UI.
#[test]
fn shared_codex_archive_notifications_update_thread_state() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "conversation-123".to_owned())
        .unwrap();
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-thread-state");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let archived = json!({
        "method": "thread/archived",
        "params": {
            "threadId": "conversation-123"
        }
    });
    let unarchived = json!({
        "method": "thread/unarchived",
        "params": {
            "threadId": "conversation-123"
        }
    });

    handle_shared_codex_app_server_message(
        &archived,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let archived_session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(
        archived_session.codex_thread_state,
        Some(CodexThreadState::Archived)
    );

    handle_shared_codex_app_server_message(
        &unarchived,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let restored_session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(
        restored_session.codex_thread_state,
        Some(CodexThreadState::Active)
    );
}

// pins that a `model/rerouted` notification records a user-visible
// transcript message naming both the origin and destination models
// and a humanised reason (here "high-risk cyber activity"). guards
// against silent model swaps where users see unexpected behaviour
// without knowing Codex redirected their turn.
#[test]
fn shared_codex_model_rerouted_notification_records_notice() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "conversation-reroute".to_owned())
        .unwrap();
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-reroute");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-reroute".to_owned()),
                turn_id: Some("turn-reroute".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-reroute".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let rerouted = json!({
        "method": "model/rerouted",
        "params": {
            "threadId": "conversation-reroute",
            "turnId": "turn-reroute",
            "fromModel": "gpt-5.4",
            "toModel": "gpt-5.4-mini",
            "reason": "highRiskCyberActivity"
        }
    });

    handle_shared_codex_app_server_message(
        &rerouted,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. })
            if text == "Codex rerouted this turn from `gpt-5.4` to `gpt-5.4-mini` because it detected high-risk cyber activity."
    ));
}

// pins that a `thread/compacted` notification inserts its notice
// ahead of the first visible assistant message for the turn rather
// than appending at the tail. guards transcript narrative order —
// users must see "context was compacted" before the assistant reply
// that reasoned over the compacted context.
#[test]
fn shared_codex_compaction_notice_inserts_before_visible_assistant_output() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .set_external_session_id(&session_id, "conversation-compact".to_owned())
        .unwrap();
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-compact");

    let assistant_message_id = state.allocate_message_id();
    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: assistant_message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Existing assistant output".to_owned(),
                expanded_text: None,
            },
        )
        .unwrap();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: Some(SharedCodexSessionHandle {
                runtime: runtime.clone(),
                session_id: session_id.clone(),
            }),
        });
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-compact".to_owned()),
                turn_id: Some("turn-compact".to_owned()),
                turn_state: CodexTurnState {
                    assistant_output_started: true,
                    first_visible_assistant_message_id: Some(assistant_message_id.clone()),
                    ..CodexTurnState::default()
                },
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-compact".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let compacted = json!({
        "method": "thread/compacted",
        "params": {
            "threadId": "conversation-compact",
            "turnId": "turn-compact"
        }
    });

    handle_shared_codex_app_server_message(
        &compacted,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let session = state
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    let compact_notice_index = session
        .messages
        .iter()
        .position(|message| {
            matches!(
                message,
                Message::Text { text, .. }
                    if text == "Codex compacted the thread context for this turn."
            )
        })
        .expect("compaction notice should be present");
    let assistant_index = session
        .messages
        .iter()
        .position(|message| {
            matches!(
                message,
                Message::Text { id, text, .. }
                    if id == &assistant_message_id && text == "Existing assistant output"
            )
        })
        .expect("assistant output should remain present");
    assert!(compact_notice_index < assistant_index);
}
