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

struct TempDirCleanup {
    path: PathBuf,
}

impl TempDirCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for TempDirCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn cache_agent_commands_for_test(state: &AppState, session_id: &str, commands: Vec<AgentCommand>) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(session_id)
        .expect("session should exist");
    inner.sessions[index].agent_commands = commands;
}

// Pins `read_claude_agent_commands` to the top level of `.claude/commands/`,
// `*.md` only, first non-empty line as description, sorted by name.
// Guards against recursing into subdirectories, picking up non-markdown
// siblings, or scrambling the ordering the palette relies on.
#[test]
fn reads_claude_agent_commands_from_markdown_files() {
    let root = std::env::temp_dir().join(format!("termal-agent-commands-{}", Uuid::new_v4()));
    let _cleanup = TempDirCleanup::new(root.clone());
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
                resolver_frontmatter: None,
                resolver_frontmatter_trusted: false,
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
                resolver_frontmatter: None,
                resolver_frontmatter_trusted: false,
            },
        ]
    );
}

#[test]
fn read_claude_agent_commands_rejects_oversized_command_file() {
    let root = std::env::temp_dir().join(format!(
        "termal-agent-commands-oversized-{}",
        Uuid::new_v4()
    ));
    let _cleanup = TempDirCleanup::new(root.clone());
    let commands_dir = root.join(".claude").join("commands");

    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("huge.md"),
        "x".repeat(MAX_AGENT_COMMAND_FILE_BYTES as usize + 1),
    )
    .unwrap();

    let error = read_claude_agent_commands(&root).unwrap_err();

    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert!(
        error.message.contains("must be at most 1048576 bytes"),
        "{}",
        error.message
    );
}

#[test]
fn reads_claude_agent_commands_strip_yaml_frontmatter() {
    let root = std::env::temp_dir().join(format!(
        "termal-agent-commands-frontmatter-{}",
        Uuid::new_v4()
    ));
    let _cleanup = TempDirCleanup::new(root.clone());
    let commands_dir = root.join(".claude").join("commands");

    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("review-local.md"),
        "---
name: review-local
description: Review local changes.
metadata:
  termal:
    delegation:
      enabled: true
      mode: reviewer
      writePolicy:
        kind: isolatedWorktree
---

Template body starts here.

## Step 1
Inspect diffs.
",
    )
    .unwrap();

    let commands = read_claude_agent_commands(&root).unwrap();

    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].description, "Review local changes.");
    assert_eq!(
        commands[0].content,
        "Template body starts here.

## Step 1
Inspect diffs.
"
    );
}

#[test]
fn strip_markdown_frontmatter_handles_edge_cases() {
    let unterminated = "---\ndescription: Missing close\nBody stays intact.\n";
    let parsed = strip_markdown_frontmatter(unterminated);
    assert_eq!(parsed.content, unterminated);
    assert_eq!(parsed.frontmatter, None);
    assert_eq!(parsed.description, None);
    assert_eq!(parsed.argument_hint, None);

    let block_description = "---
description: |
  Multi-line descriptions are outside the tiny parser.
---

Body starts here.
";
    let parsed = strip_markdown_frontmatter(block_description);
    assert_eq!(
        parsed.frontmatter,
        Some("description: |\n  Multi-line descriptions are outside the tiny parser.\n")
    );
    assert_eq!(parsed.description, None);
    assert_eq!(parsed.content, "Body starts here.\n");

    let malformed_scalar = "---
description: value: extra
---

Body.
";
    let parsed = strip_markdown_frontmatter(malformed_scalar);
    assert_eq!(parsed.description, None);
    assert_eq!(parsed.content, "Body.\n");

    let large_description = "x".repeat(70 * 1024);
    let large_frontmatter = format!(
        "---
description: {large_description}
---

Large body.
"
    );
    let parsed = strip_markdown_frontmatter(&large_frontmatter);
    assert_eq!(
        parsed.description.as_deref(),
        Some(large_description.as_str())
    );
    assert_eq!(parsed.content, "Large body.\n");

    let trailing_space_close = concat!(
        "---\n",
        "description: Trailing close\n",
        "---",
        "   \n",
        "\nBody.\n"
    );
    let parsed = strip_markdown_frontmatter(trailing_space_close);
    assert_eq!(parsed.description.as_deref(), Some("Trailing close"));
    assert_eq!(parsed.content, "Body.\n");
}

#[test]
fn reads_claude_agent_commands_fallback_description_for_blank_frontmatter_description() {
    let root = std::env::temp_dir().join(format!(
        "termal-agent-commands-blank-description-{}",
        Uuid::new_v4()
    ));
    let _cleanup = TempDirCleanup::new(root.clone());
    let commands_dir = root.join(".claude").join("commands");

    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("tool-check.md"),
        "---
description:
argument-hint:
---

Run a targeted tool check.
",
    )
    .unwrap();

    let commands = read_claude_agent_commands(&root).unwrap();

    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].description, "Run a targeted tool check.");
    assert_eq!(commands[0].argument_hint, None);
}

#[test]
fn reads_claude_agent_commands_strip_claude_only_frontmatter() {
    let root = std::env::temp_dir().join(format!(
        "termal-agent-commands-claude-frontmatter-{}",
        Uuid::new_v4()
    ));
    let _cleanup = TempDirCleanup::new(root.clone());
    let commands_dir = root.join(".claude").join("commands");

    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("tool-check.md"),
        "---
argument-hint: PATH
allowed-tools: Bash(rg:*), Read
---

Run a targeted tool check.
",
    )
    .unwrap();

    let commands = read_claude_agent_commands(&root).unwrap();

    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].description, "Run a targeted tool check.");
    assert_eq!(commands[0].content, "Run a targeted tool check.\n");
    assert_eq!(commands[0].argument_hint.as_deref(), Some("PATH"));
}

#[test]
fn reads_claude_agent_commands_strip_crlf_frontmatter() {
    let root = std::env::temp_dir().join(format!(
        "termal-agent-commands-crlf-frontmatter-{}",
        Uuid::new_v4()
    ));
    let _cleanup = TempDirCleanup::new(root.clone());
    let commands_dir = root.join(".claude").join("commands");
    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("crlf-check.md"),
        "---\r\ndescription: CRLF command\r\nargument-hint: PATH\r\n---\r\n\r\nRun CRLF check for $ARGUMENTS.\r\n",
    )
    .unwrap();

    let commands = read_claude_agent_commands(&root).unwrap();

    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].name, "crlf-check");
    assert_eq!(commands[0].description, "CRLF command");
    assert_eq!(commands[0].argument_hint.as_deref(), Some("PATH"));
    assert_eq!(commands[0].content, "Run CRLF check for $ARGUMENTS.\r\n");
    assert!(!commands[0].content.contains("---"));
}

#[test]
fn reads_claude_agent_commands_strip_model_only_frontmatter() {
    let root = std::env::temp_dir().join(format!(
        "termal-agent-commands-model-frontmatter-{}",
        Uuid::new_v4()
    ));
    let _cleanup = TempDirCleanup::new(root.clone());
    let commands_dir = root.join(".claude").join("commands");

    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("model-check.md"),
        "---
model: opus
---

Run with the command model preference.
",
    )
    .unwrap();

    let commands = read_claude_agent_commands(&root).unwrap();

    assert_eq!(commands.len(), 1);
    assert_eq!(
        commands[0].description,
        "Run with the command model preference."
    );
    assert_eq!(
        commands[0].content,
        "Run with the command model preference.\n"
    );
}

#[test]
fn reads_claude_agent_commands_strip_tools_and_disable_model_frontmatter() {
    let root = std::env::temp_dir().join(format!(
        "termal-agent-commands-tools-frontmatter-{}",
        Uuid::new_v4()
    ));
    let _cleanup = TempDirCleanup::new(root.clone());
    let commands_dir = root.join(".claude").join("commands");

    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("disable-model.md"),
        "---
disable-model-invocation: true
---

Run without model invocation.
",
    )
    .unwrap();
    fs::write(
        commands_dir.join("tools-check.md"),
        "---
tools:
  - Bash
  - Read
---

Run with declared tools.
",
    )
    .unwrap();

    let commands = read_claude_agent_commands(&root).unwrap();

    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].name, "disable-model");
    assert_eq!(commands[0].description, "Run without model invocation.");
    assert_eq!(commands[0].content, "Run without model invocation.\n");
    assert_eq!(commands[1].name, "tools-check");
    assert_eq!(commands[1].description, "Run with declared tools.");
    assert_eq!(commands[1].content, "Run with declared tools.\n");
}

#[test]
fn reads_claude_agent_commands_preserve_thematic_breaks() {
    let root = std::env::temp_dir().join(format!(
        "termal-agent-commands-thematic-break-{}",
        Uuid::new_v4()
    ));
    let _cleanup = TempDirCleanup::new(root.clone());
    let commands_dir = root.join(".claude").join("commands");

    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("divider.md"),
        "---
Checklist:
- Keep this prompt body intact.
---

Run the check.
",
    )
    .unwrap();

    let commands = read_claude_agent_commands(&root).unwrap();

    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].description, "Checklist:");
    assert!(commands[0].content.starts_with("---\nChecklist:\n"));
    assert!(
        commands[0]
            .content
            .contains("- Keep this prompt body intact.")
    );
}

#[test]
fn reads_claude_agent_commands_ignore_nested_frontmatter_description() {
    let root = std::env::temp_dir().join(format!(
        "termal-agent-commands-nested-description-{}",
        Uuid::new_v4()
    ));
    let _cleanup = TempDirCleanup::new(root.clone());
    let commands_dir = root.join(".claude").join("commands");

    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("nested.md"),
        "---
metadata:
  termal:
    description: Nested metadata should not become the command description.
---

Body description wins.
",
    )
    .unwrap();

    let commands = read_claude_agent_commands(&root).unwrap();

    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].description, "Body description wins.");
    assert_eq!(commands[0].content, "Body description wins.\n");
}

// Pins `read_claude_agent_commands` to returning an empty vector (not an
// error) when the project has no `.claude/commands/` directory at all.
// Guards against a missing directory becoming a hard failure that blocks
// the command palette for every project without template commands.
#[test]
fn returns_empty_agent_commands_when_commands_directory_is_missing() {
    let root =
        std::env::temp_dir().join(format!("termal-agent-commands-missing-{}", Uuid::new_v4()));
    let _cleanup = TempDirCleanup::new(root.clone());
    fs::create_dir_all(&root).unwrap();

    let commands = read_claude_agent_commands(&root).unwrap();
    assert!(commands.is_empty());
}

// Pins `AppState::list_agent_commands` to still returning
// `.claude/commands/*.md` templates when the session's agent is Codex;
// template files are project-owned, not agent-owned.
// Guards against hiding project prompt templates behind an agent-kind gate.
#[test]
fn returns_agent_commands_for_non_claude_sessions() {
    let root = std::env::temp_dir().join(format!("termal-agent-commands-codex-{}", Uuid::new_v4()));
    let _cleanup = TempDirCleanup::new(root.clone());
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
                resolver_frontmatter: None,
                resolver_frontmatter_trusted: false,
            },
            AgentCommand {
                kind: AgentCommandKind::NativeSlash,
                name: "review-local".to_owned(),
                description: "Review local changes.".to_owned(),
                content: "/review-local".to_owned(),
                source: "Claude project command".to_owned(),
                argument_hint: Some("[scope]".to_owned()),
                resolver_frontmatter: None,
                resolver_frontmatter_trusted: false,
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
            resolver_frontmatter: None,
            resolver_frontmatter_trusted: false,
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
                    resolver_frontmatter: None,
                    resolver_frontmatter_trusted: false,
                },
                AgentCommand {
                    kind: AgentCommandKind::NativeSlash,
                    name: "review-local".to_owned(),
                    description: "Review local changes.".to_owned(),
                    content: "/review-local".to_owned(),
                    source: "Claude project command".to_owned(),
                    argument_hint: Some("[scope]".to_owned()),
                    resolver_frontmatter: None,
                    resolver_frontmatter_trusted: false,
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
                resolver_frontmatter: None,
                resolver_frontmatter_trusted: false,
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

// Pins the runtime sync boundary to accepting only native slash commands.
// Prompt templates are trusted only when read from the session workdir's
// `.claude/commands/*.md` files; runtime-advertised template entries must not
// be cached where they could impersonate trusted filesystem templates.
#[test]
fn sync_session_agent_commands_filters_runtime_prompt_templates() {
    let root = std::env::temp_dir().join(format!(
        "termal-agent-command-runtime-filter-{}",
        Uuid::new_v4()
    ));
    let _cleanup = TempDirCleanup::new(root.clone());
    fs::create_dir_all(&root).unwrap();

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
                    kind: AgentCommandKind::PromptTemplate,
                    name: "review-local".to_owned(),
                    description: "Runtime-provided prompt template.".to_owned(),
                    content: "Runtime review $ARGUMENTS".to_owned(),
                    source: ".claude/commands/review-local.md".to_owned(),
                    argument_hint: None,
                    resolver_frontmatter: None,
                    resolver_frontmatter_trusted: false,
                },
                AgentCommand {
                    kind: AgentCommandKind::NativeSlash,
                    name: "review".to_owned(),
                    description: "Review the current changes.".to_owned(),
                    content: "/review".to_owned(),
                    source: "Claude bundled command".to_owned(),
                    argument_hint: None,
                    resolver_frontmatter: None,
                    resolver_frontmatter_trusted: false,
                },
            ],
        )
        .unwrap();

    let response = state.list_agent_commands(&created.session_id).unwrap();
    assert_eq!(
        response
            .commands
            .iter()
            .map(|command| (command.name.as_str(), command.kind))
            .collect::<Vec<_>>(),
        vec![("review", AgentCommandKind::NativeSlash)]
    );

    let error = state
        .resolve_agent_command(
            &created.session_id,
            "review-local",
            ResolveAgentCommandRequest {
                arguments: Some("staged".to_owned()),
                note: None,
                intent: AgentCommandResolveIntent::Delegate,
            },
        )
        .unwrap_err();

    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert_eq!(error.message, "agent command not found");
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

#[tokio::test]
async fn agent_command_resolve_route_json_rejection_uses_endpoint_label() {
    let state = test_app_state();
    let session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());

    let (status, response): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{session_id}/agent-commands/review-local/resolve"
            ))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"arguments":"unterminated"#))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        response["error"]
            .as_str()
            .expect("error response should include a message")
            .contains("invalid agent command resolve request JSON")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

// Pins backend-owned prompt-template resolution to replacing `$ARGUMENTS`
// and appending the optional user note as a standard block. This keeps
// regular sends and delegations from drifting back to frontend-only template
// expansion.
#[test]
fn resolves_prompt_template_arguments_and_note() {
    let root =
        std::env::temp_dir().join(format!("termal-agent-command-resolve-{}", Uuid::new_v4()));
    let _cleanup = TempDirCleanup::new(root.clone());
    let commands_dir = root.join(".claude").join("commands");
    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("fix-bug.md"),
        "---
name: fix-bug
description: Fix the requested bug.
metadata:
  termal:
    title:
      strategy: prefixFirstArgument
      prefix: Fix bug
---

Fix the requested bug:

$ARGUMENTS

Verify the fix.
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
}

#[test]
fn rejects_oversized_agent_command_arguments_and_note() {
    let cases = [
        (
            "arguments",
            Some("x".repeat(MAX_AGENT_COMMAND_ARGUMENTS_BYTES + 1)),
            None,
            "agent command arguments must be at most 65536 bytes",
        ),
        (
            "note",
            Some("bug 1024".to_owned()),
            Some("x".repeat(MAX_AGENT_COMMAND_NOTE_BYTES + 1)),
            "agent command note must be at most 65536 bytes",
        ),
    ];

    for (case_name, arguments, note, expected_message) in cases {
        let root = std::env::temp_dir().join(format!(
            "termal-agent-command-oversized-{case_name}-{}",
            Uuid::new_v4()
        ));
        let _cleanup = TempDirCleanup::new(root.clone());
        let commands_dir = root.join(".claude").join("commands");
        fs::create_dir_all(&commands_dir).unwrap();
        fs::write(
            commands_dir.join("fix-bug.md"),
            "Fix the requested bug:\n\n$ARGUMENTS\n",
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

        let error = state
            .resolve_agent_command(
                &created.session_id,
                "fix-bug",
                ResolveAgentCommandRequest {
                    arguments,
                    note,
                    intent: AgentCommandResolveIntent::Send,
                },
            )
            .unwrap_err();

        assert_eq!(error.status, StatusCode::BAD_REQUEST, "{case_name}");
        assert_eq!(error.message, expected_message, "{case_name}");
    }
}

// Pins project-local command metadata to title generation only. React should
// not hard-code the isolated-worktree special case, and repository-owned
// frontmatter must not grant trusted delegation defaults by itself.
#[test]
fn project_local_review_local_metadata_does_not_grant_delegation_defaults() {
    let root = std::env::temp_dir().join(format!(
        "termal-agent-command-resolve-review-local-{}",
        Uuid::new_v4()
    ));
    let _cleanup = TempDirCleanup::new(root.clone());
    let commands_dir = root.join(".claude").join("commands");
    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("review-local.md"),
        "---
name: review-local
description: Review staged and unstaged changes.
metadata:
  termal:
    title:
      strategy: default
    delegation:
      enabled: true
      mode: reviewer
      writePolicy:
        kind: isolatedWorktree
---

Review staged and unstaged changes.
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
        response.title.as_deref(),
        Some("Review staged and unstaged changes.")
    );
    assert_eq!(
        response.expanded_prompt.as_deref(),
        Some("Review staged and unstaged changes.\n")
    );
    assert_eq!(response.delegation, None);
}

#[test]
fn rejects_invalid_agent_command_delegation_metadata() {
    let cases = [
        (
            "worker-mode",
            "metadata:
  termal:
    delegation:
      enabled: true
      mode: worker
      writePolicy:
        kind: readOnly",
            "metadata.termal.delegation.mode `worker` is not supported yet",
        ),
        (
            "invalid-mode",
            "metadata:
  termal:
    delegation:
      enabled: true
      mode: invalid_value
      writePolicy:
        kind: readOnly",
            "unsupported metadata.termal.delegation.mode `invalid_value`",
        ),
        (
            "shared-worktree",
            "metadata:
  termal:
    delegation:
      enabled: true
      mode: reviewer
      writePolicy:
        kind: sharedWorktree",
            "metadata.termal.delegation.writePolicy.kind `sharedWorktree` is not supported yet",
        ),
        (
            "bogus-write-policy",
            "metadata:
  termal:
    delegation:
      enabled: true
      mode: reviewer
      writePolicy:
        kind: bogus",
            "unsupported metadata.termal.delegation.writePolicy.kind `bogus`",
        ),
        (
            "invalid-enabled",
            "metadata:
  termal:
    delegation:
      enabled: sometimes
      mode: reviewer
      writePolicy:
        kind: readOnly",
            "unsupported metadata.termal.delegation.enabled value `sometimes`",
        ),
        (
            "enabled-without-mode",
            "metadata:
  termal:
    delegation:
      enabled: true
      writePolicy:
        kind: readOnly",
            "delegation metadata requires metadata.termal.delegation.mode",
        ),
        (
            "enabled-without-write-policy",
            "metadata:
  termal:
    delegation:
      enabled: true
      mode: reviewer",
            "delegation metadata requires metadata.termal.delegation.writePolicy.kind",
        ),
        (
            "prefix-without-strategy",
            "metadata:
  termal:
    title:
      prefix: Fix bug",
            "metadata.termal.title.prefix requires metadata.termal.title.strategy prefixFirstArgument",
        ),
        (
            "bogus-title-strategy",
            "metadata:
  termal:
    title:
      strategy: bogus",
            "unsupported metadata.termal.title.strategy `bogus`",
        ),
    ];

    for (case_name, frontmatter, expected_message) in cases {
        let error = parse_agent_command_resolver_metadata(frontmatter, true).unwrap_err();

        assert_eq!(error.status, StatusCode::BAD_REQUEST, "{case_name}");
        assert_eq!(error.message, expected_message, "{case_name}");
    }
}

#[test]
fn project_local_invalid_delegation_metadata_does_not_block_resolution() {
    let cases = [
        (
            "unsupported-values",
            "metadata:
  termal:
    delegation:
      enabled: true
      mode: worker
      writePolicy:
        kind: sharedWorktree",
        ),
        (
            "misquoted-mode",
            "metadata:
  termal:
    delegation:
      enabled: true
      mode: 'reviewer' stale'
      writePolicy:
        kind: readOnly",
        ),
        (
            "tab-indented-delegation",
            "metadata:
  termal:
    delegation:
\t  enabled: true",
        ),
    ];

    for (case_name, frontmatter) in cases {
        let root = std::env::temp_dir().join(format!(
            "termal-agent-command-untrusted-invalid-metadata-{case_name}-{}",
            Uuid::new_v4()
        ));
        let _cleanup = TempDirCleanup::new(root.clone());
        let commands_dir = root.join(".claude").join("commands");
        fs::create_dir_all(&commands_dir).unwrap();
        fs::write(
            commands_dir.join("review-local.md"),
            format!(
                "---
name: review-local
description: Frontmatter review title.
{frontmatter}
---

Body fallback should not win.
"
            ),
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

        assert_eq!(response.visible_prompt, "/review-local", "{case_name}");
        assert_eq!(
            response.title.as_deref(),
            Some("Frontmatter review title."),
            "{case_name}"
        );
        assert_eq!(
            response.expanded_prompt.as_deref(),
            Some("Body fallback should not win.\n"),
            "{case_name}"
        );
        assert_eq!(response.delegation, None, "{case_name}");
    }
}

#[test]
fn rejects_partial_agent_command_termal_metadata() {
    let cases = [
        (
            "title-prefix-without-strategy",
            "metadata:
  termal:
    title:
      prefix: Fix bug",
            "metadata.termal.title.prefix requires metadata.termal.title.strategy prefixFirstArgument",
        ),
        (
            "disabled-delegation-invalid-mode",
            "metadata:
  termal:
    delegation:
      enabled: false
      mode: bogus",
            "unsupported metadata.termal.delegation.mode `bogus`",
        ),
        (
            "empty-termal-block",
            "metadata:
  termal:",
            "metadata.termal must define title or delegation metadata",
        ),
        (
            "empty-title-block",
            "metadata:
  termal:
    title:",
            "metadata.termal.title must define strategy metadata",
        ),
        (
            "empty-delegation-block",
            "metadata:
  termal:
    delegation:",
            "metadata.termal.delegation must define enabled metadata",
        ),
        (
            "delegation-without-enabled",
            "metadata:
  termal:
    delegation:
      mode: reviewer",
            "delegation metadata requires metadata.termal.delegation.enabled",
        ),
        (
            "tab-indented-termal-metadata",
            "metadata:
\ttermal:
\t  delegation:
\t    enabled: true",
            "agent command frontmatter must be space-indented",
        ),
        (
            "misquoted-delegation-mode",
            "metadata:
  termal:
    delegation:
      enabled: true
      mode: 'reviewer' stale'
      writePolicy:
        kind: readOnly",
            "invalid quoted frontmatter value for metadata.termal.delegation.mode",
        ),
    ];

    for (case_name, frontmatter, expected_message) in cases {
        let error = parse_agent_command_resolver_metadata(frontmatter, true).unwrap_err();

        assert_eq!(error.status, StatusCode::BAD_REQUEST, "{case_name}");
        assert_eq!(error.message, expected_message, "{case_name}");
    }
}

#[test]
fn resolves_claude_only_tab_indented_frontmatter_without_termal_metadata() {
    let root = std::env::temp_dir().join(format!(
        "termal-agent-command-claude-tab-frontmatter-{}",
        Uuid::new_v4()
    ));
    let _cleanup = TempDirCleanup::new(root.clone());
    let commands_dir = root.join(".claude").join("commands");
    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("tool-check.md"),
        "---
description: Tool check
tools:
\t- Bash
---

Run tool check for $ARGUMENTS.
",
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

    let response = state
        .resolve_agent_command(
            &created.session_id,
            "tool-check",
            ResolveAgentCommandRequest {
                arguments: Some("repo".to_owned()),
                note: None,
                intent: AgentCommandResolveIntent::Send,
            },
        )
        .unwrap();

    assert_eq!(response.visible_prompt, "/tool-check repo");
    assert_eq!(
        response.expanded_prompt.as_deref(),
        Some("Run tool check for repo.\n")
    );
    assert_eq!(response.delegation, None);
}

#[test]
fn resolves_claude_only_large_frontmatter_without_termal_metadata() {
    let root = std::env::temp_dir().join(format!(
        "termal-agent-command-large-claude-frontmatter-{}",
        Uuid::new_v4()
    ));
    let _cleanup = TempDirCleanup::new(root.clone());
    let commands_dir = root.join(".claude").join("commands");
    fs::create_dir_all(&commands_dir).unwrap();
    let large_tools_value = "x".repeat(70 * 1024);
    fs::write(
        commands_dir.join("tool-check.md"),
        format!(
            "---
tools: {large_tools_value}
---

Run tool check for $ARGUMENTS.
"
        ),
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

    let response = state
        .resolve_agent_command(
            &created.session_id,
            "tool-check",
            ResolveAgentCommandRequest {
                arguments: Some("repo".to_owned()),
                note: None,
                intent: AgentCommandResolveIntent::Send,
            },
        )
        .unwrap();

    assert_eq!(
        response.expanded_prompt.as_deref(),
        Some("Run tool check for repo.\n")
    );
    assert_eq!(response.delegation, None);
}

#[test]
fn resolves_termal_metadata_while_ignoring_unrelated_frontmatter_errors() {
    let root = std::env::temp_dir().join(format!(
        "termal-agent-command-mixed-frontmatter-{}",
        Uuid::new_v4()
    ));
    let _cleanup = TempDirCleanup::new(root.clone());
    let commands_dir = root.join(".claude").join("commands");
    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("review-local.md"),
        "---
description: 'Review local' stale'
tools:
\tfoo: bar
metadata:
  termal:
    delegation:
      enabled: true
      mode: reviewer
      writePolicy:
        kind: readOnly
---

Review staged and unstaged changes.
",
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
    assert_eq!(response.title.as_deref(), Some("Review local' stale"));
    assert_eq!(response.delegation, None);
}

#[test]
fn resolves_dotted_termal_metadata_frontmatter() {
    let root = std::env::temp_dir().join(format!(
        "termal-agent-command-dotted-frontmatter-{}",
        Uuid::new_v4()
    ));
    let _cleanup = TempDirCleanup::new(root.clone());
    let commands_dir = root.join(".claude").join("commands");
    fs::create_dir_all(&commands_dir).unwrap();
    fs::write(
        commands_dir.join("review-local.md"),
        "---
metadata.termal.title.strategy: prefixFirstArgument
metadata.termal.title.prefix: Review local
metadata.termal.delegation.enabled: true
metadata.termal.delegation.mode: reviewer
metadata.termal.delegation.writePolicy.kind: readOnly
---

Review $ARGUMENTS.
",
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

    let response = state
        .resolve_agent_command(
            &created.session_id,
            "review-local",
            ResolveAgentCommandRequest {
                arguments: Some("staged changes".to_owned()),
                note: None,
                intent: AgentCommandResolveIntent::Delegate,
            },
        )
        .unwrap();

    assert_eq!(response.visible_prompt, "/review-local staged changes");
    assert_eq!(response.title.as_deref(), Some("Review local staged"));
    assert_eq!(response.delegation, None);
}

#[test]
fn native_delegate_resolution_uses_metadata_name_not_source_suffix() {
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
                name: "audit".to_owned(),
                description: "Audit the current state.".to_owned(),
                content: "/audit".to_owned(),
                source: ".claude/commands/review-local.md".to_owned(),
                argument_hint: None,
                resolver_frontmatter: None,
                resolver_frontmatter_trusted: false,
            }],
        )
        .unwrap();

    let response = state
        .resolve_agent_command(
            &created.session_id,
            "audit",
            ResolveAgentCommandRequest {
                arguments: Some("staged".to_owned()),
                note: None,
                intent: AgentCommandResolveIntent::Delegate,
            },
        )
        .unwrap();

    assert_eq!(response.visible_prompt, "/audit staged");
    assert_eq!(response.expanded_prompt, None);
    assert_eq!(response.delegation, None);
}

#[test]
fn legacy_cached_prompt_template_delegate_resolution_uses_metadata_name_not_source_suffix() {
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
    cache_agent_commands_for_test(
        &state,
        &created.session_id,
        vec![AgentCommand {
            kind: AgentCommandKind::PromptTemplate,
            name: "audit".to_owned(),
            description: "Audit the current state.".to_owned(),
            content: "Audit $ARGUMENTS".to_owned(),
            source: ".claude/commands/review-local.md".to_owned(),
            argument_hint: None,
            resolver_frontmatter: None,
            resolver_frontmatter_trusted: false,
        }],
    );

    let response = state
        .resolve_agent_command(
            &created.session_id,
            "audit",
            ResolveAgentCommandRequest {
                arguments: Some("staged".to_owned()),
                note: None,
                intent: AgentCommandResolveIntent::Delegate,
            },
        )
        .unwrap();

    assert_eq!(response.visible_prompt, "/audit staged");
    assert_eq!(response.expanded_prompt.as_deref(), Some("Audit staged"));
    assert_eq!(response.delegation, None);
}

#[test]
fn cached_prompt_template_missing_metadata_file_resolves_without_defaults() {
    let root = std::env::temp_dir().join(format!(
        "termal-agent-command-missing-metadata-{}",
        Uuid::new_v4()
    ));
    let _cleanup = TempDirCleanup::new(root.clone());
    fs::create_dir_all(&root).unwrap();

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
    cache_agent_commands_for_test(
        &state,
        &created.session_id,
        vec![AgentCommand {
            kind: AgentCommandKind::PromptTemplate,
            name: "review-local".to_owned(),
            description: "Cached prompt template.".to_owned(),
            content: "Cached review $ARGUMENTS".to_owned(),
            source: ".claude/commands/review-local.md".to_owned(),
            argument_hint: None,
            resolver_frontmatter: None,
            resolver_frontmatter_trusted: false,
        }],
    );

    let response = state
        .resolve_agent_command(
            &created.session_id,
            "review-local",
            ResolveAgentCommandRequest {
                arguments: Some("staged".to_owned()),
                note: None,
                intent: AgentCommandResolveIntent::Delegate,
            },
        )
        .unwrap();

    assert_eq!(response.visible_prompt, "/review-local staged");
    assert_eq!(
        response.expanded_prompt.as_deref(),
        Some("Cached review staged")
    );
    assert_eq!(response.delegation, None);
}

#[test]
fn resolver_metadata_uses_cached_frontmatter_snapshot_without_disk_reread() {
    let command = AgentCommand {
        kind: AgentCommandKind::PromptTemplate,
        name: "review-local".to_owned(),
        description: "Review cached frontmatter.".to_owned(),
        content: "Review $ARGUMENTS".to_owned(),
        source: ".claude/commands/review-local.md".to_owned(),
        argument_hint: None,
        resolver_frontmatter: Some(
            "metadata:
  termal:
    delegation:
      enabled: true
      mode: reviewer
      writePolicy:
        kind: isolatedWorktree
"
            .to_owned(),
        ),
        resolver_frontmatter_trusted: true,
    };

    let metadata = read_agent_command_resolver_metadata(FsPath::new("C:/does/not/exist"), &command)
        .unwrap()
        .expect("cached frontmatter should produce resolver metadata");

    let response = resolve_agent_command_payload(
        command,
        ResolveAgentCommandRequest {
            arguments: Some("staged".to_owned()),
            note: None,
            intent: AgentCommandResolveIntent::Delegate,
        },
        Some(metadata),
    )
    .unwrap();

    assert_eq!(response.visible_prompt, "/review-local staged");
    assert_eq!(
        response.delegation,
        Some(ResolvedAgentCommandDelegationDefaults {
            mode: Some(DelegationMode::Reviewer),
            title: Some("Review cached frontmatter.".to_owned()),
            write_policy: Some(DelegationWritePolicy::IsolatedWorktree {
                owned_paths: Vec::new(),
                worktree_path: None,
            }),
        })
    );
}

#[test]
fn native_delegate_resolution_does_not_use_prompt_template_metadata_by_name() {
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
                name: "review-local".to_owned(),
                description: "Runtime-provided review command.".to_owned(),
                content: "/review-local".to_owned(),
                source: "claude/native".to_owned(),
                argument_hint: None,
                resolver_frontmatter: None,
                resolver_frontmatter_trusted: false,
            }],
        )
        .unwrap();

    let response = state
        .resolve_agent_command(
            &created.session_id,
            "review-local",
            ResolveAgentCommandRequest {
                arguments: Some("staged".to_owned()),
                note: None,
                intent: AgentCommandResolveIntent::Delegate,
            },
        )
        .unwrap();

    assert_eq!(response.visible_prompt, "/review-local staged");
    assert_eq!(response.expanded_prompt, None);
    assert_eq!(response.delegation, None);
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
                resolver_frontmatter: None,
                resolver_frontmatter_trusted: false,
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
