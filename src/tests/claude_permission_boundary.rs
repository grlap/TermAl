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
