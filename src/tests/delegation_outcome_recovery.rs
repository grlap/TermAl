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
fn delegation_idle_child_without_result_packet_fails() {
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

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "Plain assistant response without the required packet.",
    );
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
                agent: Some(Agent::Claude),
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
                agent: Some(Agent::Claude),
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
                agent: Some(Agent::Claude),
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
