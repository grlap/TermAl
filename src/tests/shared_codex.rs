//! Shared Codex runtime event-handling tests.
//!
//! Codex's shared-app-server mode runs ONE long-lived Codex process that
//! speaks JSON-RPC over stdio and hosts MULTIPLE concurrent sessions. Every
//! notification carries a `session_id` (or a `turn_id`/`conversation_id`
//! from which it can be derived) and the runtime must fan each event out to
//! the right per-session transcript — `SharedCodexSessionState` in
//! `src/runtime.rs` tracks the active `turn_id`, a grace-period
//! `completed_turn_id`, the pending-turn-start request id, and the recorder
//! bookkeeping that ties item-completed / content-delta / final messages
//! back to a single on-screen assistant message.
//!
//! Routing is fragile because events race: a `codex/event/agent_message`
//! final can arrive AFTER `turn/completed`, still-in-flight chunks for the
//! previous turn can land AFTER the next turn started (stale turn_id), and
//! a dropped `session_id` forces a fallback through `conversation_id`. The
//! streaming reconciler further has to cope with Codex sending both
//! `agent_message_content_delta` chunks AND a `final` whole-text message —
//! it must append a missing suffix, skip an exact duplicate, or replace
//! divergent text, and tolerate the final arriving outside the turn window.
//! Codex's subagent "agent message" results are buffered during the turn
//! and flushed as a summary AFTER the final assistant text so narrative
//! order is preserved; a stop-in-progress flag defers runtime events while
//! stop machinery finalizes state (dedicated replay tests live in
//! `tests/session_stop.rs`). Production entry points are the
//! `handle_shared_codex_*` helpers in `src/runtime.rs` plus the
//! `*_if_runtime_matches_*` state helpers in `src/state.rs`.

use super::*;

// Pins that a subagent task_complete summary is buffered until the final
// agent_message lands, then inserted BEFORE the final assistant text.
// Guards against surfacing the subagent result out of narrative order
// (above or without the human-facing answer).
#[test]
fn shared_codex_task_complete_event_buffers_subagent_result_until_final_agent_message() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-task-complete");

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
        inner.sessions[index].session.status = SessionStatus::Active;
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
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-1"
            }
        }
    });
    let task_complete = json!({
        "method": "codex/event/task_complete",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "last_agent_message": "Reviewer found a real bug.",
                "turn_id": "turn-sub-1",
                "type": "task_complete"
            }
        }
    });
    let final_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-sub-1",
            "msg": {
                "message": "Final shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &task_complete,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &final_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.preview, "Final shared Codex answer.");
    assert_eq!(session.messages.len(), 2);
    assert!(matches!(
        session.messages.first(),
        Some(Message::SubagentResult {
            title,
            summary,
            conversation_id,
            turn_id,
            ..
        }) if title == "Subagent completed"
            && summary == "Reviewer found a real bug."
            && conversation_id.as_deref() == Some("conversation-123")
            && turn_id.as_deref() == Some("turn-sub-1")
    ));
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Final shared Codex answer."
    ));
}

// Pins that an agent_message notification missing params.id falls back to
// the session's currently-active turn_id rather than being dropped.
// Guards against silently discarding finals whenever Codex omits the
// per-event turn stamp mid-turn.
#[test]
fn shared_codex_agent_message_event_without_turn_id_uses_active_turn() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-agent-final-no-turn-id");

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
        inner.sessions[index].session.status = SessionStatus::Active;
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
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-no-id"
            }
        }
    });
    let final_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "message": "Final shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &final_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.preview, "Final shared Codex answer.");
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Final shared Codex answer."
    ));
}

// Pins that an agent_message carrying a stale turn_id from params.id (a
// turn that has already been superseded by a newer turn/started) is
// dropped rather than appended to the current transcript.
// Guards against stale turn_id from params.id overwriting the current turn's text.
#[test]
fn shared_codex_agent_message_event_ignores_stale_turn_id_from_params_id() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-agent-final-stale-params-id");

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
        inner.sessions[index].session.status = SessionStatus::Active;
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
    let turn_started_previous = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stale"
            }
        }
    });
    let turn_started_current = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-current"
            }
        }
    });
    let stale_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-stale",
            "msg": {
                "message": "Stale shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });
    let current_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-current",
            "msg": {
                "message": "Current shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started_previous,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_started_current,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &stale_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &current_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.preview, "Current shared Codex answer.");
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Current shared Codex answer."
    ));
}

// Pins that a buffered subagent summary is inserted ahead of the CURRENT
// turn's final answer, leaving any pre-existing assistant message from a
// prior turn untouched at the top of the transcript.
// Guards against splicing the summary above unrelated historical messages.
#[test]
fn shared_codex_task_complete_event_stays_in_current_turn_after_prior_assistant_message() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    state
        .push_message(
            &session_id,
            Message::Text {
                attachments: Vec::new(),
                id: "assistant-previous".to_owned(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "Previous shared Codex answer.".to_owned(),
                expanded_text: None,
            },
        )
        .unwrap();
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-task-complete-order");

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
        inner.sessions[index].session.status = SessionStatus::Active;
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
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-2"
            }
        }
    });
    let task_complete = json!({
        "method": "codex/event/task_complete",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "last_agent_message": "Reviewer found a real bug.",
                "turn_id": "turn-sub-2",
                "type": "task_complete"
            }
        }
    });
    let final_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-sub-2",
            "msg": {
                "message": "Current shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &task_complete,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(matches!(
        session.messages.as_slice(),
        [Message::Text { text, .. }] if text == "Previous shared Codex answer."
    ));

    handle_shared_codex_app_server_message(
        &final_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.messages.len(), 3);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Previous shared Codex answer."
    ));
    assert!(matches!(
        session.messages.get(1),
        Some(Message::SubagentResult {
            title,
            summary,
            conversation_id,
            turn_id,
            ..
        }) if title == "Subagent completed"
            && summary == "Reviewer found a real bug."
            && conversation_id.as_deref() == Some("conversation-123")
            && turn_id.as_deref() == Some("turn-sub-2")
    ));
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Current shared Codex answer."
    ));
}

// Pins that a task_complete arriving while the session has no active or
// recently-completed turn (idle session) produces no messages at all.
// Guards against ghost subagent summaries leaking into an idle transcript
// when Codex emits events outside any turn window.
#[test]
fn shared_codex_task_complete_event_without_active_turn_is_ignored() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-task-complete-no-active-turn");

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
        inner.sessions[index].session.status = SessionStatus::Idle;
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
    let task_complete = json!({
        "method": "codex/event/task_complete",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "last_agent_message": "Stale hidden summary.",
                "type": "task_complete"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &task_complete,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert!(session.messages.is_empty());
}

// Pins that when task_complete arrives after streaming delta output has
// already started (assistant_output_started=true), the buffered subagent
// summary is inserted BEFORE the already-visible streamed answer.
// Guards against appending the summary after the final text in the wrong order.
#[test]
fn shared_codex_task_complete_event_after_streaming_output_inserts_before_answer() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-task-complete-late");

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
        inner.sessions[index].session.status = SessionStatus::Active;
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
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-3"
            }
        }
    });
    let delta_message = json!({
        "method": "codex/event/agent_message_content_delta",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-sub-3",
            "msg": {
                "delta": "Current shared Codex answer.",
                "item_id": "msg-123",
                "thread_id": "conversation-123",
                "turn_id": "turn-sub-3",
                "type": "agent_message_content_delta"
            }
        }
    });
    let task_complete = json!({
        "method": "codex/event/task_complete",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "last_agent_message": "Reviewer found a real bug.",
                "turn_id": "turn-sub-3",
                "type": "task_complete"
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-3",
                "error": null
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &delta_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &task_complete,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_completed,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.preview, "Current shared Codex answer.");
    assert_eq!(session.messages.len(), 2);
    assert!(matches!(
        session.messages.first(),
        Some(Message::SubagentResult {
            title,
            summary,
            conversation_id,
            turn_id,
            ..
        }) if title == "Subagent completed"
            && summary == "Reviewer found a real bug."
            && conversation_id.as_deref() == Some("conversation-123")
            && turn_id.as_deref() == Some("turn-sub-3")
    ));
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Current shared Codex answer."
    ));
}

// Pins that a task_complete tagged with a previous turn_id is dropped once
// a newer turn/started has rotated the active turn, so only the current
// turn's final text lands.
// Guards against stale turn_id from previous turn injecting a summary
// above the current turn's answer.
#[test]
fn shared_codex_task_complete_event_ignores_stale_summary_from_previous_turn() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-task-complete-stale");

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
        inner.sessions[index].session.status = SessionStatus::Active;
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
    let turn_started_previous = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stale"
            }
        }
    });
    let turn_started_current = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-current"
            }
        }
    });
    let stale_task_complete = json!({
        "method": "codex/event/task_complete",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "last_agent_message": "Stale hidden summary.",
                "turn_id": "turn-stale",
                "type": "task_complete"
            }
        }
    });
    let final_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-current",
            "msg": {
                "message": "Current shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started_previous,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_started_current,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &stale_task_complete,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &final_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.preview, "Current shared Codex answer.");
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Current shared Codex answer."
    ));
}

// Pins that if a turn ends with a turn/completed error before any final
// agent_message arrives, the buffered subagent summary is discarded and
// only the turn-failed notice is recorded.
// Guards against leaking orphaned subagent summaries into failed turns.
#[test]
fn shared_codex_task_complete_event_drops_buffered_summary_on_failed_turn() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-task-complete-error");

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
        inner.sessions[index].session.status = SessionStatus::Active;
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
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-4"
            }
        }
    });
    let task_complete = json!({
        "method": "codex/event/task_complete",
        "params": {
            "conversationId": "conversation-123",
            "msg": {
                "last_agent_message": "Reviewer found a real bug.",
                "turn_id": "turn-sub-4",
                "type": "task_complete"
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-4",
                "error": {
                    "message": "stream failed"
                }
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &task_complete,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_completed,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.status, SessionStatus::Error);
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Turn failed: stream failed"
    ));
}

// Pins that turn/completed flushes any still-buffered subagent results
// into the transcript even when no late final agent_message arrives, so
// long as assistant output has already started in this turn.
// Guards against subagent summaries being silently dropped at turn close.
#[test]
fn shared_codex_turn_completed_flushes_buffered_subagent_results_after_output_started() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-turn-completed-flushes-buffer");

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
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                turn_id: Some("turn-sub-5".to_owned()),
                turn_state: CodexTurnState {
                    assistant_output_started: true,
                    pending_subagent_results: vec![PendingSubagentResult {
                        title: "Subagent completed".to_owned(),
                        summary: "Buffered reviewer summary.".to_owned(),
                        conversation_id: Some("conversation-123".to_owned()),
                        turn_id: Some("turn-sub-5".to_owned()),
                    }],
                    ..CodexTurnState::default()
                },
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-sub-5",
                "error": null
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_completed,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::SubagentResult {
            title,
            summary,
            conversation_id,
            turn_id,
            ..
        }) if title == "Subagent completed"
            && summary == "Buffered reviewer summary."
            && conversation_id.as_deref() == Some("conversation-123")
            && turn_id.as_deref() == Some("turn-sub-5")
    ));
}

// Pins that a codex/event/item_completed carrying an AgentMessage item
// with a single Text content part records the text as a transcript
// message on the correct session.
// Guards against item-completed events being treated as non-recordable.
#[test]
fn shared_codex_item_completed_event_records_agent_message() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-item-completed");

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
        inner.sessions[index].session.status = SessionStatus::Active;
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
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-123"
            }
        }
    });
    let message = json!({
        "method": "codex/event/item_completed",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "item": {
                    "content": [
                        {
                            "text": "Hello.",
                            "type": "Text"
                        }
                    ],
                    "id": "msg-123",
                    "phase": "final_answer",
                    "type": "AgentMessage"
                },
                "thread_id": "conversation-123",
                "turn_id": "turn-123",
                "type": "item_completed"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Hello."
    ));
}

// Pins that an item_completed whose params.id names an already-rotated
// prior turn is dropped; only the current-turn item_completed records.
// Guards against stale turn_id from params.id causing an item_completed
// to append the prior turn's assistant text.
#[test]
fn shared_codex_item_completed_event_ignores_stale_turn_id_from_params_id() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-item-completed-stale-params-id");

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
        inner.sessions[index].session.status = SessionStatus::Active;
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
    let turn_started_previous = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stale"
            }
        }
    });
    let turn_started_current = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-current"
            }
        }
    });
    let stale_message = json!({
        "method": "codex/event/item_completed",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-stale",
            "msg": {
                "item": {
                    "content": [
                        {
                            "text": "Stale shared Codex answer.",
                            "type": "Text"
                        }
                    ],
                    "id": "msg-stale",
                    "phase": "final_answer",
                    "type": "AgentMessage"
                },
                "thread_id": "conversation-123",
                "type": "item_completed"
            }
        }
    });
    let current_message = json!({
        "method": "codex/event/item_completed",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-current",
            "msg": {
                "item": {
                    "content": [
                        {
                            "text": "Current shared Codex answer.",
                            "type": "Text"
                        }
                    ],
                    "id": "msg-current",
                    "phase": "final_answer",
                    "type": "AgentMessage"
                },
                "thread_id": "conversation-123",
                "type": "item_completed"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started_previous,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_started_current,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &stale_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &current_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.preview, "Current shared Codex answer.");
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Current shared Codex answer."
    ));
}

// Pins that an AgentMessage item with multiple content parts is
// concatenated across Text parts (skipping Reasoning/metadata-only parts)
// into a single joined text message.
// Guards against dropping trailing Text parts after a Reasoning block, or
// emitting one transcript entry per content fragment.
#[test]
fn shared_codex_item_completed_event_concatenates_multipart_agent_message() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-item-completed-multipart");

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
        inner.sessions[index].session.status = SessionStatus::Active;
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
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-123"
            }
        }
    });
    let message = json!({
        "method": "codex/event/item_completed",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "item": {
                    "content": [
                        {
                            "text": "Hello",
                            "type": "Text"
                        },
                        {
                            "metadata": {
                                "ignored": true
                            },
                            "type": "Reasoning"
                        },
                        {
                            "text": ", world.",
                            "type": "Text"
                        }
                    ],
                    "id": "msg-123",
                    "phase": "final_answer",
                    "type": "AgentMessage"
                },
                "thread_id": "conversation-123",
                "turn_id": "turn-123",
                "type": "item_completed"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Hello, world."
    ));
}

// Pins that a content_delta tagged with a prior turn's id (now rotated
// out) does not begin a streamed message, and the next current-turn delta
// starts a fresh transcript entry.
// Guards against stale turn_id from params.id streaming characters into a
// message that belongs to a turn Codex has already moved past.
#[test]
fn shared_codex_agent_message_content_delta_event_ignores_stale_turn_id_from_params_id() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-agent-delta-stale-params-id");

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
        inner.sessions[index].session.status = SessionStatus::Active;
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
    let turn_started_previous = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stale"
            }
        }
    });
    let turn_started_current = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-current"
            }
        }
    });
    let stale_delta_message = json!({
        "method": "codex/event/agent_message_content_delta",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-stale",
            "msg": {
                "delta": "Stale shared Codex answer.",
                "item_id": "msg-stale",
                "thread_id": "conversation-123",
                "type": "agent_message_content_delta"
            }
        }
    });
    let current_delta_message = json!({
        "method": "codex/event/agent_message_content_delta",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-current",
            "msg": {
                "delta": "Current shared Codex answer.",
                "item_id": "msg-current",
                "thread_id": "conversation-123",
                "type": "agent_message_content_delta"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started_previous,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_started_current,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &stale_delta_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &current_delta_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.preview, "Current shared Codex answer.");
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Current shared Codex answer."
    ));
}

// Pins the append-missing-suffix reconciliation case: when a delta
// streamed "Hello" and the final message is "Hello there.", the recorder
// extends the streamed message with the missing " there." suffix rather
// than emitting a second duplicate block.
// Guards against losing tail characters Codex only delivers in the final.
#[test]
fn shared_codex_agent_message_final_event_appends_missing_suffix_after_streamed_delta() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-agent-final-suffix");

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
        inner.sessions[index].session.status = SessionStatus::Active;
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
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-123"
            }
        }
    });
    let delta_message = json!({
        "method": "codex/event/agent_message_content_delta",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "delta": "Hello",
                "item_id": "msg-123",
                "thread_id": "conversation-123",
                "turn_id": "turn-123",
                "type": "agent_message_content_delta"
            }
        }
    });
    let final_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "message": "Hello there.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &delta_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &final_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Hello there."
    ));
}

// Pins the replace-divergent reconciliation case: when the final
// agent_message text does not share a prefix with the streamed delta, the
// recorder overwrites the streamed body with the final answer.
// Guards against leaving half-streamed divergent text in the transcript
// when Codex's final message reroutes output.
#[test]
fn shared_codex_agent_message_final_event_replaces_divergent_streamed_text() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-agent-final-replace");
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
        inner.sessions[index].session.status = SessionStatus::Active;
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
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-123"
            }
        }
    });
    let delta_message = json!({
        "method": "codex/event/agent_message_content_delta",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "delta": "Hello from stream",
                "item_id": "msg-123",
                "thread_id": "conversation-123",
                "turn_id": "turn-123",
                "type": "agent_message_content_delta"
            }
        }
    });
    let final_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "message": "Different final answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });
    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &delta_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Hello from stream"
    ));
    handle_shared_codex_app_server_message(
        &final_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.preview, "Different final answer.");
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Different final answer."
    ));
}
// Pins the skip-duplicate reconciliation case: when the final
// agent_message exactly matches the accumulated delta text, the streamed
// message is kept and the final is NOT emitted as a second transcript
// entry.
// Guards against the "Hello." streamed answer being doubled by a matching
// final that arrives immediately after.
#[test]
fn shared_codex_agent_message_content_delta_streams_without_duplicate_final_message() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-agent-delta");

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
        inner.sessions[index].session.status = SessionStatus::Active;
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
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-123"
            }
        }
    });
    let app_server_delta_message = json!({
        "method": "item/agentMessage/delta",
        "params": {
            "delta": "Hello.",
            "itemId": "msg-123"
        }
    });
    let delta_message = json!({
        "method": "codex/event/agent_message_content_delta",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "delta": "Hello.",
                "item_id": "msg-123",
                "thread_id": "conversation-123",
                "turn_id": "turn-123",
                "type": "agent_message_content_delta"
            }
        }
    });
    let final_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-123",
            "msg": {
                "message": "Hello.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &app_server_delta_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &delta_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &final_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Hello."
    ));
}

// Pins that a final agent_message arriving AFTER turn/completed still
// records into the just-completed turn during its grace-period window
// (see SHARED_CODEX_COMPLETED_TURN_GRACE_PERIOD).
// Guards against dropping legitimate finals that race turn/completed.
#[test]
fn shared_codex_agent_message_event_after_turn_completed_is_recorded() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-agent-final-after-turn-completed");

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
        inner.sessions[index].session.status = SessionStatus::Active;
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
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-finished"
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-finished",
                "error": null
            }
        }
    });
    let late_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-finished",
            "msg": {
                "message": "Late shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_completed,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &late_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Late shared Codex answer."
    ));
}

// Pins that once turn/started advances completed_turn_id has rolled off,
// a late final agent_message carrying the previous turn's id is dropped
// and the current-turn state is not polluted.
// Guards against stale turn_id from previous turn sneaking a message into
// the next turn's transcript.
#[test]
fn shared_codex_previous_turn_message_is_ignored_after_next_turn_starts() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-late-previous-turn-message");

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
        inner.sessions[index].session.status = SessionStatus::Active;
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
    let first_turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-first"
            }
        }
    });
    let first_turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-first",
                "error": null
            }
        }
    });
    let second_turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-second"
            }
        }
    });
    let late_first_turn_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-first",
            "msg": {
                "message": "Late first-turn answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    for message in [
        &first_turn_started,
        &first_turn_completed,
        &second_turn_started,
        &late_first_turn_message,
    ] {
        handle_shared_codex_app_server_message(
            message,
            &state,
            &runtime.runtime_id,
            &pending_requests,
            &runtime.sessions,
            &runtime.thread_sessions,
            &mpsc::channel::<CodexRuntimeCommand>().0,
        )
        .unwrap();
    }

    {
        let sessions = runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        let session_state = sessions
            .get(&session_id)
            .expect("shared Codex session state should exist");
        assert_eq!(session_state.turn_id.as_deref(), Some("turn-second"));
        assert!(session_state.completed_turn_id.is_none());
    }

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert!(session.messages.is_empty());
}

// Pins that an item/completed of type "agentMessage" arriving after
// turn/completed still records the final assistant text during the
// grace-period window — the app-server path mirrors codex/event handling.
// Guards against losing finals delivered via the app-server item/completed
// surface instead of the codex/event/agent_message surface.
#[test]
fn shared_codex_app_server_agent_message_completed_after_turn_completed_is_recorded() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-agent-item-after-turn-completed");

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
        inner.sessions[index].session.status = SessionStatus::Active;
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
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-finished"
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-finished",
                "error": null
            }
        }
    });
    let late_item = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "id": "msg-final",
                "type": "agentMessage",
                "text": "Late app-server final answer."
            }
        }
    });

    for message in [&turn_started, &turn_completed, &late_item] {
        handle_shared_codex_app_server_message(
            message,
            &state,
            &runtime.runtime_id,
            &pending_requests,
            &runtime.sessions,
            &runtime.thread_sessions,
            &mpsc::channel::<CodexRuntimeCommand>().0,
        )
        .unwrap();
    }

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Late app-server final answer."
    ));
}

// Pins that a non-agentMessage item/completed (here commandExecution)
// arriving after turn/completed is ignored — only final agent messages
// earn the grace-period window, not side-channel items.
// Guards against late command-execution output polluting a completed turn.
#[test]
fn shared_codex_app_server_item_completed_after_turn_completed_is_ignored() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-app-server-item-completed-after-turn-completed");

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
        inner.sessions[index].session.status = SessionStatus::Active;
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
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-finished"
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-finished",
                "error": null
            }
        }
    });
    let late_item = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "type": "commandExecution",
                "command": "pwd",
                "aggregatedOutput": "C:/github/Personal/TermAl",
                "status": "completed",
                "exitCode": 0
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_completed,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &late_item,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");

    assert!(session.messages.is_empty());
}

// Pins the completed-turn cleanup worker: once the grace-period expires,
// completed_turn_id, the current streamed item_id, and the streamed-text
// buffers are all cleared, and a subsequent late agent_message for that
// turn_id is refused.
// Guards against late events resurrecting the just-completed turn after
// cleanup has nominally closed the window.
#[test]
fn shared_codex_completed_turn_cleanup_expires_late_event_window() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-completed-turn-cleanup");

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
        inner.sessions[index].session.status = SessionStatus::Active;
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
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-finished"
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-finished",
                "error": null
            }
        }
    });
    let late_message = json!({
        "method": "codex/event/agent_message",
        "params": {
            "conversationId": "conversation-123",
            "id": "turn-finished",
            "msg": {
                "message": "Late shared Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_completed,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    {
        let mut sessions = runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        let session_state = sessions
            .get_mut(&session_id)
            .expect("shared Codex session state should exist");
        assert_eq!(
            session_state.completed_turn_id.as_deref(),
            Some("turn-finished")
        );
        session_state.turn_state.current_agent_message_id = Some("msg-final".to_owned());
        session_state
            .turn_state
            .streamed_agent_message_text_by_item_id
            .insert("msg-final".to_owned(), "stale buffered text".to_owned());
        session_state
            .turn_state
            .streamed_agent_message_item_ids
            .insert("msg-final".to_owned());
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let cleanup_complete = {
            let sessions = runtime
                .sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            let session_state = sessions
                .get(&session_id)
                .expect("shared Codex session state should exist");
            session_state.completed_turn_id.is_none()
                && session_state.turn_state.current_agent_message_id.is_none()
                && session_state
                    .turn_state
                    .streamed_agent_message_text_by_item_id
                    .is_empty()
                && session_state
                    .turn_state
                    .streamed_agent_message_item_ids
                    .is_empty()
        };
        if cleanup_complete {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "completed turn cleanup should clear residual turn state"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    handle_shared_codex_app_server_message(
        &late_message,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());
}

// Pins that turn/started clears the recorder's command_messages keys so
// the next turn's webSearch (or other recorder-keyed command) creates a
// fresh Message::Command rather than mutating the previous turn's entry.
// Guards against the second turn's search overwriting the first turn's
// recorded command output via a stale recorder key.
#[test]
fn shared_codex_turn_started_clears_command_recorder_keys_for_new_prompt() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-turn-start-clears-command-keys");

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
        inner.sessions[index].session.status = SessionStatus::Active;
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
    let turn_started_one = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-1"
            }
        }
    });
    let item_started_one = json!({
        "method": "item/started",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "type": "webSearch",
                "query": "rust anyhow",
                "action": {
                    "type": "search",
                    "queries": ["rust anyhow"]
                }
            }
        }
    });
    let item_completed_one = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "type": "webSearch",
                "query": "rust anyhow",
                "action": {
                    "type": "search",
                    "queries": ["rust anyhow"]
                }
            }
        }
    });
    let turn_completed_one = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-1",
                "error": null
            }
        }
    });
    let turn_started_two = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-2"
            }
        }
    });
    let item_started_two = json!({
        "method": "item/started",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "type": "webSearch",
                "query": "serde_json value",
                "action": {
                    "type": "search",
                    "queries": ["serde_json value"]
                }
            }
        }
    });
    let item_completed_two = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "type": "webSearch",
                "query": "serde_json value",
                "action": {
                    "type": "search",
                    "queries": ["serde_json value"]
                }
            }
        }
    });

    for message in [
        &turn_started_one,
        &item_started_one,
        &item_completed_one,
        &turn_completed_one,
        &turn_started_two,
        &item_started_two,
        &item_completed_two,
    ] {
        handle_shared_codex_app_server_message(
            message,
            &state,
            &runtime.runtime_id,
            &pending_requests,
            &runtime.sessions,
            &runtime.thread_sessions,
            &mpsc::channel::<CodexRuntimeCommand>().0,
        )
        .unwrap();
    }

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    let command_messages = session
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Command {
                command,
                output,
                status,
                ..
            } => Some((command.clone(), output.clone(), *status)),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        command_messages,
        vec![
            (
                "Web search: rust anyhow".to_owned(),
                "rust anyhow".to_owned(),
                CommandStatus::Success,
            ),
            (
                "Web search: serde_json value".to_owned(),
                "serde_json value".to_owned(),
                CommandStatus::Success,
            ),
        ]
    );
}

// Pins that a turn/completed carrying a turn.error clears recorder state
// (command_messages, parallel_agents_messages, streaming_text_message_id)
// plus turn_id, turn_started, and assistant_output_started, and marks the
// session status Error.
// Guards against stale recorder keys surviving a failed turn.
#[test]
fn shared_codex_turn_completed_error_clears_recorder_state() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-turn-completed-error-clears-recorder");

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
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                pending_turn_start_request_id: Some("turn-start-1".to_owned()),
                recorder: SessionRecorderState {
                    command_messages: HashMap::from([(
                        "search".to_owned(),
                        "command-message".to_owned(),
                    )]),
                    parallel_agents_messages: HashMap::from([(
                        "parallel".to_owned(),
                        "parallel-message".to_owned(),
                    )]),
                    streaming_text_message_id: Some("stream-message".to_owned()),
                },
                thread_id: Some("conversation-123".to_owned()),
                turn_id: Some("turn-1".to_owned()),
                completed_turn_id: None,
                turn_started: true,
                turn_state: CodexTurnState {
                    current_agent_message_id: Some("stream-message".to_owned()),
                    assistant_output_started: true,
                    ..CodexTurnState::default()
                },
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-1",
                "error": {
                    "message": "Turn failed"
                }
            }
        }
    });

    handle_shared_codex_app_server_message(
        &turn_completed,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .expect("turn/completed error should be handled");

    let sessions = runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    let session_state = sessions
        .get(&session_id)
        .expect("shared Codex session state should exist");
    assert!(session_state.recorder.command_messages.is_empty());
    assert!(session_state.recorder.parallel_agents_messages.is_empty());
    assert_eq!(session_state.recorder.streaming_text_message_id, None);
    assert_eq!(session_state.turn_id, None);
    assert!(!session_state.turn_started);
    assert_eq!(session_state.turn_state.current_agent_message_id, None);
    assert!(!session_state.turn_state.assistant_output_started);
    drop(sessions);

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Error);
}

// Pins that a standalone "error" notification (turnId in params) clears
// the same recorder/turn state that a turn/completed error would, and
// flips session status to Error.
// Guards against error notifications being treated as cosmetic while
// recorder state silently persists into the next turn.
#[test]
fn shared_codex_error_notification_clears_recorder_state() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-error-notification-clears-recorder");

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
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                pending_turn_start_request_id: Some("turn-start-1".to_owned()),
                recorder: SessionRecorderState {
                    command_messages: HashMap::from([(
                        "search".to_owned(),
                        "command-message".to_owned(),
                    )]),
                    parallel_agents_messages: HashMap::from([(
                        "parallel".to_owned(),
                        "parallel-message".to_owned(),
                    )]),
                    streaming_text_message_id: Some("stream-message".to_owned()),
                },
                thread_id: Some("conversation-123".to_owned()),
                turn_id: Some("turn-1".to_owned()),
                completed_turn_id: None,
                turn_started: true,
                turn_state: CodexTurnState {
                    current_agent_message_id: Some("stream-message".to_owned()),
                    assistant_output_started: true,
                    ..CodexTurnState::default()
                },
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests = Arc::new(Mutex::new(HashMap::new()));
    let error_notice = json!({
        "method": "error",
        "params": {
            "threadId": "conversation-123",
            "turnId": "turn-1",
            "message": "Codex runtime failure"
        }
    });

    handle_shared_codex_app_server_message(
        &error_notice,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .expect("error notification should be handled");

    let sessions = runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned");
    let session_state = sessions
        .get(&session_id)
        .expect("shared Codex session state should exist");
    assert!(session_state.recorder.command_messages.is_empty());
    assert!(session_state.recorder.parallel_agents_messages.is_empty());
    assert_eq!(session_state.recorder.streaming_text_message_id, None);
    assert_eq!(session_state.turn_id, None);
    assert!(!session_state.turn_started);
    assert_eq!(session_state.turn_state.current_agent_message_id, None);
    assert!(!session_state.turn_state.assistant_output_started);
    drop(sessions);

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Error);
}

// Pins that handle_shared_codex_prompt_command clears stale
// command_messages/streaming_text keys at dispatch time, so even if the
// next turn's item/started notification arrives BEFORE turn/started, the
// recorder still creates a fresh Message::Command.
// Guards against pre-turn-started notifications mutating the previous
// turn's command entry through leftover recorder keys.
#[test]
fn shared_codex_prompt_dispatch_clears_stale_command_state_before_turn_started_notification() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-prompt-dispatch-clears-stale-state");

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
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    state
        .upsert_command_message(
            &session_id,
            "old-command-message",
            "Web search: previous turn",
            "previous turn",
            CommandStatus::Success,
        )
        .unwrap();

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                recorder: SessionRecorderState {
                    command_messages: HashMap::from([(
                        "webSearch".to_owned(),
                        "old-command-message".to_owned(),
                    )]),
                    parallel_agents_messages: HashMap::new(),
                    streaming_text_message_id: Some("stale-stream".to_owned()),
                },
                thread_id: Some("conversation-123".to_owned()),
                turn_id: Some("turn-old".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let pending_requests_for_response = pending_requests.clone();
    let response_thread = std::thread::spawn(move || {
        for _ in 0..100 {
            let sender = {
                let mut pending = pending_requests_for_response
                    .lock()
                    .expect("Codex pending requests mutex poisoned");
                if let Some(request_id) = pending.keys().next().cloned() {
                    pending.remove(&request_id)
                } else {
                    None
                }
            };
            if let Some(sender) = sender {
                let _ = sender.send(Ok(json!({
                    "turn": {
                        "id": "turn-new"
                    }
                })));
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("turn/start request should be pending");
    });

    let mut writer = Vec::new();
    // The session already has a thread_id so the fast path is taken and
    // input_tx is unused, but the parameter is still required.
    let (dummy_input_tx, _dummy_input_rx) = mpsc::channel::<CodexRuntimeCommand>();
    handle_shared_codex_prompt_command(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        &dummy_input_tx,
        &session_id,
        CodexPromptCommand {
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "check the repo".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .unwrap();
    response_thread
        .join()
        .expect("turn/start response thread should join cleanly");

    let item_started = json!({
        "method": "item/started",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "type": "webSearch",
                "query": "serde_json value",
                "action": {
                    "type": "search",
                    "queries": ["serde_json value"]
                }
            }
        }
    });
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-new"
            }
        }
    });
    let item_completed = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "type": "webSearch",
                "query": "serde_json value",
                "action": {
                    "type": "search",
                    "queries": ["serde_json value"]
                }
            }
        }
    });

    handle_shared_codex_app_server_message(
        &item_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &item_completed,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    let command_messages = session
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::Command {
                command,
                output,
                status,
                ..
            } => Some((command.clone(), output.clone(), *status)),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        command_messages,
        vec![
            (
                "Web search: previous turn".to_owned(),
                "previous turn".to_owned(),
                CommandStatus::Success,
            ),
            (
                "Web search: serde_json value".to_owned(),
                "serde_json value".to_owned(),
                CommandStatus::Success,
            ),
        ]
    );
}

// Pins the turn/start race: if turn/started lands while the turn/start
// JSON-RPC request is still being written, the fast notification must NOT
// reintroduce pending_turn_start_request_id once handle_shared_codex_start_turn
// returns — its post-write state merge wins.
// Guards against the notification path re-setting pending state that the
// writer path has already cleared.
#[test]
fn shared_codex_turn_started_notification_does_not_restore_pending_state() {
    struct RaceWriter<F: FnMut()> {
        buffer: Vec<u8>,
        injected: bool,
        on_turn_start_written: F,
    }

    impl<F: FnMut()> std::io::Write for RaceWriter<F> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.buffer.extend_from_slice(buf);
            if !self.injected && self.buffer.ends_with(b"\n") {
                let line = std::str::from_utf8(&self.buffer)
                    .expect("turn/start payload should stay valid UTF-8");
                if line.contains("\"method\":\"turn/start\"") {
                    self.injected = true;
                    (self.on_turn_start_written)();
                }
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) = test_shared_codex_runtime("shared-codex-turn-start-race");

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
        inner.sessions[index].session.status = SessionStatus::Active;
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

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let callback_state = state.clone();
    let callback_pending_requests = pending_requests.clone();
    let callback_sessions = runtime.sessions.clone();
    let callback_thread_sessions = runtime.thread_sessions.clone();
    let callback_runtime_id = runtime.runtime_id.clone();
    let (callback_input_tx, _callback_input_rx) = mpsc::channel::<CodexRuntimeCommand>();
    let mut writer = RaceWriter {
        buffer: Vec::new(),
        injected: false,
        on_turn_start_written: move || {
            handle_shared_codex_app_server_message(
                &json!({
                    "method": "turn/started",
                    "params": {
                        "threadId": "conversation-123",
                        "turn": {
                            "id": "turn-fast"
                        }
                    }
                }),
                &callback_state,
                &callback_runtime_id,
                &callback_pending_requests,
                &callback_sessions,
                &callback_thread_sessions,
                &callback_input_tx,
            )
            .expect("turn/started callback should be handled");
        },
    };

    handle_shared_codex_start_turn(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &session_id,
        "conversation-123",
        CodexPromptCommand {
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "inspect race handling".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .unwrap();

    {
        let sessions = runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned");
        let session_state = sessions
            .get(&session_id)
            .expect("shared Codex session state should exist");
        assert_eq!(session_state.turn_id.as_deref(), Some("turn-fast"));
        assert!(session_state.turn_started);
        assert_eq!(session_state.pending_turn_start_request_id, None);
    }

    let (_request_id, sender) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));
    sender
        .send(Ok(json!({
            "turn": {
                "id": "turn-fast"
            }
        })))
        .unwrap();
}

// Pins that if the StartTurnAfterSetup channel hand-off fails (input_rx
// dropped), the provisional thread registration is rolled back: runtime
// cleared, external_session_id cleared, shared thread_id cleared, and the
// thread_sessions map no longer contains the conversation id.
// Guards against orphaned thread mappings lingering after a failed handoff.
#[test]
fn shared_codex_thread_setup_handoff_failure_rolls_back_registration() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _runtime_input_rx, process) =
        test_shared_codex_runtime("shared-codex-thread-setup-handoff-failure");

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
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = Vec::new();
    let (input_tx, input_rx) = mpsc::channel::<CodexRuntimeCommand>();
    drop(input_rx);

    handle_shared_codex_prompt_command(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        &input_tx,
        &session_id,
        CodexPromptCommand {
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "start the turn".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .unwrap();

    let (_request_id, sender) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));
    sender
        .send(Ok(json!({
            "thread": {
                "id": "conversation-orphan"
            }
        })))
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let (runtime_cleared, external_session_id, status, preview) = {
            let inner = state.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&session_id)
                .expect("Codex session should exist");
            let record = &inner.sessions[index];
            (
                matches!(record.runtime, SessionRuntime::None),
                record.external_session_id.clone(),
                record.session.status,
                record.session.preview.clone(),
            )
        };
        let (shared_thread_id, has_thread_mapping) = {
            let sessions = runtime
                .sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            let thread_id = sessions
                .get(&session_id)
                .and_then(|session| session.thread_id.clone());
            drop(sessions);
            let thread_sessions = runtime
                .thread_sessions
                .lock()
                .expect("shared Codex thread mutex poisoned");
            (
                thread_id,
                thread_sessions.contains_key("conversation-orphan"),
            )
        };

        if runtime_cleared
            && external_session_id.is_none()
            && shared_thread_id.is_none()
            && !has_thread_mapping
        {
            assert_eq!(status, SessionStatus::Error);
            assert!(preview.contains("failed to queue shared Codex turn/start after thread setup"));
            break;
        }

        assert!(
            std::time::Instant::now() < deadline,
            "failed StartTurnAfterSetup handoff should roll back provisional thread registration"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

// Pins that a persistence failure during post-thread-setup state commit
// leaves the shared Codex runtime handle intact on the session, marks the
// session Error with a generic "Failed to save session state" preview, and
// blocks the StartTurnAfterSetup command from being queued.
// Guards against persistence IO errors tearing down a healthy runtime or
// leaking a thread mapping the caller cannot recover from.
#[test]
fn shared_codex_thread_setup_persist_failure_does_not_tear_down_runtime() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _runtime_input_rx, process) =
        test_shared_codex_runtime("shared-codex-thread-setup-persist-failure");
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-shared-codex-thread-setup-persist-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

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
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    state.persist_tx = mpsc::channel().0;

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = Vec::new();
    let (input_tx, input_rx) = mpsc::channel::<CodexRuntimeCommand>();

    handle_shared_codex_prompt_command(
        &mut writer,
        &pending_requests,
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &runtime.thread_sessions,
        &input_tx,
        &session_id,
        CodexPromptCommand {
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "start the turn".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .unwrap();

    let (_request_id, sender) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));
    sender
        .send(Ok(json!({
            "thread": {
                "id": "conversation-persist-failure"
            }
        })))
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        match input_rx.try_recv() {
            Ok(_) => panic!("failed thread registration should not queue StartTurnAfterSetup"),
            Err(mpsc::TryRecvError::Disconnected) => {
                panic!("test input channel should remain open")
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }

        let failed = {
            let snapshot = state.full_snapshot();
            snapshot
                .sessions
                .iter()
                .find(|session| session.id == session_id)
                .is_some_and(|session| {
                    session.status == SessionStatus::Error
                        && session.preview.contains("Failed to save session state")
                })
        };
        if failed {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "failed thread registration should mark the session error"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        assert!(matches!(
            &inner.sessions[index].runtime,
            SessionRuntime::Codex(handle) if handle.runtime_id == runtime.runtime_id
        ));
    }
    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Error);
    assert!(
        session.preview.contains("Failed to save session state"),
        "persistence-failure preview should use generic message, got: {}",
        session.preview,
    );
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. })
            if text.contains("Turn failed: Failed to save session state")
    ));
    assert!(
        !runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("conversation-persist-failure"),
        "failed thread registration should not publish a shared thread mapping"
    );

    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Pins that handle_shared_codex_start_turn with a runtime_id that no
// longer matches the session's current runtime is a no-op: no bytes
// written, no pending request registered, no session entry inserted, and
// active_codex_* config fields stay None.
// Guards against stale StartTurnAfterSetup handoffs writing a different
// runtime's config onto the current session.
#[test]
fn shared_codex_stale_start_turn_handoff_skips_runtime_config_persistence() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (other_runtime, _other_input_rx) = test_codex_runtime_handle("other-runtime");
    let sessions = SharedCodexSessions::new();
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let mut writer = Vec::new();

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(other_runtime);
        inner.sessions[index].active_codex_approval_policy = None;
        inner.sessions[index].active_codex_reasoning_effort = None;
        inner.sessions[index].active_codex_sandbox_mode = None;
    }

    handle_shared_codex_start_turn(
        &mut writer,
        &pending_requests,
        &state,
        "stale-runtime",
        &sessions,
        &session_id,
        "conversation-stale",
        CodexPromptCommand {
            approval_policy: CodexApprovalPolicy::OnRequest,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "stale handoff".to_owned(),
            reasoning_effort: CodexReasoningEffort::XHigh,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::DangerFullAccess,
        },
    )
    .unwrap();

    assert!(writer.is_empty());
    assert!(
        pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .is_empty()
    );
    assert!(
        sessions
            .lock()
            .expect("shared Codex session mutex poisoned")
            .is_empty()
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&session_id)
        .expect("Codex session should exist");
    assert_eq!(inner.sessions[index].active_codex_approval_policy, None);
    assert_eq!(inner.sessions[index].active_codex_reasoning_effort, None);
    assert_eq!(inner.sessions[index].active_codex_sandbox_mode, None);
}

// Pins that set_external_session_id_if_runtime_matches surfaces Err when
// the commit fails rather than silently collapsing into the stale-session
// "skip" branch that returns Ok(()).
// Guards against persistence failures being masked as routine stale-runtime
// misses and thus going undetected by callers.
#[test]
fn set_external_session_id_if_runtime_matches_reports_persist_failure() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx) = test_codex_runtime_handle("persist-thread-id-runtime");
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-codex-thread-id-persist-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    state.persist_tx = mpsc::channel().0;

    let result = state.set_external_session_id_if_runtime_matches(
        &session_id,
        &RuntimeToken::Codex("persist-thread-id-runtime".to_owned()),
        "conversation-123".to_owned(),
    );

    assert!(
        result.is_err(),
        "commit failures should not collapse into stale-session misses"
    );
    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Pins that record_codex_runtime_config_if_runtime_matches propagates
// persistence errors as Err instead of folding them into the stale-session
// skip path — the sibling helper to set_external_session_id_if_runtime_matches.
// Guards against runtime-config persistence errors being lost to callers.
#[test]
fn record_codex_runtime_config_if_runtime_matches_reports_persist_failure() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx) = test_codex_runtime_handle("persist-runtime-config-runtime");
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-codex-runtime-config-persist-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(&session_id)
            .expect("Codex session should exist");
        inner.sessions[index].runtime = SessionRuntime::Codex(runtime);
    }
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    state.persist_tx = mpsc::channel().0;

    let result = state.record_codex_runtime_config_if_runtime_matches(
        &session_id,
        &RuntimeToken::Codex("persist-runtime-config-runtime".to_owned()),
        CodexSandboxMode::WorkspaceWrite,
        CodexApprovalPolicy::Never,
        CodexReasoningEffort::Medium,
    );

    assert!(
        result.is_err(),
        "persistence failures should remain fatal to the caller"
    );
    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Pins that a persistence failure during handle_shared_codex_start_turn's
// runtime-config commit keeps the shared runtime attached to the session
// (SessionRuntime::Codex stays), flips session to Error, and records a
// "Turn failed: Failed to save session state" transcript message.
// Guards against one session's persistence IO killing the shared process.
#[test]
fn shared_codex_start_turn_persist_failure_does_not_tear_down_runtime() {
    let mut state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _runtime_input_rx, process) =
        test_shared_codex_runtime("shared-codex-start-turn-persist-failure");
    let failing_persistence_path = std::env::temp_dir().join(format!(
        "termal-shared-codex-start-turn-persist-failure-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&failing_persistence_path)
        .expect("failing persistence directory should exist");

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
        inner.sessions[index].session.status = SessionStatus::Active;
    }
    state.persistence_path = Arc::new(failing_persistence_path.clone());
    state.persist_tx = mpsc::channel().0;

    handle_shared_codex_start_turn(
        &mut Vec::new(),
        &Arc::new(Mutex::new(HashMap::new())),
        &state,
        &runtime.runtime_id,
        &runtime.sessions,
        &session_id,
        "conversation-123",
        CodexPromptCommand {
            approval_policy: CodexApprovalPolicy::Never,
            attachments: Vec::new(),
            cwd: "/tmp".to_owned(),
            model: "gpt-5.4".to_owned(),
            prompt: "start the turn".to_owned(),
            reasoning_effort: CodexReasoningEffort::Medium,
            resume_thread_id: None,
            sandbox_mode: CodexSandboxMode::WorkspaceWrite,
        },
    )
    .unwrap();

    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(&session_id)
        .expect("Codex session should exist");
    assert!(matches!(
        &inner.sessions[index].runtime,
        SessionRuntime::Codex(handle) if handle.runtime_id == runtime.runtime_id
    ));
    drop(inner);

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Error);
    assert!(
        session.preview.contains("Failed to save session state"),
        "persistence-failure preview should use generic message, got: {}",
        session.preview,
    );
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. })
            if text.contains("Turn failed: Failed to save session state")
    ));

    let _ = fs::remove_dir_all(failing_persistence_path);
}

// Pins the shared_codex_event_matches_visible_turn predicate: it accepts
// events for the active turn_id, accepts events whose turn_id matches the
// completed_turn_id ONLY when there is no active turn, and rejects
// orphan/mismatched turn ids.
// Guards against the grace-period branch firing while a new turn is live,
// or orphan events being accepted with no turn context at all.
#[test]
fn shared_codex_event_matches_visible_turn_handles_active_and_completed_turns() {
    assert!(shared_codex_event_matches_visible_turn(
        Some("turn-active"),
        None,
        Some("turn-active"),
    ));
    assert!(shared_codex_event_matches_visible_turn(
        None,
        Some("turn-completed"),
        Some("turn-completed"),
    ));
    assert!(!shared_codex_event_matches_visible_turn(
        None,
        Some("turn-completed"),
        Some("turn-other"),
    ));
    assert!(!shared_codex_event_matches_visible_turn(
        None,
        Some("turn-completed"),
        None,
    ));
    // No active or completed turn — event with a turn ID is rejected.
    assert!(!shared_codex_event_matches_visible_turn(
        None,
        None,
        Some("turn-orphan"),
    ));
    // Active turn differs from event but completed turn matches — the
    // completed branch is only entered when current_turn_id is None.
    assert!(!shared_codex_event_matches_visible_turn(
        Some("turn-active"),
        Some("turn-completed"),
        Some("turn-completed"),
    ));
}

// Pins the app-server error classifier: only the exact "session
// `<id>` not found" shape (possibly wrapped in context) counts as a stale
// session; "session ... message ... not found", "anchor message not
// found", and unrelated errors are fatal.
// Guards against over-broad stale-session matching that would swallow
// genuine persist/runtime failures as routine stale-session skips.
#[test]
fn shared_codex_app_server_error_classifier_only_ignores_missing_sessions() {
    assert!(shared_codex_app_server_error_is_stale_session(&anyhow!(
        "session `session-1` not found"
    )));
    assert!(shared_codex_app_server_error_is_stale_session(
        &anyhow!("session `session-1` not found").context("wrapped")
    ));
    assert!(!shared_codex_app_server_error_is_stale_session(&anyhow!(
        "session `session-1` message `message-1` not found"
    )));
    assert!(!shared_codex_app_server_error_is_stale_session(
        &anyhow!("session `session-1` message `message-1` not found").context("wrapped")
    ));
    assert!(!shared_codex_app_server_error_is_stale_session(&anyhow!(
        "session `session-1` anchor message `message-1` not found"
    )));
    assert!(!shared_codex_app_server_error_is_stale_session(&anyhow!(
        "failed to persist Codex notice"
    )));
}

// Pins that a server-initiated JSON-RPC request whose thread_id maps to
// no known session is auto-rejected with a -32001 "Session unavailable"
// error, routed back through the CodexRuntimeCommand::JsonRpcResponse
// writer-loop path.
// Guards against Codex hanging on requests the runtime cannot deliver.
#[test]
fn shared_codex_undeliverable_server_request_returns_json_rpc_error() {
    let state = test_app_state();
    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let sessions = SharedCodexSessions::new();
    let thread_sessions: SharedCodexThreadMap = Arc::new(Mutex::new(HashMap::new()));
    let (input_tx, input_rx) = mpsc::channel::<CodexRuntimeCommand>();

    handle_shared_codex_app_server_message(
        &json!({
            "jsonrpc": "2.0",
            "id": "request-missing-session",
            "method": "session/request_permission",
            "params": {
                "threadId": "missing-thread"
            }
        }),
        &state,
        "shared-codex-missing-session",
        &pending_requests,
        &sessions,
        &thread_sessions,
        &input_tx,
    )
    .unwrap();

    match input_rx.recv_timeout(Duration::from_secs(1)).unwrap() {
        CodexRuntimeCommand::JsonRpcResponse { response } => {
            assert_eq!(
                codex_json_rpc_response_message(&response),
                json!({
                    "jsonrpc": "2.0",
                    "id": "request-missing-session",
                    "error": {
                        "code": -32001,
                        "message": "Session unavailable; request could not be delivered."
                    }
                })
            );
        }
        _ => panic!("expected JSON-RPC rejection"),
    }
}

// Pins that while handle_shared_codex_prompt_command blocks waiting on a
// turn/start JSON-RPC response, the writer loop still accepts and
// forwards other CodexRuntimeCommand::JsonRpcResponse items (e.g. an
// approval reply) to the shared stdin.
// Guards against turn/start dispatch starving the writer loop and stalling
// concurrent approval/response traffic on the shared Codex process.
#[test]
fn shared_codex_prompt_command_keeps_writer_loop_responsive_while_turn_start_is_pending() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _runtime_input_rx, process) =
        test_shared_codex_runtime("shared-codex-prompt-writer-responsive");

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
        inner.sessions[index].session.status = SessionStatus::Active;
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

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let writer = SharedBufferWriter::default();
    let thread_writer = writer.clone();
    let thread_pending_requests = pending_requests.clone();
    let thread_state = state.clone();
    let thread_runtime = runtime.clone();
    let (input_tx, input_rx) = mpsc::channel();
    let thread_input_tx = input_tx.clone();

    let writer_thread = std::thread::spawn(move || {
        let mut stdin = thread_writer;
        let runtime_token = RuntimeToken::Codex(thread_runtime.runtime_id.clone());
        while let Ok(command) = input_rx.recv_timeout(Duration::from_millis(250)) {
            match command {
                CodexRuntimeCommand::Prompt {
                    session_id,
                    command,
                } => {
                    handle_shared_codex_prompt_command_result(
                        &thread_state,
                        &session_id,
                        &runtime_token,
                        handle_shared_codex_prompt_command(
                            &mut stdin,
                            &thread_pending_requests,
                            &thread_state,
                            &thread_runtime.runtime_id,
                            &thread_runtime.sessions,
                            &thread_runtime.thread_sessions,
                            &thread_input_tx,
                            &session_id,
                            command,
                        ),
                    )
                    .unwrap();
                }
                CodexRuntimeCommand::StartTurnAfterSetup {
                    session_id,
                    thread_id,
                    command,
                } => {
                    handle_shared_codex_prompt_command_result(
                        &thread_state,
                        &session_id,
                        &runtime_token,
                        handle_shared_codex_start_turn(
                            &mut stdin,
                            &thread_pending_requests,
                            &thread_state,
                            &thread_runtime.runtime_id,
                            &thread_runtime.sessions,
                            &session_id,
                            &thread_id,
                            command,
                        ),
                    )
                    .unwrap();
                }
                CodexRuntimeCommand::JsonRpcResponse { response } => {
                    write_codex_json_rpc_message(
                        &mut stdin,
                        &codex_json_rpc_response_message(&response),
                    )
                    .unwrap();
                }
                _ => panic!("unexpected shared Codex runtime command"),
            }
        }
    });

    input_tx
        .send(CodexRuntimeCommand::Prompt {
            session_id: session_id.clone(),
            command: CodexPromptCommand {
                approval_policy: CodexApprovalPolicy::Never,
                attachments: Vec::new(),
                cwd: "/tmp".to_owned(),
                model: "gpt-5.4".to_owned(),
                prompt: "check the repo".to_owned(),
                reasoning_effort: CodexReasoningEffort::Medium,
                resume_thread_id: None,
                sandbox_mode: CodexSandboxMode::WorkspaceWrite,
            },
        })
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let written = writer.contents();
        let pending_count = pending_requests
            .lock()
            .expect("Codex pending requests mutex poisoned")
            .len();
        if written.contains("\"method\":\"turn/start\"") && pending_count == 1 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "turn/start request should stay pending while the writer loop remains active"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    input_tx
        .send(CodexRuntimeCommand::JsonRpcResponse {
            response: CodexJsonRpcResponseCommand {
                request_id: json!("approval-1"),
                payload: CodexJsonRpcResponsePayload::Result(json!({
                    "outcome": "approved",
                })),
            },
        })
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let written = writer.contents();
        if written.contains("\"id\":\"approval-1\"") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "writer loop should still write JSON-RPC responses while turn/start is pending"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let (_request_id, sender) =
        take_pending_codex_request(&pending_requests, Duration::from_secs(1));
    sender
        .send(Ok(json!({
            "turn": {
                "id": "turn-1"
            }
        })))
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let (turn_id, pending_turn_start) = {
            let sessions = runtime
                .sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            let session_state = sessions
                .get(&session_id)
                .expect("shared Codex session state should exist");
            (
                session_state.turn_id.clone(),
                session_state.pending_turn_start_request_id.clone(),
            )
        };
        if turn_id.as_deref() == Some("turn-1") && pending_turn_start.is_none() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "turn/start waiter should record the turn id after the response arrives"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    drop(input_tx);
    writer_thread
        .join()
        .expect("shared Codex writer thread should join cleanly");
}

// Pins that a CodexResponseError::JsonRpc from turn/start is recorded as
// a session-scoped turn failure (status Error, preview set to the error
// message, "Turn failed: ..." transcript message), while the shared
// runtime handle stays attached to the session record.
// Guards against one turn's JSON-RPC rejection tearing down the entire
// shared Codex process for all co-hosted sessions.
#[test]
fn shared_codex_prompt_json_rpc_errors_fail_the_turn_without_tearing_down_runtime() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-prompt-jsonrpc-error");
    let runtime_token = RuntimeToken::Codex(runtime.runtime_id.clone());

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
        inner.sessions[index].session.status = SessionStatus::Active;
        inner.sessions[index].session.preview = "Waiting for Codex".to_owned();
    }

    handle_shared_codex_prompt_command_result(
        &state,
        &session_id,
        &runtime_token,
        Err(anyhow::Error::new(CodexResponseError::JsonRpc(
            "turn/start rejected the request".to_owned(),
        ))),
    )
    .expect("JSON-RPC prompt errors should be recorded as turn failures");

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.status, SessionStatus::Error);
    assert_eq!(session.preview, "turn/start rejected the request");
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. })
            if text == "Turn failed: turn/start rejected the request"
    ));

    let inner = state.inner.lock().expect("state mutex poisoned");
    let record = inner
        .sessions
        .iter()
        .find(|record| record.session.id == session_id)
        .expect("Codex session should exist");
    assert!(matches!(record.runtime, SessionRuntime::Codex(_)));
}

// Pins that an item/agentMessage/delta arriving while turn_started=false
// is held (no transcript write) and then takes effect once turn/started
// flips the flag for the matching turn_id.
// Guards against streamed text appearing before Codex has acknowledged the
// turn, which would ride ahead of turn-setup notices.
#[test]
fn shared_codex_app_server_agent_message_delta_waits_for_turn_started() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-app-server-agent-delta-turn-started");

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
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                turn_id: Some("turn-current".to_owned()),
                turn_started: false,
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let delta = json!({
        "method": "item/agentMessage/delta",
        "params": {
            "threadId": "conversation-123",
            "itemId": "msg-1",
            "delta": "Hello"
        }
    });
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-current"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &delta,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &delta,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert_eq!(session.preview, "Hello");
    assert!(matches!(
        session.messages.last(),
        Some(Message::Text { text, .. }) if text == "Hello"
    ));
}

// Pins that a server-initiated request (item/tool/requestUserInput) is
// held while turn_started=false, and only records a Message::UserInputRequest
// once turn/started has fired for the matching turn_id.
// Guards against user-input prompts rendering before the turn is actually
// live, which would mix tool-input UI into setup noise.
#[test]
fn shared_codex_app_server_request_waits_for_turn_started() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let (runtime, _input_rx, process) =
        test_shared_codex_runtime("shared-codex-app-server-request-turn-started");

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
        inner.sessions[index].session.status = SessionStatus::Active;
    }

    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("conversation-123".to_owned()),
                turn_id: Some("turn-current".to_owned()),
                turn_started: false,
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert("conversation-123".to_owned(), session_id.clone());

    let pending_requests: CodexPendingRequestMap = Arc::new(Mutex::new(HashMap::new()));
    let request = json!({
        "id": "req-1",
        "method": "item/tool/requestUserInput",
        "params": {
            "threadId": "conversation-123",
            "questions": [
                {
                    "header": "Scope",
                    "id": "scope",
                    "question": "What should Codex review?"
                }
            ]
        }
    });
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-current"
            }
        }
    });

    handle_shared_codex_app_server_message(
        &request,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(session.messages.is_empty());

    handle_shared_codex_app_server_message(
        &turn_started,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();
    handle_shared_codex_app_server_message(
        &request,
        &state,
        &runtime.runtime_id,
        &pending_requests,
        &runtime.sessions,
        &runtime.thread_sessions,
        &mpsc::channel::<CodexRuntimeCommand>().0,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("updated session should be present");
    assert!(matches!(
        session.messages.last(),
        Some(Message::UserInputRequest { title, detail, state, .. })
            if title == "Codex needs input"
                && detail == "Codex requested additional input for \"Scope\"."
                && *state == InteractionRequestState::Pending
    ));
}
