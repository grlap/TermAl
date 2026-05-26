//! Delegation wait and fan-in tests split from `delegations.rs`.
//!
//! This module owns delegation wait scheduling, consumption, parent resume, and
//! restart reconciliation coverage. It deliberately does not own persistence
//! delta, read-only enforcement, or result recovery tests.

use super::delegation_support::{
    finish_delegation_child_with_assistant_text, install_delegation_codex_runtime,
    temp_delegation_state_paths, test_app_state_with_delegation_codex_runtime,
    test_app_state_with_drained_delegation_codex_runtime,
};
use super::*;

fn test_app_state() -> AppState {
    test_app_state_with_drained_delegation_codex_runtime("delegation-wait-runtime")
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
fn delegation_wait_all_queues_consolidated_parent_resume_after_children_finish() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let first = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review backend wait flow.".to_owned(),
                title: Some("Backend Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("first delegation should be created");
    let second = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review frontend wait flow.".to_owned(),
                title: Some("Frontend Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("second delegation should be created");

    let wait = state
        .create_delegation_wait(
            &parent_session_id,
            CreateDelegationWaitRequest {
                delegation_ids: vec![first.delegation.id.clone(), second.delegation.id.clone()],
                mode: DelegationWaitMode::All,
                title: Some("Reviewer fan-in".to_owned()),
            },
        )
        .expect("wait should be scheduled");
    assert!(!wait.resume_prompt_queued);
    assert!(!wait.resume_dispatch_requested);
    assert_delegation_wait_response_serializes_queue_flags(&wait, false, false);
    let snapshot = state.snapshot();
    assert_eq!(snapshot.delegation_waits.len(), 1);
    assert_eq!(snapshot.delegation_waits[0].id, wait.wait.id);
    assert_eq!(
        snapshot.delegation_waits[0].delegation_ids,
        vec![first.delegation.id.clone(), second.delegation.id.clone()]
    );

    finish_delegation_child_with_assistant_text(
        &state,
        &first.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nBackend side is clean.\n\nFindings:\n- High src/delegations.rs:1413 - Resume prompt drops child findings.\n\nNotes:\n- Backend wait path inspected.",
    );
    state
        .refresh_delegation_for_child_session(&first.delegation.child_session_id)
        .expect("first refresh should complete");
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert_eq!(inner.delegation_waits.len(), 1);
        let parent = inner
            .sessions
            .iter()
            .find(|record| record.session.id == parent_session_id)
            .expect("parent should exist");
        assert!(
            parent.queued_prompts.is_empty(),
            "all-mode wait should not resume before every child is terminal"
        );
    }

    finish_delegation_child_with_assistant_text(
        &state,
        &second.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nFrontend side is clean.",
    );
    state
        .refresh_delegation_for_child_session(&second.delegation.child_session_id)
        .expect("second refresh should complete and resume parent");

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.delegation_waits.is_empty());
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should exist");
    let resume_text = parent
        .session
        .messages
        .iter()
        .find_map(|message| match message {
            Message::Text {
                author: Author::You,
                text,
                ..
            } if text.contains("Reviewer fan-in") => Some(text),
            _ => None,
        })
        .expect("parent should receive consolidated resume prompt");
    assert!(resume_text.contains(&first.delegation.id));
    assert!(resume_text.contains("Backend side is clean."));
    assert!(
        resume_text
            .contains("High `src/delegations.rs:1413` - Resume prompt drops child findings.")
    );
    assert!(resume_text.contains("Backend wait path inspected."));
    assert!(resume_text.contains(&second.delegation.id));
    assert!(resume_text.contains("Frontend side is clean."));
}

#[test]
fn create_delegation_wait_reports_queue_result_for_returned_wait_only() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let first = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review first finished work.".to_owned(),
                title: Some("First Finished Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("first delegation should be created");
    let second = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review second finished work.".to_owned(),
                title: Some("Second Finished Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("second delegation should be created");

    finish_delegation_child_with_assistant_text(
        &state,
        &first.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nFirst review finished before the wait was scheduled.",
    );
    state
        .refresh_delegation_for_child_session(&first.delegation.child_session_id)
        .expect("first delegation should be marked terminal");
    finish_delegation_child_with_assistant_text(
        &state,
        &second.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nSecond review finished before the wait was scheduled.",
    );
    state
        .refresh_delegation_for_child_session(&second.delegation.child_session_id)
        .expect("second delegation should be marked terminal");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.delegation_waits.push(DelegationWaitRecord {
            id: "delegation-wait-preexisting".to_owned(),
            parent_session_id: parent_session_id.clone(),
            delegation_ids: vec![first.delegation.id.clone()],
            mode: DelegationWaitMode::All,
            created_at: stamp_now(),
            title: Some("Preexisting terminal fan-in".to_owned()),
        });
    }

    let wait = state
        .create_delegation_wait(
            &parent_session_id,
            CreateDelegationWaitRequest {
                delegation_ids: vec![second.delegation.id.clone()],
                mode: DelegationWaitMode::All,
                title: Some("Returned terminal fan-in".to_owned()),
            },
        )
        .expect("returned wait should be accepted");

    assert!(wait.resume_prompt_queued);
    assert!(
        !wait.resume_dispatch_requested,
        "dispatch requested by a different same-parent wait must not leak into this response"
    );
}

#[test]
fn removing_delegation_parent_consumes_pending_wait_with_parent_removed_reason() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review parent-removal wait cleanup.".to_owned(),
                title: Some("Parent Removal Wait".to_owned()),
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
                title: Some("Parent removal fan-in".to_owned()),
            },
        )
        .expect("wait should be scheduled");
    assert!(
        !wait.resume_prompt_queued,
        "running child should leave the wait pending"
    );
    let second_wait = state
        .create_delegation_wait(
            &parent_session_id,
            CreateDelegationWaitRequest {
                delegation_ids: vec![created.delegation.id.clone()],
                mode: DelegationWaitMode::All,
                title: Some("Second parent removal fan-in".to_owned()),
            },
        )
        .expect("second wait should be scheduled");
    assert!(
        !second_wait.resume_prompt_queued,
        "running child should leave the second wait pending"
    );
    let wait_ids = BTreeSet::from([wait.wait.id.clone(), second_wait.wait.id.clone()]);

    let mut delta_events = state.subscribe_delta_events();
    while delta_events.try_recv().is_ok() {}
    state
        .kill_session(&parent_session_id)
        .expect("parent removal should succeed");

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .delegation_waits
            .iter()
            .all(|record| !wait_ids.contains(&record.id)),
        "parent-owned waits should be removed instead of orphaned"
    );
    assert!(
        inner
            .sessions
            .iter()
            .all(|record| record.session.id != created.delegation.child_session_id),
        "parent removal should cascade-delete the running child session"
    );
    let delegation = inner
        .delegations
        .iter()
        .find(|delegation| delegation.id == created.delegation.id)
        .expect("delegation record should remain");
    assert_eq!(delegation.status, DelegationStatus::Failed);
    drop(inner);

    let mut consumed_wait_ids = BTreeSet::new();
    while let Ok(payload) = delta_events.try_recv() {
        let event: DeltaEvent =
            serde_json::from_str(&payload).expect("delta event should deserialize");
        if let DeltaEvent::DelegationWaitConsumed {
            wait_id,
            parent_session_id: consumed_parent_session_id,
            reason,
            ..
        } = event
        {
            if wait_ids.contains(&wait_id) {
                assert_eq!(consumed_parent_session_id, parent_session_id);
                assert_eq!(reason, DelegationWaitConsumedReason::ParentSessionRemoved);
                consumed_wait_ids.insert(wait_id);
            }
        }
    }
    assert_eq!(
        consumed_wait_ids, wait_ids,
        "parent removal should publish reasoned wait-consumed deltas"
    );
}

#[test]
fn removing_delegation_parent_consumes_already_satisfied_wait_with_parent_removed_reason() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review satisfied parent-removal wait cleanup.".to_owned(),
                title: Some("Satisfied Parent Removal Wait".to_owned()),
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
                title: Some("Already satisfied parent removal fan-in".to_owned()),
            },
        )
        .expect("wait should be scheduled");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let delegation_index = inner
            .find_delegation_index(&created.delegation.id)
            .expect("delegation should exist");
        let delegation = inner
            .delegations
            .get_mut(delegation_index)
            .expect("delegation index should be valid");
        delegation.status = DelegationStatus::Completed;
        delegation.completed_at = Some(stamp_now());
        delegation.result = Some(DelegationResult {
            delegation_id: created.delegation.id.clone(),
            child_session_id: created.delegation.child_session_id.clone(),
            status: DelegationStatus::Completed,
            summary: "Completed before parent removal.".to_owned(),
            findings: Vec::new(),
            changed_files: Vec::new(),
            commands_run: Vec::new(),
            notes: Vec::new(),
        });
        inner.mark_delegation_mutated(delegation_index);
    }

    let mut delta_events = state.subscribe_delta_events();
    while delta_events.try_recv().is_ok() {}
    state
        .kill_session(&parent_session_id)
        .expect("parent removal should succeed");

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .delegation_waits
            .iter()
            .all(|record| record.id != wait.wait.id),
        "already-satisfied parent-owned wait should be removed"
    );
    assert!(
        inner
            .sessions
            .iter()
            .all(|record| record.session.id != created.delegation.child_session_id),
        "parent removal should cascade-delete terminal child sessions too"
    );
    drop(inner);

    let mut saw_parent_removed_consumption = false;
    while let Ok(payload) = delta_events.try_recv() {
        let event: DeltaEvent =
            serde_json::from_str(&payload).expect("delta event should deserialize");
        if let DeltaEvent::DelegationWaitConsumed {
            wait_id,
            parent_session_id: consumed_parent_session_id,
            reason,
            ..
        } = event
        {
            if wait_id == wait.wait.id {
                assert_eq!(consumed_parent_session_id, parent_session_id);
                assert_eq!(reason, DelegationWaitConsumedReason::ParentSessionRemoved);
                saw_parent_removed_consumption = true;
            }
        }
    }
    assert!(
        saw_parent_removed_consumption,
        "already-satisfied wait should not be mislabeled as normal completion"
    );
}

#[test]
fn boot_reconciliation_drops_unsatisfied_wait_with_missing_parent_and_running_target() {
    let (project_root, persistence_path, templates_path) = temp_delegation_state_paths();
    let wait_id = "delegation-wait-removed-parent-unsatisfied".to_owned();
    let delegation_id;
    {
        let state = AppState::new_with_paths(
            project_root.to_string_lossy().into_owned(),
            persistence_path.clone(),
            templates_path.clone(),
        )
        .expect("state should boot");
        install_delegation_codex_runtime(&state, "delegation-wait-missing-parent-target");
        let other_parent_session_id = test_session_id(&state, Agent::Codex);
        let other_delegation = state
            .create_read_only_delegation(
                &other_parent_session_id,
                CreateDelegationRequest {
                    prompt: "Keep running while a stale wait owner is missing.".to_owned(),
                    title: Some("Still Running Review".to_owned()),
                    cwd: None,
                    agent: Some(Agent::Codex),
                    model: None,
                    mode: Some(DelegationMode::Reviewer),
                    write_policy: Some(DelegationWritePolicy::ReadOnly),
                },
            )
            .expect("delegation should be created");
        delegation_id = other_delegation.delegation.id.clone();
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let delegation_index = inner
                .find_delegation_index(&delegation_id)
                .expect("delegation should exist");
            let child_session_id = inner.delegations[delegation_index].child_session_id.clone();
            inner.delegations[delegation_index].parent_session_id =
                "session-missing-parent".to_owned();
            let child_index = inner
                .find_session_index(&child_session_id)
                .expect("child session should exist");
            let child = inner
                .session_mut_by_index(child_index)
                .expect("child session index should be valid");
            child.remote_id = Some("remote-recovery-skip".to_owned());
            child.remote_session_id = Some("remote-running-child".to_owned());
            child.session.status = SessionStatus::Active;
            inner.delegation_waits.push(DelegationWaitRecord {
                id: wait_id.clone(),
                parent_session_id: "session-missing-parent".to_owned(),
                delegation_ids: vec![delegation_id.clone()],
                mode: DelegationWaitMode::All,
                created_at: stamp_now(),
                title: Some("Unsatisfied removed-parent fan-in".to_owned()),
            });
            state.commit_locked(&mut inner).unwrap();
        }
        state.shutdown_persist_blocking();
    }

    let restarted = AppState::new_with_paths(
        project_root.to_string_lossy().into_owned(),
        persistence_path.clone(),
        templates_path.clone(),
    )
    .expect("state should reload");

    let inner = restarted.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .delegation_waits
            .iter()
            .all(|record| record.id != wait_id),
        "boot reconciliation should drop stale waits with missing parents even when targets are still running"
    );
    let delegation = inner
        .delegations
        .iter()
        .find(|delegation| delegation.id == delegation_id)
        .expect("target delegation should remain after stale wait cleanup");
    assert_eq!(delegation.status, DelegationStatus::Running);
    drop(inner);
    restarted.shutdown_persist_blocking();

    let state_root = persistence_path
        .parent()
        .expect("persistence path should have a parent")
        .to_path_buf();
    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn legacy_delegation_wait_consumed_delta_defaults_reason_to_completed() {
    let event: DeltaEvent = serde_json::from_value(json!({
        "type": "delegationWaitConsumed",
        "revision": 42,
        "waitId": "delegation-wait-legacy",
        "parentSessionId": "session-legacy"
    }))
    .expect("legacy wait-consumed delta should deserialize");

    match event {
        DeltaEvent::DelegationWaitConsumed {
            wait_id,
            parent_session_id,
            reason,
            ..
        } => {
            assert_eq!(wait_id, "delegation-wait-legacy");
            assert_eq!(parent_session_id, "session-legacy");
            assert_eq!(reason, DelegationWaitConsumedReason::Completed);
        }
        _ => panic!("expected delegation wait consumed delta"),
    }
}

#[test]
fn delegation_wait_consumed_delta_serializes_reason() {
    for (reason, expected_reason) in [
        (DelegationWaitConsumedReason::Completed, "completed"),
        (
            DelegationWaitConsumedReason::ParentSessionRemoved,
            "parentSessionRemoved",
        ),
    ] {
        let event = DeltaEvent::DelegationWaitConsumed {
            revision: 42,
            wait_id: "delegation-wait-serialized".to_owned(),
            parent_session_id: "session-parent".to_owned(),
            reason,
        };

        let value = serde_json::to_value(&event).expect("wait-consumed delta should serialize");

        assert_eq!(value["type"], "delegationWaitConsumed");
        assert_eq!(value["revision"], 42);
        assert_eq!(value["waitId"], "delegation-wait-serialized");
        assert_eq!(value["parentSessionId"], "session-parent");
        assert_eq!(value["reason"], expected_reason);

        let decoded: DeltaEvent =
            serde_json::from_value(value).expect("serialized delta should round-trip");
        match decoded {
            DeltaEvent::DelegationWaitConsumed {
                revision,
                wait_id,
                parent_session_id,
                reason: decoded_reason,
            } => {
                assert_eq!(revision, 42);
                assert_eq!(wait_id, "delegation-wait-serialized");
                assert_eq!(parent_session_id, "session-parent");
                assert_eq!(decoded_reason, reason);
            }
            _ => panic!("expected wait-consumed delta"),
        }
    }
}

#[test]
fn delegation_wait_resume_dispatch_failed_delta_serializes_parent_and_error() {
    let event = DeltaEvent::DelegationWaitResumeDispatchFailed {
        revision: 42,
        parent_session_id: "session-parent".to_owned(),
        error: "failed to inspect queued resume".to_owned(),
    };

    let value = serde_json::to_value(&event).expect("dispatch failure delta should serialize");

    assert_eq!(value["type"], "delegationWaitResumeDispatchFailed");
    assert_eq!(value["revision"], 42);
    assert_eq!(value["parentSessionId"], "session-parent");
    assert_eq!(value["error"], "failed to inspect queued resume");

    let decoded: DeltaEvent =
        serde_json::from_value(value).expect("serialized delta should round-trip");
    match decoded {
        DeltaEvent::DelegationWaitResumeDispatchFailed {
            revision,
            parent_session_id,
            error,
        } => {
            assert_eq!(revision, 42);
            assert_eq!(parent_session_id, "session-parent");
            assert_eq!(error, "failed to inspect queued resume");
        }
        _ => panic!("expected wait resume dispatch failure delta"),
    }
}

#[test]
fn delegation_wait_normalizes_parent_id_and_rejects_oversized_title() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review wait validation.".to_owned(),
                title: Some("Wait Validation".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let oversized_title = "x".repeat(MAX_DELEGATION_TITLE_CHARS + 1);
    let err = match state.create_delegation_wait(
        &parent_session_id,
        CreateDelegationWaitRequest {
            delegation_ids: vec![created.delegation.id.clone()],
            mode: DelegationWaitMode::All,
            title: Some(oversized_title),
        },
    ) {
        Ok(_) => panic!("oversized wait title should be rejected"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        err.message,
        format!("delegation wait title must be at most {MAX_DELEGATION_TITLE_CHARS} characters")
    );

    let boundary_title = "界".repeat(MAX_DELEGATION_TITLE_CHARS);
    let wait = state
        .create_delegation_wait(
            &format!("  {parent_session_id}  "),
            CreateDelegationWaitRequest {
                delegation_ids: vec![created.delegation.id.clone()],
                mode: DelegationWaitMode::All,
                title: Some(format!("  {boundary_title}  ")),
            },
        )
        .expect("normalized parent id and boundary title should be accepted");
    assert_eq!(wait.wait.parent_session_id, parent_session_id);
    assert_eq!(wait.wait.title.as_deref(), Some(boundary_title.as_str()));
}

#[test]
fn delegation_wait_rejects_delegation_owned_by_another_parent() {
    let state = test_app_state();
    let owner_session_id = test_session_id(&state, Agent::Codex);
    let other_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &owner_session_id,
            CreateDelegationRequest {
                prompt: "Review wait ownership.".to_owned(),
                title: Some("Owned Delegation".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let err = match state.create_delegation_wait(
        &other_session_id,
        CreateDelegationWaitRequest {
            delegation_ids: vec![created.delegation.id.clone()],
            mode: DelegationWaitMode::All,
            title: None,
        },
    ) {
        Ok(_) => panic!("foreign delegation should be rejected"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        err.message,
        format!(
            "delegation `{}` does not belong to parent session `{other_session_id}`",
            created.delegation.id
        )
    );
}

#[test]
fn delegation_wait_rejects_archived_codex_parent() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review wait parent eligibility.".to_owned(),
                title: Some("Parent Eligibility".to_owned()),
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
        let parent_index = inner
            .find_session_index(&parent_session_id)
            .expect("parent should exist");
        inner.sessions[parent_index].session.codex_thread_state = Some(CodexThreadState::Archived);
    }

    let err = match state.create_delegation_wait(
        &parent_session_id,
        CreateDelegationWaitRequest {
            delegation_ids: vec![created.delegation.id.clone()],
            mode: DelegationWaitMode::All,
            title: None,
        },
    ) {
        Ok(_) => panic!("archived parent should reject delegation waits"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::CONFLICT);
    assert_eq!(
        err.message,
        "delegation wait parent session is archived; unarchive it before scheduling a wait"
    );
}

#[test]
fn delegation_wait_consumes_without_queueing_when_parent_becomes_archived() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review wait parent archive race.".to_owned(),
                title: Some("Parent Archive Race".to_owned()),
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
                title: Some("Archived parent fan-in".to_owned()),
            },
        )
        .expect("wait should be scheduled");

    let mut delta_events = state.subscribe_delta_events();
    while delta_events.try_recv().is_ok() {}
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let parent_index = inner
            .find_session_index(&parent_session_id)
            .expect("parent should exist");
        inner.sessions[parent_index].session.codex_thread_state = Some(CodexThreadState::Archived);
        state.commit_locked(&mut inner).unwrap();
    }

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nParent archived before fan-in finished.",
    );
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("child completion should consume the wait");

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .delegation_waits
            .iter()
            .all(|record| record.id != wait.wait.id),
        "wait should be consumed instead of left pending"
    );
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should exist");
    assert!(
        parent.queued_prompts.is_empty(),
        "archived parents should not receive stranded resume prompts"
    );
    drop(inner);

    let mut saw_parent_unavailable_consumption = false;
    while let Ok(payload) = delta_events.try_recv() {
        let event: DeltaEvent =
            serde_json::from_str(&payload).expect("delta event should deserialize");
        if let DeltaEvent::DelegationWaitConsumed {
            wait_id,
            parent_session_id: consumed_parent_session_id,
            reason,
            ..
        } = event
        {
            if wait_id == wait.wait.id {
                assert_eq!(consumed_parent_session_id, parent_session_id);
                assert_eq!(
                    reason,
                    DelegationWaitConsumedReason::ParentSessionUnavailable
                );
                saw_parent_unavailable_consumption = true;
            }
        }
    }
    assert!(
        saw_parent_unavailable_consumption,
        "archived-parent wait should publish a reasoned consumed delta"
    );
}

#[test]
fn delegation_wait_resume_prompt_is_capped_with_marker() {
    let oversized = "界".repeat(MAX_DELEGATION_WAIT_RESUME_PROMPT_BYTES);
    let prompt = limit_delegation_wait_resume_prompt(oversized);

    assert!(prompt.len() <= MAX_DELEGATION_WAIT_RESUME_PROMPT_BYTES);
    assert!(prompt.ends_with(DELEGATION_WAIT_RESUME_TRUNCATED_MARKER));
}

#[test]
fn delegation_wait_missing_record_prompt_uses_resume_prompt_cap() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    // Unit-test the cap directly with an oversized missing id; API validation
    // rejects ids this large before they can be persisted through normal routes.
    let wait = DelegationWaitRecord {
        id: "delegation-wait-missing-record-cap".to_owned(),
        parent_session_id,
        delegation_ids: vec!["x".repeat(MAX_DELEGATION_WAIT_RESUME_PROMPT_BYTES)],
        mode: DelegationWaitMode::All,
        created_at: stamp_now(),
        title: None,
    };

    let inner = state.inner.lock().expect("state mutex poisoned");
    let prompt = delegation_wait_resume_prompt_locked(&inner, &wait)
        .expect("missing delegation records should resolve the wait");

    assert!(prompt.len() <= MAX_DELEGATION_WAIT_RESUME_PROMPT_BYTES);
    assert!(prompt.ends_with(DELEGATION_WAIT_RESUME_TRUNCATED_MARKER));
}

#[test]
fn delegation_wait_dispatches_resume_prompt_to_idle_parent_runtime() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("delegation-wait-parent-dispatch");
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review the delegation fan-in loop.".to_owned(),
                title: Some("Independent Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    match input_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("delegation child prompt should be delivered first")
    {
        CodexRuntimeCommand::Prompt {
            session_id,
            command,
        } => {
            assert_eq!(session_id, created.delegation.child_session_id);
            assert!(
                command
                    .prompt
                    .contains("Review the delegation fan-in loop.")
            );
        }
        _ => panic!("delegation should dispatch the child review prompt first"),
    }

    state
        .create_delegation_wait(
            &parent_session_id,
            CreateDelegationWaitRequest {
                delegation_ids: vec![created.delegation.id.clone()],
                mode: DelegationWaitMode::All,
                title: Some("Act on independent review".to_owned()),
            },
        )
        .expect("wait should be scheduled");

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nThe independent review is ready to act on.",
    );
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("child completion should resume the parent");

    match input_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("parent resume prompt should dispatch to runtime")
    {
        CodexRuntimeCommand::Prompt {
            session_id,
            command,
        } => {
            assert_eq!(session_id, parent_session_id);
            assert!(command.prompt.contains("Act on independent review"));
            assert!(command.prompt.contains(&created.delegation.id));
            assert!(
                command
                    .prompt
                    .contains("The independent review is ready to act on.")
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
    assert_eq!(parent.session.status, SessionStatus::Active);
}

#[test]
fn delegation_wait_reconciles_after_restart_recovery() {
    let (project_root, persistence_path, templates_path) = temp_delegation_state_paths();
    let parent_session_id;
    let delegation_id;
    {
        let state = AppState::new_with_paths(
            project_root.to_string_lossy().into_owned(),
            persistence_path.clone(),
            templates_path.clone(),
        )
        .expect("state should boot");
        install_delegation_codex_runtime(&state, "delegation-wait-restart-runtime");
        parent_session_id = test_session_id(&state, Agent::Codex);
        let created = state
            .create_read_only_delegation(
                &parent_session_id,
                CreateDelegationRequest {
                    prompt: "Run until restart recovery handles this delegation.".to_owned(),
                    title: Some("Restart Recovery Review".to_owned()),
                    cwd: None,
                    agent: Some(Agent::Codex),
                    model: None,
                    mode: Some(DelegationMode::Reviewer),
                    write_policy: Some(DelegationWritePolicy::ReadOnly),
                },
            )
            .expect("delegation should be created");
        delegation_id = created.delegation.id.clone();
        state
            .create_delegation_wait(
                &parent_session_id,
                CreateDelegationWaitRequest {
                    delegation_ids: vec![delegation_id.clone()],
                    mode: DelegationWaitMode::All,
                    title: Some("Restart fan-in".to_owned()),
                },
            )
            .expect("wait should be scheduled");
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let parent_index = inner
                .find_session_index(&parent_session_id)
                .expect("parent should exist");
            inner.sessions[parent_index].orchestrator_auto_dispatch_blocked = true;
            state.commit_locked(&mut inner).unwrap();
        }
        state.shutdown_persist_blocking();
    }

    let restarted = AppState::new_with_paths(
        project_root.to_string_lossy().into_owned(),
        persistence_path.clone(),
        templates_path.clone(),
    )
    .expect("state should reload");
    let inner = restarted.inner.lock().expect("state mutex poisoned");
    assert!(inner.delegation_waits.is_empty());
    let delegation = inner
        .delegations
        .iter()
        .find(|delegation| delegation.id == delegation_id)
        .expect("delegation should reload");
    assert_eq!(delegation.status, DelegationStatus::Failed);
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should reload");
    let resume_prompt = parent
        .queued_prompts
        .front()
        .expect("blocked parent should keep queued resume prompt");
    assert!(resume_prompt.pending_prompt.text.contains("Restart fan-in"));
    assert!(resume_prompt.pending_prompt.text.contains(&delegation_id));
    assert!(
        resume_prompt
            .pending_prompt
            .text
            .contains("failed - Restart Recovery Review")
    );
    drop(inner);
    restarted.shutdown_persist_blocking();

    let state_root = persistence_path
        .parent()
        .expect("persistence path should have a parent")
        .to_path_buf();
    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn delegation_wait_reconciles_missing_parent_after_restart() {
    let (project_root, persistence_path, templates_path) = temp_delegation_state_paths();
    let wait_id = "delegation-wait-restart-missing-parent".to_owned();
    {
        let state = AppState::new_with_paths(
            project_root.to_string_lossy().into_owned(),
            persistence_path.clone(),
            templates_path.clone(),
        )
        .expect("state should boot");
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            inner.delegation_waits.push(DelegationWaitRecord {
                id: wait_id.clone(),
                parent_session_id: "session-missing-parent".to_owned(),
                delegation_ids: vec!["delegation-stale-target".to_owned()],
                mode: DelegationWaitMode::All,
                created_at: stamp_now(),
                title: Some("Restart missing-parent fan-in".to_owned()),
            });
            state.commit_locked(&mut inner).unwrap();
        }
        state.shutdown_persist_blocking();
    }

    let restarted = AppState::new_with_paths(
        project_root.to_string_lossy().into_owned(),
        persistence_path.clone(),
        templates_path.clone(),
    )
    .expect("state should reload");
    let inner = restarted.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .delegation_waits
            .iter()
            .all(|record| record.id != wait_id),
        "boot reconciliation should not retain waits whose parent is missing"
    );
    drop(inner);
    restarted.shutdown_persist_blocking();

    let state_root = persistence_path
        .parent()
        .expect("persistence path should have a parent")
        .to_path_buf();
    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(state_root);
}

#[test]
fn delegation_wait_reconciles_when_child_session_is_removed() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "This child will be removed before it finishes.".to_owned(),
                title: Some("Removed Child Review".to_owned()),
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
                title: Some("Removed child fan-in".to_owned()),
            },
        )
        .expect("wait should be scheduled");

    state
        .kill_session(&created.delegation.child_session_id)
        .expect("child session removal should succeed");

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.delegation_waits.is_empty());
    let delegation = inner
        .delegations
        .iter()
        .find(|delegation| delegation.id == created.delegation.id)
        .expect("delegation should remain as audit record");
    assert_eq!(delegation.status, DelegationStatus::Failed);
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should exist");
    let resume_text = parent
        .session
        .messages
        .iter()
        .find_map(|message| match message {
            Message::Text {
                author: Author::You,
                text,
                ..
            } if text.contains("Removed child fan-in") => Some(text),
            _ => None,
        })
        .expect("parent should receive child-removal resume prompt");
    assert!(resume_text.contains(&created.delegation.id));
    assert!(resume_text.contains("delegation child session was removed"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}
