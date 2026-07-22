//! Delegation outcome recovery tests split out of `src/tests/delegations.rs`.
//!
//! This module owns result-packet recovery and stale-result classification
//! coverage for child delegation refreshes. It deliberately does not own
//! general delegation lifecycle, queue, wait, or orchestration tests; those
//! remain in focused sibling modules or `delegations.rs`.

use super::delegation_support::{
    finish_delegation_child_with_assistant_text,
    test_app_state_with_drained_delegation_codex_runtime,
};
use super::*;

fn test_app_state() -> AppState {
    test_app_state_with_drained_delegation_codex_runtime("delegation-outcome-recovery-runtime")
}

#[test]
fn delegation_idle_child_without_assistant_output_fails() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Return a result packet.".to_owned(),
                title: Some("Packet Required".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child_index = inner
            .find_session_index(&created.delegation.child_session_id)
            .expect("child session should exist");
        let child = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");
        child.session.status = SessionStatus::Idle;
        child.session.preview.clear();
        state
            .commit_locked(&mut inner)
            .expect("idle child update should persist");
    }
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete");
    let response = state
        .get_delegation_result(&parent_session_id, &created.delegation.id)
        .expect("failed delegation should expose a result");

    assert_eq!(response.result.status, DelegationStatus::Failed);
    assert_eq!(
        response.result.summary,
        "child finished without a result packet"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_idle_child_without_result_packet_preserves_final_assistant_output() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Return a normal review.".to_owned(),
                title: Some("Plain Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let review = "### Sync Review\n\n\
**Findings:**\n\
- **[Note]** `lib/database/sync/sync_engine.dart:3506` \u{2014} Verified the cross-user row remains pending.\n\n\
**Summary:** SYNC15 and SYNC20 fixes were reviewed successfully.";

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        review,
    );
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete");
    let response = state
        .get_delegation_result(&parent_session_id, &created.delegation.id)
        .expect("completed delegation should expose synthesized result");

    assert_eq!(response.result.status, DelegationStatus::Completed);
    assert_eq!(response.result.summary, review);
    assert_eq!(
        response.result.findings,
        vec![DelegationFinding {
            severity: "Note".to_owned(),
            file: Some("lib/database/sync/sync_engine.dart".to_owned()),
            line: Some(3506),
            message: "Verified the cross-user row remains pending.".to_owned(),
        }]
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_idle_child_error_like_plain_output_is_completed() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Return plain output.".to_owned(),
                title: Some("Plain Output".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let output = "I could not finish the review because the required files were unavailable.";

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        output,
    );
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete");
    let response = state
        .get_delegation_result(&parent_session_id, &created.delegation.id)
        .expect("completed delegation should expose synthesized result");

    assert_eq!(response.result.status, DelegationStatus::Completed);
    assert_eq!(response.result.summary, output);
    assert!(response.result.findings.is_empty());

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_plain_output_synthesis_ignores_stop_marker_and_bounds_summary() {
    assert!(
        synthesize_delegation_result_from_assistant_output(SESSION_STOPPED_BY_USER_MESSAGE)
            .is_none()
    );

    let long_review = format!(
        "{}\n\n**Findings:**\n- **[Low]** `src/lib.rs:12` - Example finding.",
        "x".repeat(MAX_DELEGATION_RESULT_SUMMARY_CHARS + 128),
    );
    let result = synthesize_delegation_result_from_assistant_output(&long_review)
        .expect("long non-packet output should synthesize");

    assert_eq!(result.status, DelegationStatus::Completed);
    assert!(result.summary.ends_with("..."));
    assert!(
        result.summary.chars().count() <= MAX_DELEGATION_RESULT_SUMMARY_CHARS + 3,
        "summary should be capped with the truncation marker"
    );
    assert_eq!(
        result.findings,
        vec![DelegationFinding {
            severity: "Low".to_owned(),
            file: Some("src/lib.rs".to_owned()),
            line: Some(12),
            message: "Example finding.".to_owned(),
        }]
    );
}

#[test]
fn delegation_review_findings_parser_scopes_and_deduplicates_sections() {
    let findings = parse_delegation_review_findings(
        "## Informational\n\
- **[High]** `src/ignored.rs:1` - Informational bullets must not surface.\n\
\n\
## Actionable\n\
- **[High]** `src/lib.rs:10` - Real issue.\n\
  - Why it matters: this continuation should be skipped.\n\
\n\
**Findings:**\n\
- **[High]** `src/lib.rs:10` - Real issue.\n\
- **[Medium]** `src/second.rs:42` - Second issue.\n\
\n\
# Reviewer Summaries\n\
- **[Low]** `src/ignored.rs:2` - Summary bullets must not surface.\n\
\n\
## Notes\n\
- **[Low]** `src/ignored.rs:3` - Notes must not surface.\n",
    );

    assert_eq!(
        findings,
        vec![
            DelegationFinding {
                severity: "High".to_owned(),
                file: Some("src/lib.rs".to_owned()),
                line: Some(10),
                message: "Real issue.".to_owned(),
            },
            DelegationFinding {
                severity: "Medium".to_owned(),
                file: Some("src/second.rs".to_owned()),
                line: Some(42),
                message: "Second issue.".to_owned(),
            },
        ]
    );
}

#[test]
fn delegation_review_findings_parser_caps_finding_count() {
    let review = format!(
        "## Findings\n{}",
        (0..MAX_DELEGATION_RESULT_FINDINGS + 2)
            .map(|index| format!("- **[Low]** `src/file_{index}.rs:1` - Finding {index}."))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let findings = parse_delegation_review_findings(&review);

    assert_eq!(findings.len(), MAX_DELEGATION_RESULT_FINDINGS);
    let expected_last_message = format!("Finding {}.", MAX_DELEGATION_RESULT_FINDINGS - 1);
    assert_eq!(
        findings.last().map(|finding| finding.message.as_str()),
        Some(expected_last_message.as_str())
    );
}

#[test]
fn delegation_result_packet_summary_is_bounded() {
    let result = parse_delegation_result_packet(&format!(
        "## Result\n\nStatus: completed\n\nSummary:\n{}",
        "x".repeat(MAX_DELEGATION_RESULT_SUMMARY_CHARS + 128),
    ))
    .expect("result packet should parse");

    assert!(result.summary.ends_with("..."));
    assert!(
        result.summary.chars().count() <= MAX_DELEGATION_RESULT_SUMMARY_CHARS + 3,
        "packet summary should be capped with the truncation marker"
    );
}

#[test]
fn delegation_stopped_child_without_result_packet_does_not_complete() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review until stopped.".to_owned(),
                title: Some("Stopped Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    state
        .stop_session(&created.delegation.child_session_id)
        .expect("stop should succeed");
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete");
    let response = state
        .get_delegation_result(&parent_session_id, &created.delegation.id)
        .expect("failed delegation should expose a result");

    assert_eq!(response.result.status, DelegationStatus::Failed);
    assert_eq!(
        response.result.summary,
        "child finished without a result packet"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_stopped_child_after_partial_review_does_not_complete() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review until stopped.".to_owned(),
                title: Some("Stopped Partial Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "Partial review without a result packet.",
    );
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child_index = inner
            .find_session_index(&created.delegation.child_session_id)
            .expect("child session should exist");
        let child = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");
        child.session.status = SessionStatus::Active;
        state
            .commit_locked(&mut inner)
            .expect("active child update should persist");
    }

    state
        .stop_session(&created.delegation.child_session_id)
        .expect("stop should succeed");
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete");
    let response = state
        .get_delegation_result(&parent_session_id, &created.delegation.id)
        .expect("failed delegation should expose a result");

    assert_eq!(response.result.status, DelegationStatus::Failed);
    assert_eq!(
        response.result.summary,
        "child finished without a result packet"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_stopped_child_with_file_changes_after_stop_marker_does_not_complete() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review and touch files until stopped.".to_owned(),
                title: Some("Stopped Review With Files".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child_index = inner
            .find_session_index(&created.delegation.child_session_id)
            .expect("child session should exist");
        let child = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");
        child
            .active_turn_file_changes
            .insert("src/lib.rs".to_owned(), WorkspaceFileChangeKind::Modified);
        state
            .commit_locked(&mut inner)
            .expect("file-change setup should persist");
    }

    state
        .stop_session(&created.delegation.child_session_id)
        .expect("stop should succeed");
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete");
    let response = state
        .get_delegation_result(&parent_session_id, &created.delegation.id)
        .expect("failed delegation should expose a result");

    assert_eq!(response.result.status, DelegationStatus::Failed);
    assert_eq!(
        response.result.summary,
        "child finished without a result packet"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_active_child_without_runtime_ignores_plain_output_fallback() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review without a result packet.".to_owned(),
                title: Some("Interrupted Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "I found an issue, but the runtime exited before I could write ## Result.",
    );
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child_index = inner
            .find_session_index(&created.delegation.child_session_id)
            .expect("child session should exist");
        let child = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");
        child.session.status = SessionStatus::Active;
        child.runtime = SessionRuntime::None;
        state
            .commit_locked(&mut inner)
            .expect("active child update should persist");
    }

    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete");
    let response = state
        .get_delegation_result(&parent_session_id, &created.delegation.id)
        .expect("failed delegation should expose a result");

    assert_eq!(response.result.status, DelegationStatus::Failed);
    assert_eq!(
        response.result.summary,
        "Codex session exited before the active turn completed"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_active_remote_proxy_child_with_result_packet_stays_running() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Keep reviewing from a remote runtime.".to_owned(),
                title: Some("Remote Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "Remote child published a partial packet before its runtime proxy settled.\n\n\
## Result\n\n\
Status: completed\n\n\
Summary:\n\
This packet must not terminalize a still-running remote child.",
    );
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child_index = inner
            .find_session_index(&created.delegation.child_session_id)
            .expect("child session should exist");
        let child = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");
        child.runtime = SessionRuntime::None;
        child.remote_id = Some("remote-running-reviewer".to_owned());
        child.remote_session_id = Some("remote-session-running-reviewer".to_owned());
        child.session.status = SessionStatus::Active;
        state
            .commit_locked(&mut inner)
            .expect("remote child update should persist");
    }

    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert!(
            matches!(
                delegation_child_outcome(&inner, &created.delegation.child_session_id),
                DelegationChildOutcome::Running
            ),
            "remote proxy child with a local result packet should still be running"
        );
    }

    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete");
    let response = state
        .get_delegation(&parent_session_id, &created.delegation.id)
        .expect("running delegation should remain queryable");
    assert_eq!(response.delegation.status, DelegationStatus::Running);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_error_child_with_result_packet_recovers_completed_result() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review and return a result packet.".to_owned(),
                title: Some("Recovered Packet".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "All consistent. Let me produce the final consolidated review.\n\n\
## Result\n\n\
Status: completed\n\n\
Summary:\n\
The review completed successfully.",
    );
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child_index = inner
            .find_session_index(&created.delegation.child_session_id)
            .expect("child session should exist");
        let child = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");
        child.session.status = SessionStatus::Error;
        child.session.preview = "child finished without a result packet".to_owned();
        state
            .commit_locked(&mut inner)
            .expect("errored child update should persist");
    }

    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete");
    let response = state
        .get_delegation_result(&parent_session_id, &created.delegation.id)
        .expect("completed delegation should expose a result");

    assert_eq!(response.result.status, DelegationStatus::Completed);
    assert_eq!(
        response.result.summary,
        "The review completed successfully."
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_error_child_ignores_stale_result_packet() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review and return a result packet.".to_owned(),
                title: Some("Stale Packet".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "Intermediate review packet.\n\n\
## Result\n\n\
Status: completed\n\n\
Summary:\n\
The earlier packet should be considered stale.",
    );
    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "runtime failed after the result packet",
    );
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child_index = inner
            .find_session_index(&created.delegation.child_session_id)
            .expect("child session should exist");
        let child = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");
        child.session.status = SessionStatus::Error;
        child.session.preview = "runtime failed after the result packet".to_owned();
        state
            .commit_locked(&mut inner)
            .expect("errored child update should persist");
    }

    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete");
    let response = state
        .get_delegation_result(&parent_session_id, &created.delegation.id)
        .expect("failed delegation should expose a result");

    assert_eq!(response.result.status, DelegationStatus::Failed);
    assert_eq!(
        response.result.summary,
        "runtime failed after the result packet"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_completed_child_recovers_actionable_findings_when_result_packet_defers() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review and return a result packet.".to_owned(),
                title: Some("Deferred Packet Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "# Code Review\n\n\
## Changes Reviewed\n- `src/state.rs`\n\n\
## Actionable\n\
- **[Medium]** `src/state.rs:66-109` \u{2014} State mutex waits behind the bounded mailbox.\n\
  - Why it matters: unrelated requests can stall.\n\n\
## Informational\n- No other issues found.\n\n\
## Result\n\n\
Status: completed\n\n\
Findings:\n\
- Note - See the Actionable and Informational sections above; the headline finding is the state-mutex coupling.\n",
    );
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete");
    let response = state
        .get_delegation_result(&parent_session_id, &created.delegation.id)
        .expect("completed delegation should expose a result");

    assert_eq!(response.result.status, DelegationStatus::Completed);
    assert_eq!(
        response.result.findings,
        vec![DelegationFinding {
            severity: "Medium".to_owned(),
            file: Some("src/state.rs".to_owned()),
            line: Some(66),
            message: "State mutex waits behind the bounded mailbox.".to_owned(),
        }]
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}
