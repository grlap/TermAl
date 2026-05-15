// Claude Code CLI recorder and turn-state tests.
//
// The anthropic/claude-code CLI emits an NDJSON stream on stdout that TermAl
// parses via `handle_claude_stdout_message` in `src/runtime.rs`. Each line is
// an `assistant`, `user`, `stream_event`, or `result` envelope.
// `ClaudeTurnState` accumulates per-turn bookkeeping — pending tool uses
// keyed by `tool_use_id`, parallel sub-agents spawned via the `task` tool,
// the streamed assistant text buffer, and approval keys already seen — and
// is finalized by a `result` event or torn down when the runtime exits.
//
// Streamed text reconciliation is the trickiest seam: Claude emits a stream
// of `text_delta` chunks and then a final full-text payload inside an
// `assistant` frame after `message_stop`. `handle_claude_streamed_text` must
// append the missing suffix when the final is longer, skip the duplicate
// when the final matches, and REPLACE the bubble when the final diverges.
// Parallel agents (the `task` tool) spawn sub-recorders that fan progress
// into the parent transcript; their tool-use / tool-result / tool-error
// frames are folded into `ParallelAgentProgress` entries and recorded as
// subagent results. Transcript boundary: a `tool_use` arriving after
// streamed text ends must start a follow-up `Message`, not append to the
// closed text bubble. Production surfaces under test live in
// `src/runtime.rs`: `handle_claude_stdout_message`, `handle_claude_tool_use`,
// `handle_claude_tool_result`, the `handle_claude_task_tool_*` family,
// `handle_claude_streamed_text`, and `handle_claude_result`.

use super::*;

fn claude_permission_request(tool_name: &str, tool_input: Value) -> Value {
    json!({
        "type": "control_request",
        "request_id": "permission-request-1",
        "request": {
            "subtype": "can_use_tool",
            "tool_name": tool_name,
            "input": tool_input
        }
    })
}

// Pins read-only auto-approval as a filtered Claude permission mode, not a
// shortcut to full `AutoApprove`. Read-only Bash commands may proceed without
// surfacing an approval card so `/review-local` can finish unattended.
#[test]
fn claude_read_only_auto_approve_allows_read_only_bash_permission_request() {
    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &claude_permission_request(
            "Bash",
            json!({
                "command": "git diff --cached -- src/delegations.rs | head -40"
            }),
        ),
        &mut turn_state,
        ClaudeApprovalMode::ReadOnlyAutoApprove,
    )
    .unwrap()
    .expect("permission request should be classified");

    let ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow {
        request_id,
        updated_input,
    }) = action
    else {
        panic!("read-only bash permission should be auto-allowed");
    };

    assert_eq!(request_id, "permission-request-1");
    assert_eq!(
        updated_input.get("command").and_then(Value::as_str),
        Some("git diff --cached -- src/delegations.rs | head -40")
    );
}

#[test]
fn claude_read_only_auto_approve_allows_review_local_bash_shapes() {
    for command in [
        "git status --short",
        "git diff --cached -- src/delegations.rs",
        "git diff --name-only && git diff --cached --name-only",
        "find .claude/reviewers -name \"*.md\" 2>/dev/null",
        "grep -n ReadOnlyAutoApprove src/claude.rs | head -20",
        "grep -n 'two words' docs/bugs.md",
        "sed -n 1,120p src/claude.rs",
        "sed -e 's/window/door/' src/main.rs",
        "sed -e 's/^/word /' src/main.rs",
        "grep -n 'a & b' docs/bugs.md",
        "cat docs/bugs.md | tail -40",
        "wc -l src/claude.rs",
    ] {
        let mut turn_state = ClaudeTurnState::default();
        let action = classify_claude_control_request(
            &claude_permission_request("Bash", json!({ "command": command })),
            &mut turn_state,
            ClaudeApprovalMode::ReadOnlyAutoApprove,
        )
        .unwrap()
        .expect("permission request should be classified");

        let ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow {
            request_id,
            updated_input,
        }) = action
        else {
            panic!("read-only review-local command should be auto-allowed: {command}");
        };

        assert_eq!(request_id, "permission-request-1");
        assert_eq!(
            updated_input.get("command").and_then(Value::as_str),
            Some(command)
        );
    }
}

// Pins read-only Claude reviewer delegations denying explicit file mutation
// tool requests. This closes the bug where read-only reviewers used full
// `AutoApprove` and could allow `Write`/`Edit` operations.
#[test]
fn claude_read_only_auto_approve_denies_write_permission_request() {
    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &claude_permission_request(
            "Write",
            json!({
                "file_path": "src/main.rs",
                "content": "mutated"
            }),
        ),
        &mut turn_state,
        ClaudeApprovalMode::ReadOnlyAutoApprove,
    )
    .unwrap()
    .expect("permission request should be classified");

    let ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Deny {
        request_id,
        message,
    }) = action
    else {
        panic!("write permission should be denied");
    };

    assert_eq!(request_id, "permission-request-1");
    assert!(message.contains("read-only"));
}

#[test]
fn claude_read_only_auto_approve_denies_unsafe_bash_permission_request() {
    let mut turn_state = ClaudeTurnState::default();
    let action = classify_claude_control_request(
        &claude_permission_request(
            "Bash",
            json!({
                "command": "echo mutated > README.md"
            }),
        ),
        &mut turn_state,
        ClaudeApprovalMode::ReadOnlyAutoApprove,
    )
    .unwrap()
    .expect("permission request should be classified");

    let ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Deny {
        request_id,
        message,
    }) = action
    else {
        panic!("unsafe bash permission should be denied");
    };

    assert_eq!(request_id, "permission-request-1");
    assert!(message.contains("read-only"));
}

#[test]
fn claude_read_only_auto_approve_denies_mutating_git_find_and_sed_shapes() {
    for command in [
        "git branch -D old-branch",
        "git branch -m old-name new-name",
        "git branch new-branch",
        "find . -execdir rm {} \\;",
        "find . -fls files.txt",
        "find . -fprint files.txt",
        "find . -ok rm {} \\;",
        "find . '-execdir' rm {} \\;",
        "sed --in-place s/a/b/ src/main.rs",
        "sed -i.bak s/a/b/ src/main.rs",
        "sed -e w/out.txt src/main.rs",
        "sed '-i.bak' s/a/b/ src/main.rs",
        "sed -e 'w out.txt' src/main.rs",
        "sed -f script.sed src/main.rs",
        "sed 'w/tmp/out' src/main.rs",
        "sed '1w/tmp/out' src/main.rs",
        "sed -n '/foo/w/tmp/out' src/main.rs",
        "sed -e 's/a/b/w out.txt' src/main.rs",
        "sed -e 'W out.txt' src/main.rs",
        "sed -e 'e date' src/main.rs",
        "git diff --output=out.patch",
        "git log --output out.log",
        "git show --output=out.patch HEAD",
        "git diff --ext-diff",
        "git diff --textconv",
        "git grep -O pattern",
        "git grep --open-files-in-pager pattern",
        "git branch --set-upstream-to=origin/main",
        "git branch --unset-upstream",
        "git branch --edit-description",
        "git branch --create-reflog",
        "rg --pre 'cat' pattern src",
        "cat README.md & touch /tmp/termal-owned",
    ] {
        let mut turn_state = ClaudeTurnState::default();
        let action = classify_claude_control_request(
            &claude_permission_request("Bash", json!({ "command": command })),
            &mut turn_state,
            ClaudeApprovalMode::ReadOnlyAutoApprove,
        )
        .unwrap()
        .expect("permission request should be classified");

        let ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Deny {
            request_id,
            message,
        }) = action
        else {
            panic!("mutating read-only-looking command should be denied: {command}");
        };

        assert_eq!(request_id, "permission-request-1");
        assert!(message.contains("read-only"));
    }
}

// Pins `clear_claude_turn_state` zeroing every field of `ClaudeTurnState` —
// approval keys, parallel agent group key and order, pending tools, the
// streamed text buffer, the `saw_text_delta` flag, and
// `permission_denied_this_turn`. Guards against leaking per-turn state
// (stale pending tools, phantom parallel agents, already-seen approvals)
// into the next Claude turn, which would corrupt the next transcript.
#[test]
fn clear_claude_turn_state_resets_all_fields() {
    let mut state = ClaudeTurnState {
        approval_keys_this_turn: HashSet::from(["approval-1".to_owned()]),
        parallel_agent_group_key: Some("group-1".to_owned()),
        parallel_agent_order: vec!["agent-1".to_owned()],
        parallel_agents: HashMap::from([(
            "agent-1".to_owned(),
            ParallelAgentProgress {
                detail: Some("Working".to_owned()),
                id: "agent-1".to_owned(),
                source: ParallelAgentSource::Tool,
                status: ParallelAgentStatus::Running,
                title: "Agent 1".to_owned(),
            },
        )]),
        permission_denied_this_turn: true,
        pending_tools: HashMap::from([(
            "tool-1".to_owned(),
            ClaudeToolUse {
                command: Some("echo hi".to_owned()),
                description: Some("Shell".to_owned()),
                file_path: Some("README.md".to_owned()),
                name: "bash".to_owned(),
                subagent_type: Some("worker".to_owned()),
            },
        )]),
        streamed_assistant_text: "partial".to_owned(),
        saw_text_delta: true,
    };

    clear_claude_turn_state(&mut state);

    assert!(state.approval_keys_this_turn.is_empty());
    assert_eq!(state.parallel_agent_group_key, None);
    assert!(state.parallel_agent_order.is_empty());
    assert!(state.parallel_agents.is_empty());
    assert!(!state.permission_denied_this_turn);
    assert!(state.pending_tools.is_empty());
    assert!(state.streamed_assistant_text.is_empty());
    assert!(!state.saw_text_delta);
}

// Pins `reset_claude_turn_state` as the softer variant used at end-of-turn:
// it runs the full `clear_claude_turn_state` field wipe plus finalizes any
// open streaming text bubble on the recorder and calls `reset_turn_state`.
// Guards against a result envelope leaving a half-streamed text bubble open
// or failing to notify the recorder that the turn has ended, which would
// leak partial text into the next turn's transcript.
#[test]
fn reset_claude_turn_state_clears_all_fields_and_finishes_streaming_text() {
    let mut state = ClaudeTurnState {
        approval_keys_this_turn: HashSet::from(["approval-1".to_owned()]),
        parallel_agent_group_key: Some("group-1".to_owned()),
        parallel_agent_order: vec!["agent-1".to_owned()],
        parallel_agents: HashMap::from([(
            "agent-1".to_owned(),
            ParallelAgentProgress {
                detail: Some("Working".to_owned()),
                id: "agent-1".to_owned(),
                source: ParallelAgentSource::Tool,
                status: ParallelAgentStatus::Running,
                title: "Agent 1".to_owned(),
            },
        )]),
        permission_denied_this_turn: true,
        pending_tools: HashMap::from([(
            "tool-1".to_owned(),
            ClaudeToolUse {
                command: Some("echo hi".to_owned()),
                description: Some("Shell".to_owned()),
                file_path: Some("README.md".to_owned()),
                name: "bash".to_owned(),
                subagent_type: Some("worker".to_owned()),
            },
        )]),
        streamed_assistant_text: "partial".to_owned(),
        saw_text_delta: true,
    };
    let mut recorder = TestRecorder {
        streaming_text_delta_start: Some(2),
        streaming_text_active: true,
        ..TestRecorder::default()
    };

    reset_claude_turn_state(&mut state, &mut recorder).unwrap();

    assert!(state.approval_keys_this_turn.is_empty());
    assert_eq!(state.parallel_agent_group_key, None);
    assert!(state.parallel_agent_order.is_empty());
    assert!(state.parallel_agents.is_empty());
    assert!(!state.permission_denied_this_turn);
    assert!(state.pending_tools.is_empty());
    assert!(state.streamed_assistant_text.is_empty());
    assert!(!state.saw_text_delta);
    assert_eq!(recorder.reset_turn_state_calls, 1);
    assert_eq!(recorder.finish_streaming_text_calls, 2);
    assert_eq!(recorder.streaming_text_delta_start, None);
    assert!(!recorder.streaming_text_active);
}

// Pins `handle_claude_tool_use` fanning out two concurrent `task` tool_use
// frames into a pair of `ParallelAgentProgress` entries titled by
// `description`, both in `Initializing` status with detail "Initializing...".
// Guards against the `task` fan-out being lost, collapsed into a single
// agent, or recorded with the wrong status so the UI would show only one
// sub-agent instead of the full group running in parallel.
#[test]
fn claude_task_tool_use_updates_parallel_agent_progress() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "task-1",
                        "name": "Task",
                        "input": {
                            "description": "Rust code review",
                            "subagent_type": "general-purpose"
                        }
                    },
                    {
                        "type": "tool_use",
                        "id": "task-2",
                        "name": "Task",
                        "input": {
                            "description": "Architecture code review",
                            "subagent_type": "general-purpose"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("parallel agents update should be recorded");
    assert_eq!(latest.len(), 2);
    assert_eq!(latest[0].title, "Rust code review");
    assert_eq!(latest[0].detail.as_deref(), Some("Initializing..."));
    assert_eq!(latest[0].status, ParallelAgentStatus::Initializing);
    assert_eq!(latest[1].title, "Architecture code review");
    assert_eq!(latest[1].status, ParallelAgentStatus::Initializing);
}

// Pins `handle_claude_task_tool_result` advancing an initializing
// `ParallelAgentProgress` to `Completed` with a single-line detail preview,
// and emitting a `push_subagent_result` carrying the full multi-line body.
// Guards against the parent transcript losing the sub-agent's return value
// or the progress card being stuck in `Initializing` after the task tool
// returns successfully.
#[test]
fn claude_task_tool_result_updates_parallel_agents_and_records_subagent_result() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "task-1",
                        "name": "Task",
                        "input": {
                            "description": "Rust code review",
                            "subagent_type": "general-purpose"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let detail = "Reviewer found a batching bug in location smoothing.\nRead src/state.rs for the stale preview path.";
    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "task-1",
                        "content": detail
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("completed parallel agent update should be recorded");
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].title, "Rust code review");
    assert_eq!(latest[0].source, ParallelAgentSource::Tool);
    assert_eq!(latest[0].status, ParallelAgentStatus::Completed);
    assert_eq!(
        latest[0].detail.as_deref(),
        Some("Reviewer found a batching bug in location smoothing.")
    );
    assert_eq!(
        recorder.subagent_results,
        vec![("Rust code review".to_owned(), detail.to_owned())]
    );
}

// Pins Claude task-result updates reclaiming an existing progress row as
// tool-sourced. This is a release-mode guard: a source mismatch must not
// silently preserve a delegation-routable id on a Claude Task row.
#[test]
fn claude_task_tool_result_resets_existing_non_tool_progress_source() {
    let mut turn_state = ClaudeTurnState {
        parallel_agent_group_key: Some("group-1".to_owned()),
        parallel_agent_order: vec!["task-1".to_owned()],
        parallel_agents: HashMap::from([(
            "task-1".to_owned(),
            ParallelAgentProgress {
                detail: Some("Running".to_owned()),
                id: "task-1".to_owned(),
                source: ParallelAgentSource::Delegation,
                status: ParallelAgentStatus::Running,
                title: "Task agent".to_owned(),
            },
        )]),
        pending_tools: HashMap::from([(
            "task-1".to_owned(),
            ClaudeToolUse {
                command: None,
                description: Some("Rust code review".to_owned()),
                file_path: None,
                name: "Task".to_owned(),
                subagent_type: Some("general-purpose".to_owned()),
            },
        )]),
        ..ClaudeTurnState::default()
    };
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "task-1",
                        "content": "Reviewer finished."
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("parallel agent source repair should be recorded");
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].id, "task-1");
    assert_eq!(latest[0].source, ParallelAgentSource::Tool);
    assert_eq!(latest[0].status, ParallelAgentStatus::Completed);
    assert_eq!(
        turn_state
            .parallel_agents
            .get("task-1")
            .expect("task row should remain")
            .source,
        ParallelAgentSource::Tool,
    );
}

// Pins `handle_claude_task_tool_error` flipping the progress entry to
// `Error` with the first failure line as the preview detail, while handing
// the full multi-line payload (stack trace and all) to the recorder via
// `push_subagent_result`. Guards against failure diagnostics being
// truncated to the preview or dropped entirely, which would hide the real
// cause of the sub-agent failure from the user.
#[test]
fn claude_task_tool_error_records_full_failure_detail() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "task-1",
                        "name": "Task",
                        "input": {
                            "description": "Rust code review",
                            "subagent_type": "general-purpose"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let detail = "Reviewer failed to parse the diff.\nStack trace line 1\nStack trace line 2";
    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "task-1",
                        "is_error": true,
                        "content": detail
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("errored parallel agent update should be recorded");
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].title, "Rust code review");
    assert_eq!(latest[0].status, ParallelAgentStatus::Error);
    assert_eq!(
        latest[0].detail.as_deref(),
        Some("Reviewer failed to parse the diff.")
    );
    assert_eq!(
        recorder.subagent_results,
        vec![("Rust code review".to_owned(), detail.to_owned())]
    );
}

// Pins `handle_claude_task_tool_error` substituting the literal "Task
// failed." string when the tool_result has `is_error: true` but an empty
// content body — used both for the progress detail and for
// `push_subagent_result`. Guards against empty-detail errors producing an
// empty subagent result bubble or a parallel agent card that shows no
// reason for the failure.
#[test]
fn claude_task_tool_error_without_detail_records_fallback_failure_message() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "task-1",
                        "name": "Task",
                        "input": {
                            "description": "Rust code review",
                            "subagent_type": "general-purpose"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "task-1",
                        "is_error": true,
                        "content": ""
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let latest = recorder
        .parallel_agents
        .last()
        .expect("errored parallel agent update should be recorded");
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].status, ParallelAgentStatus::Error);
    assert_eq!(latest[0].detail.as_deref(), Some("Task failed."));
    assert_eq!(
        recorder.subagent_results,
        vec![("Rust code review".to_owned(), "Task failed.".to_owned())]
    );
}

// Pins `handle_claude_streamed_text` reconciling a short stream ("Hello")
// with a longer final assistant text ("Hello there.") arriving after
// `message_stop`, by appending the missing " there." suffix to the open
// bubble so the transcript ends up with the full final text in a single
// `Message::Text`. Guards against lost trailing words when Claude flushes
// the full payload only in the post-`message_stop` `assistant` envelope.
#[test]
fn claude_streamed_text_appends_missing_final_suffix_after_message_stop() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "Hello"
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "message_stop"
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "text",
                        "text": "Hello there."
                    }
                ]
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Hello there."
    ));
}

// Pins `handle_claude_streamed_text` recognizing that the final assistant
// text exactly matches the already-streamed buffer and skipping the append,
// so the transcript keeps a single `Message::Text` rather than duplicating
// the full line. Guards against doubled assistant text in the bubble when
// Claude's post-`message_stop` payload restates the complete streamed body
// verbatim.
#[test]
fn claude_streamed_text_skips_duplicate_final_text_after_message_stop() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "Hello there."
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "message_stop"
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "text",
                        "text": "Hello there."
                    }
                ]
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Hello there."
    ));
}

// Pins `handle_claude_streamed_text` calling `replace_streaming_text` when
// the final assistant body ("Final answer.") is not a prefix-extension of
// the streamed draft ("Draft answer."), so the bubble is rewritten in
// place to the authoritative final text. Guards against TermAl keeping a
// stale early draft (or concatenating draft+final) when Claude rewrites
// its own in-flight text.
#[test]
fn claude_streamed_text_replaces_divergent_final_text() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "Draft answer."
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "text",
                        "text": "Final answer."
                    }
                ]
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");

    assert_eq!(session.messages.len(), 1);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Final answer."
    ));
}

// Pins the transcript boundary: `handle_claude_tool_use` arriving after a
// streamed text bubble has ended must close the text `Message` and start a
// fresh `Message::Command`, then a subsequent stream delta opens yet
// another text bubble — yielding three distinct messages (text, command,
// text) in order. Guards against follow-up tool calls or post-tool text
// being appended to an already-closed text bubble.
#[test]
fn claude_tool_use_after_streamed_text_starts_followup_in_new_message() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "Hello"
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "message_stop"
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "bash-1",
                        "name": "Bash",
                        "input": {
                            "command": "pwd"
                        }
                    }
                ]
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": {
                    "text": "World"
                }
            }
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut external_session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");

    assert_eq!(session.messages.len(), 3);
    assert!(matches!(
        session.messages.first(),
        Some(Message::Text { text, .. }) if text == "Hello"
    ));
    assert!(matches!(
        session.messages.get(1),
        Some(Message::Command {
            command,
            output,
            status,
            ..
        }) if command == "pwd" && output.is_empty() && *status == CommandStatus::Running
    ));
    assert!(matches!(
        session.messages.get(2),
        Some(Message::Text { text, .. }) if text == "World"
    ));
}

// Pins `handle_claude_result` draining `pending_tools` so that a
// tool_result envelope arriving after the turn's `result` is silently
// discarded rather than mutating a recorded command — the Running Bash
// command keeps its original empty output and `Running` status. Guards
// against stray late tool-result frames from Claude retroactively
// rewriting a completed turn's transcript.
#[test]
fn claude_result_clears_pending_tools_and_ignores_late_tool_results() {
    let mut turn_state = ClaudeTurnState::default();
    let mut recorder = TestRecorder::default();
    let mut session_id = None;

    handle_claude_event(
        &json!({
            "type": "assistant",
            "message": {
                "content": [
                    {
                        "type": "tool_use",
                        "id": "bash-1",
                        "name": "Bash",
                        "input": {
                            "command": "pwd"
                        }
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();
    handle_claude_event(
        &json!({
            "type": "result",
            "is_error": false
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    assert!(turn_state.pending_tools.is_empty());

    handle_claude_event(
        &json!({
            "type": "user",
            "message": {
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "bash-1",
                        "content": "/tmp/late"
                    }
                ]
            }
        }),
        &mut session_id,
        &mut turn_state,
        &mut recorder,
    )
    .unwrap();

    assert_eq!(
        recorder.commands,
        vec![("pwd".to_owned(), String::new(), CommandStatus::Running)]
    );
}

// Pins `handle_claude_result` resetting the recorder's command-id keying
// between turns so a second turn reusing the same `tool_use_id` ("bash-1")
// registers a fresh command rather than overwriting the prior turn's
// completed Bash message — both commands end up persisted with their own
// output and `Success` status. Guards against cross-turn id collisions
// merging two independent Bash invocations into one transcript entry.
#[test]
fn claude_result_resets_recorder_command_keys_between_turns() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Claude);
    let mut recorder = SessionRecorder::new(state.clone(), session_id.clone());
    let mut turn_state = ClaudeTurnState::default();
    let mut external_session_id = None;

    for (command, output) in [("pwd", "/tmp/one"), ("git status", "working tree clean")] {
        handle_claude_event(
            &json!({
                "type": "assistant",
                "message": {
                    "content": [
                        {
                            "type": "tool_use",
                            "id": "bash-1",
                            "name": "Bash",
                            "input": {
                                "command": command
                            }
                        }
                    ]
                }
            }),
            &mut external_session_id,
            &mut turn_state,
            &mut recorder,
        )
        .unwrap();
        handle_claude_event(
            &json!({
                "type": "user",
                "message": {
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": "bash-1",
                            "content": output
                        }
                    ]
                }
            }),
            &mut external_session_id,
            &mut turn_state,
            &mut recorder,
        )
        .unwrap();
        handle_claude_event(
            &json!({
                "type": "result",
                "is_error": false
            }),
            &mut external_session_id,
            &mut turn_state,
            &mut recorder,
        )
        .unwrap();
    }

    let snapshot = state.full_snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == session_id)
        .expect("Claude session should exist");
    let commands = session
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
        commands,
        vec![
            (
                "pwd".to_owned(),
                "/tmp/one".to_owned(),
                CommandStatus::Success
            ),
            (
                "git status".to_owned(),
                "working tree clean".to_owned(),
                CommandStatus::Success
            ),
        ]
    );
}
