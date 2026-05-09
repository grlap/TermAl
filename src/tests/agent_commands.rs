// Claude Code exposes slash commands from two sources: project
// `.claude/commands/*.md` template files in the session workdir, and
// "native" slash commands that Claude advertises inline in its `initialize`
// control response (bundled, project, and user-scoped entries). TermAl
// merges both into one sorted list served to the frontend command palette.
//
// Surfaces exercised: `read_claude_agent_commands` in `api.rs` (filesystem
// scan), `claude_agent_commands` in `runtime.rs` (initialize-response
// parser for native slash commands), and `sync_session_agent_commands` on
// `AppState` (cache merge + revision bump). The revision counter matters
// because the web UI polls session `agent_commands_revision` to invalidate
// its palette cache; a missed bump leaves stale commands on screen.

use super::*;

// Pins `read_claude_agent_commands` to the top level of `.claude/commands/`,
// `*.md` only, first non-empty line as description, sorted by name.
// Guards against recursing into subdirectories, picking up non-markdown
// siblings, or scrambling the ordering the palette relies on.
#[test]
fn reads_claude_agent_commands_from_markdown_files() {
    let root = std::env::temp_dir().join(format!("termal-agent-commands-{}", Uuid::new_v4()));
    let commands_dir = root.join(".claude").join("commands");

    fs::create_dir_all(commands_dir.join("nested")).unwrap();
    fs::write(
        commands_dir.join("review-local.md"),
        "Review local changes.

## Step 1
Inspect diffs.
",
    )
    .unwrap();
    fs::write(
        commands_dir.join("fix-bug.md"),
        "
Fix a bug from docs/bugs.md by number.

$ARGUMENTS
",
    )
    .unwrap();
    fs::write(commands_dir.join("notes.txt"), "ignore").unwrap();
    fs::write(commands_dir.join("nested").join("ignored.md"), "ignore").unwrap();

    let commands = read_claude_agent_commands(&root).unwrap();

    assert_eq!(
        commands,
        vec![
            AgentCommand {
                kind: AgentCommandKind::PromptTemplate,
                name: "fix-bug".to_owned(),
                description: "Fix a bug from docs/bugs.md by number.".to_owned(),
                content: "
Fix a bug from docs/bugs.md by number.

$ARGUMENTS
"
                .to_owned(),
                source: ".claude/commands/fix-bug.md".to_owned(),
                argument_hint: None,
            },
            AgentCommand {
                kind: AgentCommandKind::PromptTemplate,
                name: "review-local".to_owned(),
                description: "Review local changes.".to_owned(),
                content: "Review local changes.

## Step 1
Inspect diffs.
"
                .to_owned(),
                source: ".claude/commands/review-local.md".to_owned(),
                argument_hint: None,
            },
        ]
    );

    fs::remove_dir_all(root).unwrap();
}

// Pins `read_claude_agent_commands` to returning an empty vector (not an
// error) when the project has no `.claude/commands/` directory at all.
// Guards against a missing directory becoming a hard failure that blocks
// the command palette for every project without template commands.
#[test]
fn returns_empty_agent_commands_when_commands_directory_is_missing() {
    let root =
        std::env::temp_dir().join(format!("termal-agent-commands-missing-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();

    let commands = read_claude_agent_commands(&root).unwrap();
    assert!(commands.is_empty());

    fs::remove_dir_all(root).unwrap();
}

// Pins `AppState::list_agent_commands` to still returning
// `.claude/commands/*.md` templates when the session's agent is Codex;
// template files are project-owned, not agent-owned.
// Guards against hiding project prompt templates behind an agent-kind gate.
#[test]
fn returns_agent_commands_for_non_claude_sessions() {
    let root = std::env::temp_dir().join(format!("termal-agent-commands-codex-{}", Uuid::new_v4()));
    let commands_dir = root.join(".claude").join("commands");
    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("review-local.md"),
        "Review local changes.

Use the active agent's tools.
",
    )
    .unwrap();

    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Session".to_owned()),
            workdir: Some(root.to_string_lossy().into_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let response = state.list_agent_commands(&created.session_id).unwrap();
    assert_eq!(response.commands.len(), 1);
    assert_eq!(response.commands[0].name, "review-local");
    assert_eq!(response.commands[0].description, "Review local changes.");
    assert_eq!(response.commands[0].kind, AgentCommandKind::PromptTemplate);

    fs::remove_dir_all(root).unwrap();
}

// Pins `claude_agent_commands` to parsing the `response.response.commands`
// array into `NativeSlash` entries, stamping `content = "/<name>"` and
// preserving `argumentHint`, and tagging sources as bundled vs. project.
// Guards palette scope grouping that depends on those source labels.
#[test]
fn extracts_claude_native_agent_commands_from_initialize_response() {
    let message = json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "response": {
                "commands": [
                    {
                        "name": "review",
                        "description": "Review the current changes. (bundled)",
                        "argumentHint": ""
                    },
                    {
                        "name": "review-local",
                        "description": "Review local changes. (project)",
                        "argumentHint": "[scope]"
                    }
                ]
            }
        }
    });

    assert_eq!(
        claude_agent_commands(&message),
        Some(vec![
            AgentCommand {
                kind: AgentCommandKind::NativeSlash,
                name: "review".to_owned(),
                description: "Review the current changes.".to_owned(),
                content: "/review".to_owned(),
                source: "Claude bundled command".to_owned(),
                argument_hint: None,
            },
            AgentCommand {
                kind: AgentCommandKind::NativeSlash,
                name: "review-local".to_owned(),
                description: "Review local changes.".to_owned(),
                content: "/review-local".to_owned(),
                source: "Claude project command".to_owned(),
                argument_hint: Some("[scope]".to_owned()),
            },
        ])
    );
}

// Pins `claude_agent_commands` to dropping entries with blank/whitespace
// names and recognizing the `(user)` description suffix as a
// `Claude user command` source tag.
// Guards against whitespace entries polluting the palette and scope
// labels leaking into visible description text.
#[test]
fn extracts_claude_native_agent_commands_filters_empty_names_and_normalizes_user_suffix() {
    let message = json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "response": {
                "commands": [
                    {
                        "name": "   ",
                        "description": "Should be filtered."
                    },
                    {
                        "name": "release-notes",
                        "description": "Draft release notes. (user)"
                    }
                ]
            }
        }
    });

    assert_eq!(
        claude_agent_commands(&message),
        Some(vec![AgentCommand {
            kind: AgentCommandKind::NativeSlash,
            name: "release-notes".to_owned(),
            description: "Draft release notes.".to_owned(),
            content: "/release-notes".to_owned(),
            source: "Claude user command".to_owned(),
            argument_hint: None,
        }])
    );
}

// Pins `claude_agent_commands` to returning `None` (not `Some(vec![])`)
// when Claude's initialize response carries an empty commands array.
// Guards the "no update" signal that callers use to skip bumping the
// session command revision on no-op init payloads.
#[test]
fn extracts_claude_native_agent_commands_returns_none_for_empty_command_list() {
    let message = json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "response": {
                "commands": []
            }
        }
    });

    assert_eq!(claude_agent_commands(&message), None);
}

// Pins the `sync_session_agent_commands` + `list_agent_commands` round-trip
// to merging cached native slash commands with filesystem templates, sorted
// by name and preserving per-entry `kind`, `source`, and `argument_hint`.
// Guards against native entries shadowing templates (or vice versa) when
// the same name appears in both sources.
#[test]
fn returns_cached_claude_native_commands_alongside_template_fallbacks() {
    let root = std::env::temp_dir().join(format!(
        "termal-agent-commands-claude-native-{}",
        Uuid::new_v4()
    ));
    let commands_dir = root.join(".claude").join("commands");
    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("review-local.md"),
        "Review local changes from the filesystem template.",
    )
    .unwrap();
    fs::write(
        commands_dir.join("fix-bug.md"),
        "Fix a bug from docs/bugs.md by number.\n\n$ARGUMENTS\n",
    )
    .unwrap();

    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude Session".to_owned()),
            workdir: Some(root.to_string_lossy().into_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    state
        .sync_session_agent_commands(
            &created.session_id,
            vec![
                AgentCommand {
                    kind: AgentCommandKind::NativeSlash,
                    name: "review".to_owned(),
                    description: "Review the current changes.".to_owned(),
                    content: "/review".to_owned(),
                    source: "Claude bundled command".to_owned(),
                    argument_hint: None,
                },
                AgentCommand {
                    kind: AgentCommandKind::NativeSlash,
                    name: "review-local".to_owned(),
                    description: "Review local changes.".to_owned(),
                    content: "/review-local".to_owned(),
                    source: "Claude project command".to_owned(),
                    argument_hint: Some("[scope]".to_owned()),
                },
            ],
        )
        .unwrap();

    let response = state.list_agent_commands(&created.session_id).unwrap();

    assert_eq!(
        response
            .commands
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        vec!["fix-bug", "review", "review-local"]
    );
    assert_eq!(response.commands[0].kind, AgentCommandKind::PromptTemplate);
    assert_eq!(response.commands[1].kind, AgentCommandKind::NativeSlash);
    assert_eq!(response.commands[2].kind, AgentCommandKind::NativeSlash);
    assert_eq!(
        response.commands[2].argument_hint.as_deref(),
        Some("[scope]")
    );
    assert_eq!(response.commands[2].source, "Claude project command");

    drop(response);
    drop(created);
    drop(state);
    let _ = fs::remove_dir_all(&root);
}

// Pins `AppState::sync_session_agent_commands` to bumping both the global
// state `revision` and the session's `agent_commands_revision` by exactly
// one, visible in the next `snapshot()`.
// Guards the frontend polling contract: stale palette caches only refresh
// when `agent_commands_revision` increases.
#[test]
fn sync_session_agent_commands_bumps_visible_session_command_revision() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude Session".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    let starting_revision = created.revision;
    let starting_session_revision = created.session.agent_commands_revision;

    state
        .sync_session_agent_commands(
            &created.session_id,
            vec![AgentCommand {
                kind: AgentCommandKind::NativeSlash,
                name: "review".to_owned(),
                description: "Review the current changes.".to_owned(),
                content: "/review".to_owned(),
                source: "Claude bundled command".to_owned(),
                argument_hint: None,
            }],
        )
        .unwrap();

    let snapshot = state.snapshot();
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == created.session_id)
        .expect("updated Claude session should exist");
    assert!(snapshot.revision > starting_revision);
    assert_eq!(
        session.agent_commands_revision,
        starting_session_revision.saturating_add(1)
    );
}

// Pins `AppState::list_agent_commands` to returning a 404 `ApiError` with
// the message "session not found" when the session id is unknown.
// Guards the HTTP status contract that `GET /api/sessions/:id/commands`
// relies on to distinguish missing sessions from empty command lists.
#[test]
fn returns_not_found_for_missing_agent_command_session() {
    let state = test_app_state();
    let error = state.list_agent_commands("missing-session").unwrap_err();

    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert_eq!(error.message, "session not found");
}

// Pins backend-owned prompt-template resolution to replacing `$ARGUMENTS`
// and appending the optional user note as a standard block. This keeps
// regular sends and delegations from drifting back to frontend-only template
// expansion.
#[test]
fn resolves_prompt_template_arguments_and_note() {
    let root =
        std::env::temp_dir().join(format!("termal-agent-command-resolve-{}", Uuid::new_v4()));
    let commands_dir = root.join(".claude").join("commands");
    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("fix-bug.md"),
        "Fix the requested bug:\n\n$ARGUMENTS\n\nVerify the fix.\n",
    )
    .unwrap();

    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Session".to_owned()),
            workdir: Some(root.to_string_lossy().into_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let response = state
        .resolve_agent_command(
            &created.session_id,
            "fix-bug",
            ResolveAgentCommandRequest {
                arguments: Some("1024".to_owned()),
                note: Some("Please add integration tests.".to_owned()),
                intent: AgentCommandResolveIntent::Send,
            },
        )
        .unwrap();

    assert_eq!(response.name, "fix-bug");
    assert_eq!(response.kind, AgentCommandKind::PromptTemplate);
    assert_eq!(response.visible_prompt, "/fix-bug 1024");
    assert_eq!(response.title.as_deref(), Some("Fix bug 1024"));
    assert_eq!(
        response.expanded_prompt.as_deref(),
        Some(
            "Fix the requested bug:\n\n1024\n\nVerify the fix.\n\n## Additional User Note\n\nPlease add integration tests."
        )
    );
    assert_eq!(response.delegation, None);

    fs::remove_dir_all(root).unwrap();
}

// Pins resolver-owned delegation policy for `/review-local`. React should
// not hard-code the isolated-worktree special case; the backend returns the
// default when the command is resolved with delegate intent.
#[test]
fn resolves_review_local_delegation_defaults() {
    let root = std::env::temp_dir().join(format!(
        "termal-agent-command-resolve-review-local-{}",
        Uuid::new_v4()
    ));
    let commands_dir = root.join(".claude").join("commands");
    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("review-local.md"),
        "Review staged and unstaged changes.\n",
    )
    .unwrap();

    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Codex),
            name: Some("Codex Session".to_owned()),
            workdir: Some(root.to_string_lossy().into_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();

    let response = state
        .resolve_agent_command(
            &created.session_id,
            "review-local",
            ResolveAgentCommandRequest {
                arguments: None,
                note: None,
                intent: AgentCommandResolveIntent::Delegate,
            },
        )
        .unwrap();

    assert_eq!(response.visible_prompt, "/review-local");
    assert_eq!(
        response.expanded_prompt.as_deref(),
        Some("Review staged and unstaged changes.\n")
    );
    assert_eq!(
        response.delegation,
        Some(ResolvedAgentCommandDelegationDefaults {
            mode: Some(DelegationMode::Reviewer),
            title: Some("Review staged and unstaged changes.".to_owned()),
            write_policy: Some(DelegationWritePolicy::IsolatedWorktree {
                owned_paths: Vec::new(),
                worktree_path: None,
            }),
        })
    );

    fs::remove_dir_all(root).unwrap();
}

// Pins native slash resolution to rejecting notes until TermAl owns a safe
// conversion path. Native runtimes accept literal slash prompts, not appended
// markdown blocks.
#[test]
fn rejects_note_for_native_slash_command_resolution() {
    let state = test_app_state();
    let created = state
        .create_session(CreateSessionRequest {
            agent: Some(Agent::Claude),
            name: Some("Claude Session".to_owned()),
            workdir: Some("/tmp".to_owned()),
            project_id: None,
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        })
        .unwrap();
    state
        .sync_session_agent_commands(
            &created.session_id,
            vec![AgentCommand {
                kind: AgentCommandKind::NativeSlash,
                name: "review".to_owned(),
                description: "Review the current changes.".to_owned(),
                content: "/review".to_owned(),
                source: "Claude bundled command".to_owned(),
                argument_hint: Some("[scope]".to_owned()),
            }],
        )
        .unwrap();

    let error = state
        .resolve_agent_command(
            &created.session_id,
            "review",
            ResolveAgentCommandRequest {
                arguments: Some("staged".to_owned()),
                note: Some("Include integration-test advice.".to_owned()),
                intent: AgentCommandResolveIntent::Send,
            },
        )
        .unwrap_err();

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error.message,
        "native slash commands do not support additional notes"
    );
}
