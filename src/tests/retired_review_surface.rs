// Behavior of legacy callers at the former schema-1 review entry points.
use super::*;
use tower::ServiceExt;

const LEGACY_TOOL: &str = "mcp__termal-delegation__termal_review_freeze_check";
const LEGACY_DESCRIPTION: &str = "Independently verify an Engram schema-1 frozen review using TermAl's compiled checker, not repository scripts. Supply the manifest path and the parent's separately recorded SHA-256 literal. The host derives the current review worktree; observes real checker exit status, stdout and stderr separately; and requires exact fingerprint plus LF. Call before and after inspection. Does not allow Node, shell scripts, writes, builds, or tracker access. A mismatch or unsupported input is a failed verification, never a clean review.";

#[tokio::test]
async fn retired_review_route_is_absent_for_an_active_reviewer() {
    let state = test_app_state();
    let parent = test_session_id(&state, Agent::Codex);
    let (_, child) = super::delegation_support::install_required_review_delegation(&state, &parent);
    let response = app_router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{child}/delegation-review-freeze"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"manifestPath": ".git/freeze.json", "expectedFingerprint": "a".repeat(64)})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(
        axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap()
            .is_empty()
    );
}

#[test]
fn claude_reviewers_keep_submission_but_cannot_use_the_retired_check() {
    let state = test_app_state();
    let parent = test_session_id(&state, Agent::Claude);
    let (delegation, child) =
        super::delegation_support::install_required_review_delegation(&state, &parent);
    for policy in [
        DelegationWritePolicy::ReadOnly,
        DelegationWritePolicy::SharedWorktree {
            owned_paths: vec![],
        },
        DelegationWritePolicy::IsolatedWorktree {
            base: None,
            owned_paths: vec![],
            worktree_path: None,
        },
    ] {
        {
            let mut inner = state.inner.lock().unwrap();
            let record = inner
                .delegations
                .iter_mut()
                .find(|d| d.id == delegation)
                .unwrap();
            record.write_policy = policy;
            assert!(!build_delegation_prompt(record).contains("termal_review_freeze_check"));
        }
        for (tool, expected) in [
            (LEGACY_TOOL, false),
            (TERMAL_SUBMIT_REVIEW_RESULT_QUALIFIED_TOOL_NAME, true),
        ] {
            let message = json!({"type":"control_request","request_id":"review-authority",
                "request":{"subtype":"can_use_tool","tool_name":tool,"input":{}}});
            let access = state.claude_control_plane_request_allowed(&child, &message);
            assert_eq!(access, expected);
            let action = classify_claude_control_request(
                &message,
                &mut ClaudeTurnState::default(),
                ClaudeApprovalMode::ReadOnlyAutoApprove,
                true,
                ".",
                access,
            )
            .unwrap();
            assert_eq!(
                matches!(
                    action,
                    Some(ClaudeControlRequestAction::Respond(
                        ClaudePermissionDecision::Allow { .. }
                    ))
                ),
                expected
            );
        }
    }
}

#[test]
fn codex_does_not_auto_approve_the_retired_review_check() {
    let state = test_app_state();
    let parent = test_session_id(&state, Agent::Codex);
    let (_, child) = super::delegation_support::install_required_review_delegation(&state, &parent);
    let request = json!({"id":"old-approval","params":{
        "threadId":"thread-reviewer","turnId":"turn-reviewer","serverName":TERMAL_DELEGATION_MCP_SERVER_NAME,
        "mode":"form","message":"Approval copy","requestedSchema":{"type":"object","properties":{}},
        "_meta":{"codex_approval_kind":"mcp_tool_call","tool_description":LEGACY_DESCRIPTION,
            "tool_params":{"manifestPath":".git/freeze.json","expectedFingerprint":"a".repeat(64)}}}});
    let (tx, rx) = mpsc::channel();
    assert!(
        !try_auto_respond_delegation_control_plane_request(
            "mcpServer/elicitation/request",
            &request,
            &state,
            &child,
            &tx,
        )
        .unwrap()
    );
    assert!(rx.try_recv().is_err());
}
