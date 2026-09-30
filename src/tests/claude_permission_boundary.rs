// Owns the read-only Claude spawn policy and its host permission seam.
// Does not emulate Claude's permission-rule evaluator. Companion CLI protocol
// diagnostics must verify the upstream ask-before-allow contract separately.
// Split from the CLI/permission coverage in session_lifecycle.rs and claude.rs.
use super::*;

#[test]
fn claude_read_only_spawn_overrides_inherited_permission_shortcuts() {
    for resume in [None, Some("existing-session")] {
        let args = claude_cli_persistent_args(
            "opus",
            ClaudeApprovalMode::ReadOnlyAutoApprove,
            ClaudeEffortLevel::Default,
            resume,
        );
        assert!(
            args.windows(2)
                .any(|p| p == ["--permission-mode", "default"])
        );
        assert!(
            args.windows(2)
                .any(|p| p == ["--setting-sources", "user,project,local"])
        );
        let settings = args
            .windows(2)
            .find(|p| p[0] == "--settings")
            .expect("read-only spawn must override permission settings");
        let settings: Value = serde_json::from_str(&settings[1]).unwrap();
        assert_eq!(settings["permissions"]["ask"], json!(["*"]));
        assert_eq!(settings["sandbox"]["autoAllowBashIfSandboxed"], false);
        assert_eq!(settings["disableAllHooks"], true);
    }
    for mode in [
        ClaudeApprovalMode::Ask,
        ClaudeApprovalMode::AutoApprove,
        ClaudeApprovalMode::Plan,
    ] {
        let args = claude_cli_persistent_args("opus", mode, ClaudeEffortLevel::Default, None);
        assert!(
            !args.iter().any(|a| a == "--settings"),
            "ordinary session settings stay intact"
        );
    }
}

#[test]
fn claude_read_only_host_checks_commands_even_if_cli_settings_allow_them() {
    // These match common project/user allow rules. Once the spawn-time ask
    // override routes them here, only the host classifier decides admission.
    for command in [
        "node -e \"process.exit(0)\"",
        "cargo check",
        "git add .",
        "git stash",
        "git revert HEAD",
    ] {
        assert_permission("Bash", json!({"command":command}), false, false);
    }
    assert_permission("Bash", json!({"command":"git status --short"}), false, true);
    assert_permission("Read", json!({"file_path":"src/main.rs"}), false, true);
    assert_permission(
        "ToolSearch",
        json!({"query":"select:mcp__termal-delegation__termal_submit_review_result"}),
        false,
        true,
    );
    for tool in [
        "PowerShell",
        "Write",
        "Edit",
        "NotebookEdit",
        "Agent",
        "Task",
        "mcp__other__write",
    ] {
        assert_permission(tool, json!({}), false, false);
    }
    let submit = "mcp__termal-delegation__termal_submit_review_result";
    assert_permission(submit, json!({"schemaVersion":1}), false, false);
    assert_permission(submit, json!({"schemaVersion":1}), true, true);
    let freeze = "mcp__termal-delegation__termal_review_freeze_check";
    assert_permission(freeze, json!({}), false, false);
    assert_permission(freeze, json!({}), true, true);
    for command in [
        "node scripts/review-freeze-fingerprint.mjs --check .git/freeze.json",
        "node -e 'require(\"fs\").writeFileSync(\"sentinel\",\"bad\")'",
        "git add .",
    ] {
        assert_permission("Bash", json!({"command":command}), true, false);
    }
}

#[test]
fn claude_read_only_allows_skill_loading_without_granting_write_permissions() {
    let mut turn_state = ClaudeTurnState::default();
    for (index, (tool, input, allowed)) in [
        ("Skill", json!({"skill":"review-code"}), true),
        (
            "Skill",
            json!({"skill":"project:custom", "args":"src"}),
            true,
        ),
        (
            "Read",
            json!({"file_path":".claude/commands/review-code.md"}),
            true,
        ),
        (
            "Edit",
            json!({"file_path":"src/main.rs", "old_string":"a", "new_string":"b"}),
            false,
        ),
        (
            "Write",
            json!({"file_path":"sentinel", "content":"no"}),
            false,
        ),
        ("Bash", json!({"command":"git add ."}), false),
        ("Bash", json!({"command":"cargo test"}), false),
        ("Agent", json!({"prompt":"write a file"}), false),
    ]
    .into_iter()
    .enumerate()
    {
        let action = classify_claude_control_request(
            &json!({"type":"control_request", "request_id":format!("skill-boundary-{index}"),
                "request":{"subtype":"can_use_tool", "tool_name":tool, "input":input}}),
            &mut turn_state,
            ClaudeApprovalMode::ReadOnlyAutoApprove,
            true,
            ".",
            false,
        )
        .unwrap()
        .expect("skill and subsequent tool requests must reach the classifier");
        assert_eq!(
            matches!(
                action,
                ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow { .. })
            ),
            allowed,
            "{tool}"
        );
    }
}

/// The decision for one tracker tool call of a read-only child, and the
/// denial's text when it is refused.
fn tracker_decision(
    tool: &str,
    input: Value,
    tracker_reads: bool,
) -> std::result::Result<(), String> {
    let action = classify_claude_control_request(
        &json!({"type":"control_request", "request_id":"tracker-boundary",
            "request":{"subtype":"can_use_tool", "tool_name":tool, "input":input}}),
        &mut ClaudeTurnState::default(),
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        true,
        ".",
        ClaudeHostAdmission {
            control_plane: false,
            tracker_reads,
        },
    )
    .unwrap()
    .expect("permission request should reach the host classifier");
    match action {
        ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow { .. }) => Ok(()),
        ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Deny { message, .. }) => {
            Err(message)
        }
        _ => panic!("a read-only child's request is answered at once"),
    }
}

// A project's instructions make every session read the tracker's orientation
// and memories before it acts. A read-only child may do exactly that.
#[test]
fn claude_read_only_child_may_make_the_tracker_reads_that_record_nothing() {
    for (word, input) in [
        ("memories", json!({})),
        ("memories", Value::Null),
        ("memories", json!({"query":"landing-train", "full":true})),
        ("memories", json!({"after":"greg", "revision":2})),
        ("next", json!({"peek":true})),
        ("next", json!({"peek":true, "limit":5, "verbose":false})),
        ("ls", json!({})),
        ("ls", json!({"ready":true, "limit":20, "after":"cursor"})),
        ("search", json!({"query":"read-only", "limit":10})),
        ("show", json!({"work_ref":"w-task"})),
        (
            "show",
            json!({"work_ref":"w-task", "notes":true, "gates":true}),
        ),
        ("show", json!({"work_ref":"w-task", "note":"0123abcd"})),
        ("show", json!({"work_ref":"w-task", "full":true})),
    ] {
        let tool = format!("mcp__engram__{word}");
        assert_eq!(
            tracker_decision(&tool, input.clone(), true),
            Ok(()),
            "{tool} {input}"
        );
        // Not for a child the host does not admit: an evaluator, or a runtime
        // the host gave no tracker server.
        let refused = tracker_decision(&tool, input.clone(), false).unwrap_err();
        assert!(
            refused.contains("this Claude reviewer delegation is read-only"),
            "{tool} {input}: {refused}"
        );
    }
}

#[test]
fn claude_read_only_child_is_refused_every_tracker_call_that_writes() {
    let mut refused = Vec::new();
    // Every word that writes, whatever it carries.
    for word in [
        "add", "claim", "update", "note", "done", "gate", "evaluate", "handoff", "remember",
        "forget",
    ] {
        refused.push((word, json!({})));
        refused.push((word, json!({"work_ref":"w-task", "text":"x"})));
    }
    refused.extend([
        // `next` without a peek stages delivery and acknowledges memories.
        ("next", json!({})),
        ("next", Value::Null),
        ("next", json!({"peek":false})),
        ("next", json!({"peek":"true"})),
        ("next", json!({"peek":null})),
        // A context generation is the host's to assert, and on `memories` it
        // records that the caller listed them.
        (
            "next",
            json!({"peek":true, "context_generation":"termal-1"}),
        ),
        ("memories", json!({"context_generation":"termal-1"})),
        // An argument the list does not name is not judged, so it is refused.
        ("show", json!({"work_ref":"w-task", "acknowledge":true})),
        ("ls", json!({"claim":true})),
        ("search", json!({"query":"x", "save":true})),
        // Not an argument object.
        ("show", json!("w-task")),
        ("memories", json!(["x"])),
        // Not a word of the tracker's MCP surface.
        ("core", json!({})),
        ("", json!({})),
    ]);
    for (word, input) in refused {
        let tool = format!("mcp__engram__{word}");
        let message = tracker_decision(&tool, input.clone(), true).unwrap_err();
        if word.is_empty() {
            // `mcp__engram__` names no tool of the tracker's server.
            assert!(
                message.contains("reviewer delegation is read-only"),
                "{message}"
            );
            continue;
        }
        // The denial says which tracker calls are reads, so the child adapts.
        assert_eq!(message, CLAUDE_READ_ONLY_TRACKER_DENIAL, "{tool} {input}");
    }
    assert!(CLAUDE_READ_ONLY_TRACKER_DENIAL.contains("`next` with `peek: true`"));
}

// The qualified name is the only identity a permission request carries: a
// tool of another server, or a bare leaf name, is never read as the tracker's.
#[test]
fn claude_read_only_tracker_reads_match_the_tracker_servers_qualified_name_only() {
    for tool in [
        "memories",
        "mcp__memories",
        "mcp__other__memories",
        "mcp__engram_x__memories",
        "mcp__engramx__memories",
        "mcp__Engram__memories",
        "mcp__termal-delegation__memories",
    ] {
        assert!(tracker_decision(tool, json!({}), true).is_err(), "{tool}");
        assert_eq!(engram_mcp_tool_word(tool), None, "{tool}");
    }
    assert_eq!(
        engram_mcp_tool_word("mcp__engram__memories"),
        Some("memories")
    );
    // Admitting tracker reads admits nothing else.
    for (tool, input) in [
        ("Write", json!({"file_path":"sentinel", "content":"no"})),
        ("Bash", json!({"command":"git add ."})),
        ("mcp__other__write", json!({})),
    ] {
        assert!(tracker_decision(tool, input, true).is_err(), "{tool}");
    }
    assert_eq!(
        tracker_decision("Read", json!({"file_path":"src/main.rs"}), true),
        Ok(())
    );
}

// Which sessions the host admits: the running child of a delegation that is
// not an evaluator, on a runtime the host itself gave the tracker's server.
#[test]
fn tracker_reads_are_admitted_for_a_running_non_evaluator_child_with_the_hosts_server() {
    let state = test_app_state();
    let parent = test_session_id(&state, Agent::Claude);
    let (delegation_id, child) =
        super::delegation_support::install_required_review_delegation(&state, &parent);
    let admitted = |state: &AppState, session_id: &str| {
        let inner = state.inner.lock().unwrap();
        read_only_child_tracker_reads_allowed_locked(&inner, session_id)
    };
    let message = json!({"type":"control_request", "request_id":"tracker-admission",
        "request":{"subtype":"can_use_tool", "tool_name":"mcp__engram__memories", "input":{}}});

    // The host gave this runtime no tracker server: nothing of that name is its.
    assert!(!admitted(&state, &child));
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&child).unwrap();
        inner.sessions[index].engram_mcp_installed = Some(EngramMcpInstalledDescriptor {
            binary_path: "C:/tools/engram.exe".to_owned(),
            home: "C:/engram-home".to_owned(),
            actor_id: "dev/reviewer".to_owned(),
            actor_context: None,
            store_key: None,
            work_authority_grant: None,
        });
    }
    assert!(admitted(&state, &child));
    assert_eq!(
        state.claude_host_admission(&child, &message),
        ClaudeHostAdmission {
            control_plane: false,
            tracker_reads: true,
        }
    );
    // The parent is no delegation child, and an unknown session is nobody.
    assert!(!admitted(&state, &parent));
    assert!(!admitted(&state, "session-unknown"));

    let set = |change: &dyn Fn(&mut DelegationRecord)| {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_delegation_index(&delegation_id).unwrap();
        change(&mut inner.delegations[index]);
    };
    // An evaluator is briefed to call no tracker tool, and gets none.
    set(&|delegation| delegation.mode = DelegationMode::Evaluator);
    assert!(!admitted(&state, &child));
    assert!(!state.claude_host_admission(&child, &message).tracker_reads);
    set(&|delegation| delegation.mode = DelegationMode::Explorer);
    assert!(admitted(&state, &child));
    // A delegation that has ended admits nothing.
    set(&|delegation| delegation.status = DelegationStatus::Completed);
    assert!(!admitted(&state, &child));
}

fn assert_permission(tool: &str, input: Value, authority: bool, allowed: bool) {
    let action = classify_claude_control_request(
        &json!({"type":"control_request", "request_id":"permission-boundary",
            "request":{"subtype":"can_use_tool", "tool_name":tool, "input":input}}),
        &mut ClaudeTurnState::default(),
        ClaudeApprovalMode::ReadOnlyAutoApprove,
        true,
        ".",
        authority,
    )
    .unwrap()
    .expect("permission request should reach the host classifier");
    assert_eq!(
        matches!(
            action,
            ClaudeControlRequestAction::Respond(ClaudePermissionDecision::Allow { .. })
        ),
        allowed,
        "{tool}"
    );
}
