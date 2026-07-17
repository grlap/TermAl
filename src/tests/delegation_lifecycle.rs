use super::delegation_support::{
    finish_delegation_child_with_assistant_text, test_app_state_with_delegation_codex_runtime,
};
use super::*;

fn mark_delegation_child_stale_with_result_packet(
    state: &AppState,
    child_session_id: &str,
    session_status: SessionStatus,
    result_status: &str,
    summary: &str,
) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let child_index = inner
        .find_session_index(child_session_id)
        .expect("child session should exist");
    let message_id = inner.next_message_id();
    let child = inner
        .session_mut_by_index(child_index)
        .expect("child session index should be valid");
    push_message_on_record(
        child,
        Message::Text {
            attachments: Vec::new(),
            id: message_id,
            timestamp: stamp_now(),
            author: Author::Assistant,
            text: format!(
                "## Result\nStatus: {result_status}\n\nSummary:\n{summary}\n\nFindings:\n- None\n"
            ),
            expanded_text: None,
            source: None,
        },
    );
    child.runtime = SessionRuntime::None;
    child.session.status = session_status;
    child.session.preview = "Waiting for stale runtime refresh".to_owned();
    state.commit_locked(&mut inner).unwrap();
}

fn queue_delegation_child_prompt(state: &AppState, child_session_id: &str, prompt_id: &str) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let child_index = inner
        .find_session_index(child_session_id)
        .expect("child session should exist");
    let child = inner
        .session_mut_by_index(child_index)
        .expect("child session index should be valid");
    child.queued_prompts.push_back(QueuedPromptRecord {
        source: QueuedPromptSource::User,
        attachments: Vec::new(),
        pending_prompt: PendingPrompt {
            attachments: Vec::new(),
            id: prompt_id.to_owned(),
            timestamp: stamp_now(),
            text: "queued child follow-up".to_owned(),
            expanded_text: None,
            source: None,
        },
    });
    sync_pending_prompts(child);
    state.commit_locked(&mut inner).unwrap();
}

fn assert_delegation_wait_response_serializes_queue_flags(
    response: &DelegationWaitResponse,
    resume_prompt_queued: bool,
    resume_dispatch_requested: bool,
) {
    let value = serde_json::to_value(response).expect("wait response should serialize");
    assert_eq!(value["resumePromptQueued"], resume_prompt_queued);
    assert_eq!(value["resumeDispatchRequested"], resume_dispatch_requested);
    assert!(value.get("resume_prompt_queued").is_none());
    assert!(value.get("resume_dispatch_requested").is_none());
}

#[test]
fn already_terminal_delegation_wait_reports_queued_prompt_without_dispatch_for_busy_parent() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review already-finished work.".to_owned(),
                title: Some("Already Finished Review".to_owned()),
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
        "## Result\n\nStatus: completed\n\nSummary:\nReview finished before the wait was scheduled.",
    );
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("delegation should be marked terminal");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let parent_index = inner
            .find_visible_session_index(&parent_session_id)
            .expect("parent should exist");
        let parent = inner
            .session_mut_by_index(parent_index)
            .expect("parent session index should be valid");
        parent.session.status = SessionStatus::Active;
    }

    let wait = state
        .create_delegation_wait(
            &parent_session_id,
            CreateDelegationWaitRequest {
                delegation_ids: vec![created.delegation.id.clone()],
                mode: DelegationWaitMode::All,
                title: Some("Already terminal fan-in".to_owned()),
            },
        )
        .expect("already-terminal delegation wait should be accepted");

    assert!(
        wait.resume_prompt_queued,
        "resumePromptQueued should mean the parent resume prompt was queued"
    );
    assert!(
        !wait.resume_dispatch_requested,
        "active parents should keep the queued prompt pending"
    );
    assert_delegation_wait_response_serializes_queue_flags(&wait, true, false);

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.delegation_waits.is_empty());
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should exist");
    assert_eq!(parent.queued_prompts.len(), 1);
    assert!(
        parent.queued_prompts[0]
            .pending_prompt
            .text
            .contains("Already terminal fan-in")
    );
}

#[test]
fn stale_busy_delegation_child_without_runtime_fails_and_releases_wait() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review until the runtime disappears.".to_owned(),
                title: Some("Stale Runtime Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let wait = state
        .create_delegation_wait(
            &parent_session_id,
            CreateDelegationWaitRequest {
                delegation_ids: vec![created.delegation.id.clone()],
                mode: DelegationWaitMode::All,
                title: Some("Stale runtime fan-in".to_owned()),
            },
        )
        .expect("wait should be scheduled");
    assert!(!wait.resume_prompt_queued);

    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child_index = inner
            .find_session_index(&created.delegation.child_session_id)
            .expect("child session should exist");
        let child = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");
        child.runtime = SessionRuntime::None;
        child.session.status = SessionStatus::Approval;
        child.session.preview = "Waiting for approval".to_owned();
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should terminalize the stale child");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|record| record.id == created.delegation.id)
        .expect("delegation should exist");
    assert_eq!(delegation.status, DelegationStatus::Failed);
    assert_eq!(
        delegation
            .result
            .as_ref()
            .map(|result| result.summary.as_str()),
        Some("Codex session exited before the active turn completed")
    );
    assert!(
        inner.delegation_waits.is_empty(),
        "terminal refresh should consume the fan-in wait"
    );
    assert!(
        inner.running_read_only_delegations.is_empty(),
        "stale child must not remain in the running delegation index"
    );
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should exist");
    assert!(
        parent.session.messages.iter().any(|message| matches!(
            message,
            Message::Text {
                author: Author::You,
                text,
                ..
            } if text.contains("Stale runtime fan-in")
                && text.contains("Codex session exited before the active turn completed")
        )),
        "parent should receive the failed fan-in prompt"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn stale_busy_delegation_child_without_runtime_keeps_completed_result_packet() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review until a completed result arrives before runtime cleanup."
                    .to_owned(),
                title: Some("Stale Runtime Completed Result".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    state
        .create_delegation_wait(
            &parent_session_id,
            CreateDelegationWaitRequest {
                delegation_ids: vec![created.delegation.id.clone()],
                mode: DelegationWaitMode::All,
                title: Some("Completed result fan-in".to_owned()),
            },
        )
        .expect("wait should be scheduled");
    mark_delegation_child_stale_with_result_packet(
        &state,
        &created.delegation.child_session_id,
        SessionStatus::Active,
        "completed",
        "Completed before stale runtime cleanup.",
    );

    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should use the completed result packet");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|record| record.id == created.delegation.id)
        .expect("delegation should exist");
    assert_eq!(delegation.status, DelegationStatus::Completed);
    assert_eq!(
        delegation
            .result
            .as_ref()
            .map(|result| result.summary.as_str()),
        Some("Completed before stale runtime cleanup.")
    );
    assert!(
        inner.delegation_waits.is_empty(),
        "completed result should release the fan-in wait"
    );
    assert!(
        inner.running_read_only_delegations.is_empty(),
        "completed stale-runtime child must not remain in the running delegation index"
    );
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child session should still exist");
    assert_eq!(child.session.status, SessionStatus::Idle);
    assert!(matches!(child.runtime, SessionRuntime::None));
    assert_eq!(
        child.session.preview,
        "Completed before stale runtime cleanup."
    );
    assert!(
        child.queued_prompts.is_empty(),
        "completed stale-runtime child should not leave queued prompts dispatchable"
    );
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should exist");
    assert!(
        parent.session.messages.iter().any(|message| matches!(
            message,
            Message::Text {
                author: Author::You,
                text,
                ..
            } if text.contains("Completed result fan-in")
                && text.contains("Completed before stale runtime cleanup.")
        )),
        "parent should receive the completed fan-in prompt"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn stale_busy_delegation_child_without_runtime_keeps_failed_result_summary() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review until a failed result arrives before runtime cleanup.".to_owned(),
                title: Some("Stale Runtime Failed Result".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    mark_delegation_child_stale_with_result_packet(
        &state,
        &created.delegation.child_session_id,
        SessionStatus::Approval,
        "failed",
        "Failed before stale runtime cleanup.",
    );
    queue_delegation_child_prompt(
        &state,
        &created.delegation.child_session_id,
        "queued-stale-failed-follow-up",
    );

    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should use the failed result packet");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|record| record.id == created.delegation.id)
        .expect("delegation should exist");
    assert_eq!(delegation.status, DelegationStatus::Failed);
    assert_eq!(
        delegation
            .result
            .as_ref()
            .map(|result| result.summary.as_str()),
        Some("Failed before stale runtime cleanup.")
    );
    assert!(
        inner.running_read_only_delegations.is_empty(),
        "failed stale-runtime child must not remain in the running delegation index"
    );
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child session should still exist");
    assert_eq!(child.session.status, SessionStatus::Error);
    assert!(matches!(child.runtime, SessionRuntime::None));
    assert_eq!(
        child.session.preview,
        "Failed before stale runtime cleanup."
    );
    assert!(
        child.queued_prompts.is_empty(),
        "failed stale-runtime child should not leave queued prompts dispatchable"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn stale_busy_delegation_child_without_runtime_uses_agent_name_in_summary() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review until a Cursor runtime disappears.".to_owned(),
                title: Some("Stale Cursor Runtime Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Cursor),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    state
        .create_delegation_wait(
            &parent_session_id,
            CreateDelegationWaitRequest {
                delegation_ids: vec![created.delegation.id.clone()],
                mode: DelegationWaitMode::All,
                title: Some("Stale Cursor fan-in".to_owned()),
            },
        )
        .expect("wait should be scheduled");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child_index = inner
            .find_session_index(&created.delegation.child_session_id)
            .expect("child session should exist");
        let child = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");
        child.runtime = SessionRuntime::None;
        child.session.status = SessionStatus::Approval;
        child.session.preview = "Waiting for Cursor approval".to_owned();
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should terminalize the stale child");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|record| record.id == created.delegation.id)
        .expect("delegation should exist");
    assert_eq!(delegation.status, DelegationStatus::Failed);
    assert_eq!(
        delegation
            .result
            .as_ref()
            .map(|result| result.summary.as_str()),
        Some("Cursor session exited before the active turn completed")
    );
    assert!(
        inner.delegation_waits.is_empty(),
        "terminal refresh should consume the Cursor fan-in wait"
    );
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should exist");
    assert!(
        parent.session.messages.iter().any(|message| matches!(
            message,
            Message::Text {
                author: Author::You,
                text,
                ..
            } if text.contains("Stale Cursor fan-in")
                && text.contains("Cursor session exited before the active turn completed")
        )),
        "parent should receive the Cursor failure fan-in prompt"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn already_terminal_delegation_wait_reports_dispatch_for_idle_parent() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("already-terminal-idle-parent-dispatch");
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review already-finished idle-parent work.".to_owned(),
                title: Some("Already Finished Idle Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    input_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("initial delegation prompt should dispatch");

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nReview finished before the idle-parent wait was scheduled.",
    );
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("delegation should be marked terminal");

    let wait = state
        .create_delegation_wait(
            &parent_session_id,
            CreateDelegationWaitRequest {
                delegation_ids: vec![created.delegation.id.clone()],
                mode: DelegationWaitMode::All,
                title: Some("Already terminal idle fan-in".to_owned()),
            },
        )
        .expect("already-terminal delegation wait should be accepted");

    assert!(wait.resume_prompt_queued);
    assert!(wait.resume_dispatch_requested);
    assert_delegation_wait_response_serializes_queue_flags(&wait, true, true);

    match input_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("parent resume prompt should dispatch immediately")
    {
        CodexRuntimeCommand::Prompt {
            session_id,
            command,
        } => {
            assert_eq!(session_id, parent_session_id);
            assert!(command.prompt.contains("Already terminal idle fan-in"));
            assert!(command.prompt.contains(&created.delegation.id));
            assert!(
                command
                    .prompt
                    .contains("Review finished before the idle-parent wait was scheduled.")
            );
        }
        _ => panic!("delegation wait should dispatch a parent resume prompt"),
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.delegation_waits.is_empty());
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should exist");
    assert!(
        parent.queued_prompts.is_empty(),
        "dispatched idle-parent resume should pop the queued prompt"
    );
}

#[test]
fn delegation_wait_resume_dispatch_failure_emits_structured_delta() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review dispatch failure visibility.".to_owned(),
                title: Some("Dispatch Failure Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    state
        .create_delegation_wait(
            &parent_session_id,
            CreateDelegationWaitRequest {
                delegation_ids: vec![created.delegation.id.clone()],
                mode: DelegationWaitMode::All,
                title: Some("Dispatch failure fan-in".to_owned()),
            },
        )
        .expect("wait should be scheduled");
    let (wrong_runtime, _wrong_runtime_rx) =
        test_claude_runtime_handle("delegation-wait-resume-dispatch-failure");

    let mut delta_events = state.subscribe_delta_events();
    while delta_events.try_recv().is_ok() {}
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let parent_index = inner
            .find_session_index(&parent_session_id)
            .expect("parent should exist");
        let parent = inner
            .session_mut_by_index(parent_index)
            .expect("parent index should be valid");
        parent.runtime = SessionRuntime::Claude(wrong_runtime);
        parent.session.status = SessionStatus::Idle;
    }

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nReview found no issues.",
    );
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("child completion should consume the wait");

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner.delegation_waits.is_empty(),
        "finished child should consume the wait even if resume dispatch fails"
    );
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should exist");
    assert_eq!(
        parent.queued_prompts.len(),
        1,
        "failed resume dispatch should leave the parent resume queued for retry"
    );
    drop(inner);

    let mut saw_consumed_delta = false;
    let mut saw_dispatch_failed_delta = false;
    while let Ok(payload) = delta_events.try_recv() {
        let event: DeltaEvent =
            serde_json::from_str(&payload).expect("delta event should deserialize");
        match event {
            DeltaEvent::DelegationWaitConsumed {
                parent_session_id: consumed_parent_session_id,
                reason,
                ..
            } if consumed_parent_session_id == parent_session_id => {
                assert_eq!(reason, DelegationWaitConsumedReason::Completed);
                saw_consumed_delta = true;
            }
            DeltaEvent::DelegationWaitResumeDispatchFailed {
                parent_session_id: failed_parent_session_id,
                error,
                ..
            } if failed_parent_session_id == parent_session_id => {
                assert!(error.contains("failed to inspect queued resume"));
                assert!(error.contains("unexpected Claude runtime attached to Codex session"));
                saw_dispatch_failed_delta = true;
            }
            _ => {}
        }
    }
    assert!(
        saw_consumed_delta,
        "wait consumption should still be published"
    );
    assert!(
        saw_dispatch_failed_delta,
        "resume dispatch failure should be visible as a structured delta"
    );
}
