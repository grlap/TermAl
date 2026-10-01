// Owns the Codex read-only child tracker configuration contract. The tracker
// enforces read-only arguments; Codex permits only those exposed read tools
// without changing its filesystem sandbox. Evaluator exclusion stays in
// evaluator_tool_access.rs; profile routing stays in codex_read_only_profile.rs.
use super::evaluator_tool_access::{delegation_child, tracker_project};
use super::*;

#[test]
fn codex_read_only_tracker_requires_enforced_mode_and_selective_approval() {
    let state = test_app_state();
    let (project, root) = tracker_project(&state, "codex-read-only-tracker");
    let parent = create_test_project_session(&state, Agent::Codex, &project, &root);
    let child = delegation_child(
        &state,
        &parent,
        &project,
        &root,
        Agent::Codex,
        DelegationMode::Reviewer,
    );
    for name in [&child, &parent] {
        let config = state.termal_delegation_mcp_codex_config(name).unwrap();
        let tracker = &config["mcp_servers"]["engram"];
        let args = tracker["args"].as_array().unwrap();
        if name == &child {
            assert!(
                args.contains(&json!("--read-only")),
                "an unrestricted tracker must never receive selective approval: {tracker}"
            );
            assert_eq!(
                tracker["enabled_tools"],
                json!(["next", "ls", "search", "show", "memories"])
            );
            let approvals = tracker["tools"].as_object().unwrap();
            assert_eq!(approvals.len(), 5);
            for word in ["next", "ls", "search", "show", "memories"] {
                assert_eq!(approvals[word], json!({"approval_mode": "approve"}));
            }
            assert!(tracker.get("default_tools_approval_mode").is_none());
        } else {
            assert!(!args.contains(&json!("--read-only")));
            assert!(tracker.get("tools").is_none());
            assert!(tracker.get("enabled_tools").is_none());
        }
    }
    let (sandbox, approval) = {
        let inner = state.inner.lock().unwrap();
        let child = &inner.sessions[inner.find_session_index(&child).unwrap()];
        (child.codex_sandbox_mode, child.codex_approval_policy)
    };
    assert_eq!(sandbox, CodexSandboxMode::ReadOnly);
    assert_eq!(approval, CodexApprovalPolicy::Never);
}

#[test]
fn codex_read_only_tracker_start_and_resume_keep_the_enforced_surface() {
    let home = TestTempRoot::create("codex-read-only-tracker-thread");
    fs::write(home.path().join("config.toml"), "[mcp_servers.engram]\ncommand = \"unrestricted-seed\"\ndefault_tools_approval_mode = \"approve\"\n").unwrap();
    for (method, thread) in [("thread/start", None), ("thread/resume", Some("existing"))] {
        let request = super::shared_codex_thread_setup::shared_codex_setup_request_for_tracker_test(
            home.path(),
            thread,
            true,
            true,
        );
        assert_eq!(request["method"], method);
        assert_eq!(request["params"]["sandbox"], "read-only");
        assert_eq!(request["params"]["approvalPolicy"], "never");
        let tracker = &request["params"]["config"]["mcp_servers"]["engram"];
        assert_ne!(tracker["command"], "unrestricted-seed");
        assert!(
            tracker["args"]
                .as_array()
                .unwrap()
                .contains(&json!("--read-only")),
            "{request}"
        );
        assert_eq!(
            tracker["enabled_tools"],
            json!(["next", "ls", "search", "show", "memories"])
        );
        assert_eq!(tracker["tools"]["next"]["approval_mode"], "approve");
        assert!(tracker.get("default_tools_approval_mode").is_none());
        assert!(tracker["tools"].get("note").is_none());
    }
}
