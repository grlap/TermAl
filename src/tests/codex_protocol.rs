// Codex protocol handler tests.
//
// TermAl drives OpenAI's Codex CLI in two distinct modes, and every event type
// needs a handler for each. The shared app server mode spawns a single
// persistent `codex` process that speaks JSON-RPC and multiplexes many
// concurrent sessions over one socket; the REPL mode spawns one `codex repl`
// subprocess per TermAl session and talks to it as a one-shot conversation.
// The two protocol dialects overlap but are not identical, so the translation
// layer in `src/state.rs` (shared) and `src/runtime.rs` / `src/turns.rs`
// (REPL) is the seam where regressions hide.
//
// The `codex_app_server_*` tests exercise `handle_codex_app_server_request`
// and `handle_codex_app_server_item_{started,completed}`: inbound JSON-RPC
// requests (command/file/permissions approvals, user input, MCP elicitation,
// generic tool calls) and item lifecycle events (web searches, file changes).
// The `repl_codex_*` tests exercise `handle_repl_codex_app_server_notification`
// and `handle_repl_codex_task_complete`: streaming text reconciliation
// between deltas and `item/completed` payloads (suffix append, divergent
// replace, duplicate skip), `task_complete` ordering against the final
// `agent_message`, and late agent messages arriving after `turn/completed`.
// The `codex_delta_suffix_*` tests exercise `next_codex_delta_suffix`, the
// dedup primitive shared by both modes that collapses Codex's cumulative /
// overlapping stream chunks while staying safe on UTF-8 char boundaries.
//
// All tests operate on the pure event-handler functions in isolation: no
// real subprocess is spawned, no socket is opened. `TestRecorder` stands in
// for the production `TurnRecorder` / `CodexTurnRecorder` sinks and captures
// what would otherwise have been written to the session transcript, so each
// test can assert exactly which transcript side-effects the handler emitted.

use super::*;

// pins that `item/commandExecution/requestApproval` is translated into a
// `CodexApprovalKind::CommandExecution` pending approval with the command,
// cwd, and reason composed into the detail string. exercises
// `handle_codex_app_server_request`. guards against regressions where the
// approval prompt forgets the cwd/reason or loses the JSON-RPC `id`, which
// would leave the user unable to respond and the Codex runtime deadlocked
// waiting on a reply that will never come.
#[test]
fn codex_app_server_command_approval_request_records_pending_approval() {
    let mut recorder = TestRecorder::default();
    let message = json!({
        "id": "req-1",
        "params": {
            "command": "cargo test",
            "cwd": "/tmp/project",
            "reason": "Need to verify the fix."
        }
    });

    handle_codex_app_server_request(
        "item/commandExecution/requestApproval",
        &message,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(recorder.codex_approvals.len(), 1);
    assert!(matches!(
        recorder.codex_approvals.first(),
        Some((title, command, detail, approval))
            if title == "Codex needs approval"
                && command == "cargo test"
                && detail == "Codex requested approval to execute this command in /tmp/project. Reason: Need to verify the fix."
                && matches!(approval.kind, CodexApprovalKind::CommandExecution)
                && approval.request_id == json!("req-1")
    ));
}

// pins that `item/fileChange/requestApproval` yields a
// `CodexApprovalKind::FileChange` pending approval with the fixed
// "Apply file changes" command label and a detail string embedding the
// caller-supplied reason. exercises `handle_codex_app_server_request`.
// guards against the file-change branch silently falling through to the
// generic app-request handler, which would surface a raw JSON-RPC method
// name in the UI instead of the human-readable approval prompt.
#[test]
fn codex_app_server_file_change_approval_request_records_pending_approval() {
    let mut recorder = TestRecorder::default();
    let message = json!({
        "id": "req-2",
        "params": {
            "reason": "Need to update generated files."
        }
    });

    handle_codex_app_server_request("item/fileChange/requestApproval", &message, &mut recorder)
        .unwrap();

    assert_eq!(recorder.codex_approvals.len(), 1);
    assert!(matches!(
        recorder.codex_approvals.first(),
        Some((title, command, detail, approval))
            if title == "Codex needs approval"
                && command == "Apply file changes"
                && detail == "Codex requested approval to apply file changes. Reason: Need to update generated files."
                && matches!(approval.kind, CodexApprovalKind::FileChange)
                && approval.request_id == json!("req-2")
    ));
}

// pins that `item/permissions/requestApproval` is summarised into a
// human-readable detail sentence (filesystem paths, network, macOS
// preferences, automation bundle ids) and that the full permission payload
// round-trips into `CodexApprovalKind::Permissions`. exercises
// `handle_codex_app_server_request` and `describe_codex_permission_request`.
// guards against the approval dialog dropping scope information the user
// needs to make a safe decision, and against the response path losing the
// original permission set it must echo back to Codex.
#[test]
fn codex_app_server_permissions_approval_request_records_pending_approval() {
    let mut recorder = TestRecorder::default();
    let requested_permissions = json!({
        "fileSystem": {
            "read": ["/repo/docs"],
            "write": ["/repo/src"]
        },
        "network": {
            "enabled": true
        },
        "macos": {
            "preferences": "system",
            "automations": {
                "bundle_ids": ["com.apple.Terminal"]
            }
        }
    });
    let message = json!({
        "id": "req-3",
        "params": {
            "permissions": requested_permissions,
            "reason": "Need access to update build scripts."
        }
    });

    handle_codex_app_server_request("item/permissions/requestApproval", &message, &mut recorder)
        .unwrap();

    assert_eq!(recorder.codex_approvals.len(), 1);
    let (title, command, detail, approval) = recorder
        .codex_approvals
        .first()
        .expect("Codex permissions approval should be recorded");
    assert_eq!(title, "Codex needs approval");
    assert_eq!(command, "Grant additional permissions");
    assert_eq!(
        detail,
        "Codex requested approval to grant additional permissions: read access to `/repo/docs`, write access to `/repo/src`, network access, macOS preferences access (system), macOS automation access for `com.apple.Terminal`. Reason: Need access to update build scripts."
    );
    match &approval.kind {
        CodexApprovalKind::Permissions {
            requested_permissions,
        } => {
            assert_eq!(
                requested_permissions,
                &json!({
                    "fileSystem": {
                        "read": ["/repo/docs"],
                        "write": ["/repo/src"]
                    },
                    "network": {
                        "enabled": true
                    },
                    "macos": {
                        "preferences": "system",
                        "automations": {
                            "bundle_ids": ["com.apple.Terminal"]
                        }
                    }
                })
            );
        }
        _ => panic!("expected Codex permissions approval"),
    }
    assert_eq!(approval.request_id, json!("req-3"));
}

// pins that `item/tool/requestUserInput` is decoded into a
// `CodexPendingUserInput` with its question list intact, including the
// `is_secret` flag on secret-bearing questions. exercises
// `handle_codex_app_server_request` and the `UserInputQuestion` deserde
// path. guards against dropping the `isSecret` flag (which would cause
// TermAl to echo API tokens into the transcript in clear text) or
// reordering questions away from the schema Codex expects in the reply.
#[test]
fn codex_app_server_user_input_request_records_pending_request() {
    let mut recorder = TestRecorder::default();
    let message = json!({
        "id": "req-input-1",
        "params": {
            "questions": [
                {
                    "header": "Environment",
                    "id": "environment",
                    "question": "Which environment should I use?",
                    "options": [
                        {
                            "label": "Production",
                            "description": "Use the production cluster."
                        },
                        {
                            "label": "Staging",
                            "description": "Use the staging environment."
                        }
                    ]
                },
                {
                    "header": "API token",
                    "id": "apiToken",
                    "question": "Paste the temporary token.",
                    "isSecret": true
                }
            ]
        }
    });

    handle_codex_app_server_request("item/tool/requestUserInput", &message, &mut recorder).unwrap();

    assert_eq!(recorder.codex_user_input_requests.len(), 1);
    let (title, detail, questions, request) = recorder
        .codex_user_input_requests
        .first()
        .expect("Codex user input request should be recorded");
    assert_eq!(title, "Codex needs input");
    assert_eq!(detail, "Codex requested additional input for 2 questions.");
    assert_eq!(questions.len(), 2);
    assert_eq!(questions[0].header, "Environment");
    assert_eq!(questions[1].id, "apiToken");
    assert!(questions[1].is_secret);
    assert_eq!(request.request_id, json!("req-input-1"));
    assert_eq!(request.questions, questions.clone());
}

// pins that `mcpServer/elicitation/request` is parsed into an
// `McpElicitationRequestPayload` with server name, thread id, optional
// turn id, and the `form` mode variant preserved, and is recorded as a
// pending MCP elicitation. exercises `handle_codex_app_server_request`
// and `describe_codex_mcp_elicitation_request`. guards against MCP
// server-driven input requests being mis-routed through the generic
// app-request fallback, which would strip the schema the form UI needs.
#[test]
fn codex_app_server_mcp_elicitation_request_records_pending_request() {
    let mut recorder = TestRecorder::default();
    let message = json!({
        "id": "req-elicit-1",
        "params": {
            "threadId": "thread-1",
            "turnId": "turn-1",
            "serverName": "deployment-helper",
            "mode": "form",
            "message": "Confirm the deployment settings.",
            "requestedSchema": {
                "type": "object",
                "properties": {
                    "environment": {
                        "type": "string",
                        "title": "Environment",
                        "oneOf": [
                            { "const": "production", "title": "Production" },
                            { "const": "staging", "title": "Staging" }
                        ]
                    },
                    "replicas": {
                        "type": "integer",
                        "title": "Replicas"
                    }
                },
                "required": ["environment", "replicas"]
            }
        }
    });

    handle_codex_app_server_request("mcpServer/elicitation/request", &message, &mut recorder)
        .unwrap();

    assert_eq!(recorder.codex_mcp_elicitation_requests.len(), 1);
    let (title, detail, request, pending) = recorder
        .codex_mcp_elicitation_requests
        .first()
        .expect("MCP elicitation request should be recorded");
    assert_eq!(title, "Codex needs MCP input");
    assert_eq!(
        detail,
        "MCP server deployment-helper requested additional structured input. Confirm the deployment settings."
    );
    assert_eq!(request.server_name, "deployment-helper");
    assert_eq!(request.thread_id, "thread-1");
    assert_eq!(request.turn_id.as_deref(), Some("turn-1"));
    assert!(matches!(
        request.mode,
        McpElicitationRequestMode::Form { .. }
    ));
    assert_eq!(pending.request_id, json!("req-elicit-1"));
    assert_eq!(pending.request, *request);
}

// pins that any unrecognised app-server method (here `item/tool/call`)
// falls through to the generic branch, recording the raw method name,
// full params payload, and request id so the user can answer with a
// free-form JSON result. exercises the `_` arm of
// `handle_codex_app_server_request` and `describe_codex_app_server_request`.
// guards against the generic path either swallowing the request silently
// or discarding the params (which Codex needs echoed back verbatim).
#[test]
fn codex_app_server_generic_request_records_pending_request() {
    let mut recorder = TestRecorder::default();
    let message = json!({
        "id": "req-tool-1",
        "params": {
            "toolName": "search_workspace",
            "arguments": {
                "pattern": "Codex"
            }
        }
    });

    handle_codex_app_server_request("item/tool/call", &message, &mut recorder).unwrap();

    assert_eq!(recorder.codex_app_requests.len(), 1);
    let (title, detail, method, params, pending) = recorder
        .codex_app_requests
        .first()
        .expect("generic Codex app request should be recorded");
    assert_eq!(title, "Codex needs a tool result");
    assert_eq!(
        detail,
        "Codex requested a result for `search_workspace`. Review the request payload and submit the JSON result to continue."
    );
    assert_eq!(method, "item/tool/call");
    assert_eq!(
        params,
        &json!({
            "toolName": "search_workspace",
            "arguments": {
                "pattern": "Codex"
            }
        })
    );
    assert_eq!(pending.request_id, json!("req-tool-1"));
}

// pins that a `codex/event/task_complete` notification whose
// `last_agent_message` matches the turn's final `agent_message` is
// buffered and only flushed once the final message arrives, and that the
// flush emits the subagent-result entry *before* the final text.
// exercises `handle_repl_codex_app_server_notification` dispatching into
// `handle_repl_codex_task_complete` and `flush_pending_codex_subagent_results`.
// guards against the subagent summary landing in the transcript before
// (or duplicated alongside) the answer it summarises.
#[test]
fn repl_codex_task_complete_event_buffers_subagent_result_until_final_message() {
    let mut recorder = TestRecorder::default();
    let mut repl_state = ReplCodexSessionState::default();
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
                "message": "Final REPL Codex answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_repl_codex_app_server_notification(
        "turn/started",
        &turn_started,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "codex/event/task_complete",
        &task_complete,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();

    assert!(recorder.subagent_results.is_empty());
    assert!(recorder.texts.is_empty());

    handle_repl_codex_app_server_notification(
        "codex/event/agent_message",
        &final_message,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(
        recorder.subagent_results,
        vec![(
            "Subagent completed".to_owned(),
            "Reviewer found a real bug.".to_owned(),
        )]
    );
    assert_eq!(
        recorder.texts,
        vec![
            "Subagent completed\nReviewer found a real bug.".to_owned(),
            "Final REPL Codex answer.".to_owned(),
        ]
    );
}

// pins that when the final `item/completed` agent-message text extends
// the streamed delta (streamed "Hello", completed "Hello from REPL."),
// only the missing suffix " from REPL." is emitted as an additional
// delta, not the whole completed string. exercises
// `handle_repl_codex_app_server_notification` plus
// `append_codex_streamed_text_with_dedup` / `next_codex_delta_suffix`.
// guards against the transcript showing "HelloHello from REPL." because
// the completed payload was appended wholesale on top of the delta.
#[test]
fn repl_codex_streamed_agent_message_appends_missing_completed_suffix() {
    let mut recorder = TestRecorder::default();
    let mut repl_state = ReplCodexSessionState::default();
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stream-1"
            }
        }
    });
    let delta = json!({
        "method": "item/agentMessage/delta",
        "params": {
            "threadId": "conversation-123",
            "itemId": "item-1",
            "delta": "Hello"
        }
    });
    let completed = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "id": "item-1",
                "type": "agentMessage",
                "text": "Hello from REPL."
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stream-1",
                "error": null
            }
        }
    });

    handle_repl_codex_app_server_notification(
        "turn/started",
        &turn_started,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "item/agentMessage/delta",
        &delta,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "item/completed",
        &completed,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "turn/completed",
        &turn_completed,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(
        recorder.text_deltas,
        vec!["Hello".to_owned(), " from REPL.".to_owned()]
    );
    assert!(recorder.texts.is_empty());
    assert!(repl_state.turn_completed);
    assert!(repl_state.current_turn_id.is_none());
}

// pins that when the final `item/completed` text diverges from the
// streamed delta (stream "Hello from stream" vs completed "Different
// final answer."), the handler emits the completed text as the canonical
// answer and drops the preliminary stream. exercises
// `handle_repl_codex_app_server_notification` and
// `append_codex_streamed_text_with_dedup`. guards against a stale
// streamed preamble staying in the transcript when Codex revises its
// final answer between the delta stream and the completion event.
#[test]
fn repl_codex_streamed_agent_message_replaces_divergent_completed_text() {
    let mut recorder = TestRecorder::default();
    let mut repl_state = ReplCodexSessionState::default();
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stream-1"
            }
        }
    });
    let delta = json!({
        "method": "item/agentMessage/delta",
        "params": {
            "threadId": "conversation-123",
            "itemId": "item-1",
            "delta": "Hello from stream"
        }
    });
    let completed = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "id": "item-1",
                "type": "agentMessage",
                "text": "Different final answer."
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stream-1",
                "error": null
            }
        }
    });
    handle_repl_codex_app_server_notification(
        "turn/started",
        &turn_started,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "item/agentMessage/delta",
        &delta,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "item/completed",
        &completed,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "turn/completed",
        &turn_completed,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    assert_eq!(
        recorder.text_deltas,
        vec!["Different final answer.".to_owned()]
    );
    assert!(recorder.texts.is_empty());
    assert!(repl_state.turn_completed);
    assert!(repl_state.current_turn_id.is_none());
}
// pins that when the `item/completed` text exactly equals the streamed
// delta ("Hello from REPL." both times), the handler emits nothing new
// and the transcript keeps a single copy. exercises
// `handle_repl_codex_app_server_notification` and
// `next_codex_delta_suffix`'s `incoming == existing` early-out.
// guards against the most common duplication bug, where a matching
// completion still appends a second copy of the full answer after the
// streamed one, doubling every agent message in the transcript.
#[test]
fn repl_codex_streamed_agent_message_skips_duplicate_completed_text() {
    let mut recorder = TestRecorder::default();
    let mut repl_state = ReplCodexSessionState::default();
    let turn_started = json!({
        "method": "turn/started",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stream-1"
            }
        }
    });
    let delta = json!({
        "method": "item/agentMessage/delta",
        "params": {
            "threadId": "conversation-123",
            "itemId": "item-1",
            "delta": "Hello from REPL."
        }
    });
    let completed = json!({
        "method": "item/completed",
        "params": {
            "threadId": "conversation-123",
            "item": {
                "id": "item-1",
                "type": "agentMessage",
                "text": "Hello from REPL."
            }
        }
    });
    let turn_completed = json!({
        "method": "turn/completed",
        "params": {
            "threadId": "conversation-123",
            "turn": {
                "id": "turn-stream-1",
                "error": null
            }
        }
    });

    handle_repl_codex_app_server_notification(
        "turn/started",
        &turn_started,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "item/agentMessage/delta",
        &delta,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "item/completed",
        &completed,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "turn/completed",
        &turn_completed,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(recorder.text_deltas, vec!["Hello from REPL.".to_owned()]);
    assert!(recorder.texts.is_empty());
    assert!(repl_state.turn_completed);
    assert!(repl_state.current_turn_id.is_none());
}

// pins that a `codex/event/agent_message` arriving *after* `turn/completed`
// (Codex sometimes flushes the final answer as a trailing event) is still
// recorded to the transcript rather than dropped as out-of-turn. exercises
// `handle_repl_codex_app_server_notification` plus the late-message branch
// in `handle_repl_codex_event_agent_message`. guards against a race where
// the turn looks finished, the state is reset, and the real final answer
// never makes it into the transcript.
#[test]
fn repl_codex_agent_message_after_turn_completed_is_recorded() {
    let mut recorder = TestRecorder::default();
    let mut repl_state = ReplCodexSessionState::default();
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
                "message": "Late REPL answer.",
                "phase": "final_answer",
                "type": "agent_message"
            }
        }
    });

    handle_repl_codex_app_server_notification(
        "turn/started",
        &turn_started,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "turn/completed",
        &turn_completed,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "codex/event/agent_message",
        &late_message,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(recorder.texts, vec!["Late REPL answer.".to_owned()]);
    assert!(repl_state.turn_completed);
    assert!(repl_state.current_turn_id.is_none());
}

// pins the app-server counterpart to the previous test: an
// `item/completed` carrying an `agentMessage` arriving after
// `turn/completed` is still recorded, using the `allow_late_agent_message`
// branch in `handle_repl_codex_app_server_notification`. guards against
// that late-delivery branch only being wired for the
// `codex/event/agent_message` variant, which would cause late
// `item/completed` agent messages to be silently dropped.
#[test]
fn repl_codex_app_server_agent_message_completed_after_turn_completed_is_recorded() {
    let mut recorder = TestRecorder::default();
    let mut repl_state = ReplCodexSessionState::default();
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
                "id": "item-final",
                "type": "agentMessage",
                "text": "Late REPL item answer."
            }
        }
    });

    handle_repl_codex_app_server_notification(
        "turn/started",
        &turn_started,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "turn/completed",
        &turn_completed,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();
    handle_repl_codex_app_server_notification(
        "item/completed",
        &late_item,
        &mut repl_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(recorder.texts, vec!["Late REPL item answer.".to_owned()]);
    assert!(repl_state.turn_completed);
    assert!(repl_state.current_turn_id.is_none());
}

// pins that a `webSearch` item produces a two-phase command lifecycle:
// `item/started` records a running "Web search: <query>" entry with no
// output, and `item/completed` marks the same entry as succeeded with
// the queries list joined by newlines. exercises
// `handle_codex_app_server_item_started` and
// `handle_codex_app_server_item_completed`. guards against the running
// row never flipping to success (leaving the UI pinned on "Running") or
// the extra queries from the search action being dropped.
#[test]
fn codex_app_server_web_search_item_records_command_lifecycle() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let mut recorder = TestRecorder::default();
    let mut turn_state = CodexTurnState::default();
    let item = json!({
        "id": "web-1",
        "type": "webSearch",
        "query": "rust anyhow",
        "action": {
            "type": "search",
            "queries": ["rust anyhow", "serde_json value"]
        }
    });

    handle_codex_app_server_item_started(&item, &mut recorder).unwrap();
    handle_codex_app_server_item_completed(
        &item,
        &state,
        &session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(
        recorder.commands,
        vec![
            (
                "Web search: rust anyhow".to_owned(),
                String::new(),
                CommandStatus::Running,
            ),
            (
                "Web search: rust anyhow".to_owned(),
                "rust anyhow\nserde_json value".to_owned(),
                CommandStatus::Success,
            ),
        ]
    );
}

// pins that a completed `fileChange` item fans out into one diff entry
// per change, with the correct `ChangeType` (`Create` vs `Edit`), a
// human-readable title ("Created <basename>" / "Updated <basename>"),
// and the raw diff body preserved. exercises
// `handle_codex_app_server_item_completed`. guards against mixing up
// `add` and `edit` kinds (which would mis-label new files in the diff
// review UI) or losing the diff payload needed for apply/reject.
#[test]
fn codex_app_server_file_change_item_records_create_and_edit_diffs() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let mut recorder = TestRecorder::default();
    let mut turn_state = CodexTurnState::default();
    let item = json!({
        "type": "fileChange",
        "status": "completed",
        "changes": [
            {
                "path": "src/new.rs",
                "diff": "+fn main() {}\n",
                "kind": {
                    "type": "add"
                }
            },
            {
                "path": "src/lib.rs",
                "diff": "@@ -1 +1 @@\n-old\n+new\n",
                "kind": {
                    "type": "edit"
                }
            }
        ]
    });

    handle_codex_app_server_item_completed(
        &item,
        &state,
        &session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(
        recorder.diffs,
        vec![
            (
                "src/new.rs".to_owned(),
                "Created new.rs".to_owned(),
                "+fn main() {}\n".to_owned(),
                ChangeType::Create,
            ),
            (
                "src/lib.rs".to_owned(),
                "Updated lib.rs".to_owned(),
                "@@ -1 +1 @@\n-old\n+new\n".to_owned(),
                ChangeType::Edit,
            ),
        ]
    );
}

// pins the four branches of `next_codex_delta_suffix`: initial chunk
// seeds the buffer, a cumulative extension yields only the new suffix,
// an exact repeat yields `None`, and an overlapping chunk whose prefix
// matches the buffer tail yields only the non-overlapping tail.
// guards against Codex's habit of sometimes sending cumulative deltas
// ("Try" then "Try these") and sometimes sending overlapping windows
// (" these plain" after "Try these") causing either duplicated tokens
// or dropped text in the streamed transcript.
#[test]
fn codex_delta_suffix_deduplicates_cumulative_and_overlapping_chunks() {
    let mut text = String::new();

    assert_eq!(
        next_codex_delta_suffix(&mut text, "Try"),
        Some("Try".to_owned())
    );
    assert_eq!(text, "Try");

    assert_eq!(
        next_codex_delta_suffix(&mut text, "Try these"),
        Some(" these".to_owned())
    );
    assert_eq!(text, "Try these");

    assert_eq!(next_codex_delta_suffix(&mut text, "Try these"), None);
    assert_eq!(text, "Try these");

    assert_eq!(
        next_codex_delta_suffix(&mut text, " these plain"),
        Some(" plain".to_owned())
    );
    assert_eq!(text, "Try these plain");

    assert_eq!(next_codex_delta_suffix(&mut text, " plain"), None);
    assert_eq!(text, "Try these plain");
}

// pins that `next_codex_delta_suffix` respects UTF-8 char boundaries
// when computing overlap: a 3-byte smart quote (U+2018) that straddles
// the boundary between an existing buffer and an incoming overlapping
// chunk must be detected as one overlapping codepoint, not three
// independent bytes. exercises `next_codex_delta_suffix` via
// `longest_codex_delta_overlap`'s `is_char_boundary` check. guards
// against a panic from slicing mid-codepoint when Codex streams text
// that contains apostrophes, em-dashes, or emoji.
#[test]
fn codex_delta_suffix_handles_multibyte_utf8_characters() {
    let mut text = String::new();

    // Smart quote ' is 3 bytes (U+2018: E2 80 98)
    assert_eq!(
        next_codex_delta_suffix(&mut text, "I\u{2018}m"),
        Some("I\u{2018}m".to_owned())
    );
    assert_eq!(text, "I\u{2018}m");

    // Overlapping chunk that shares the multi-byte char boundary
    assert_eq!(
        next_codex_delta_suffix(&mut text, "\u{2018}m here"),
        Some(" here".to_owned())
    );
    assert_eq!(text, "I\u{2018}m here");
}
