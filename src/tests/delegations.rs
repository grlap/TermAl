use super::*;

fn temp_delegation_state_paths() -> (PathBuf, PathBuf, PathBuf) {
    let unique = Uuid::new_v4();
    let project_root = std::env::temp_dir().join(format!("termal-delegation-root-{unique}"));
    let state_root = std::env::temp_dir().join(format!("termal-delegation-state-{unique}"));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&state_root).expect("state root should exist");
    (
        project_root,
        state_root.join("sessions.json"),
        state_root.join("orchestrators.json"),
    )
}

fn finish_delegation_child_with_assistant_text(
    state: &AppState,
    child_session_id: &str,
    text: &str,
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
            text: text.to_owned(),
            expanded_text: None,
        },
    );
    child.session.status = SessionStatus::Idle;
    child.session.preview = text.lines().last().unwrap_or_default().to_owned();
    state.commit_locked(&mut inner).unwrap();
}

fn assert_read_only_delegation_error(value: &Value, title: &str) {
    let error = value["error"]
        .as_str()
        .expect("error response should include a message");
    assert!(error.contains("disabled for read-only delegated sessions"));
    assert!(error.contains(&format!("while read-only delegation `{title}` is running")));
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

fn test_app_state_with_delegation_codex_runtime(
    runtime_id: &str,
) -> (AppState, mpsc::Receiver<CodexRuntimeCommand>) {
    let state = super::test_app_state();
    let (runtime, input_rx, _process) = test_shared_codex_runtime(runtime_id);
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);
    (state, input_rx)
}

fn test_app_state() -> AppState {
    let (state, input_rx) = test_app_state_with_delegation_codex_runtime("delegation-test-runtime");
    std::thread::spawn(move || while input_rx.recv().is_ok() {});
    state
}

fn create_delegation_for_persist_transition(title: &str) -> (AppState, DelegationResponse, u64) {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: format!("Check {title} persistence transition coverage."),
                title: Some(title.to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let watermark = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.collect_persist_delta(0).watermark
    };
    (state, created, watermark)
}

#[test]
fn claude_read_only_delegation_child_keeps_app_default_approval_mode_and_effort() {
    let state = test_app_state();
    let workdir = std::env::temp_dir().to_string_lossy().into_owned();
    let child_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.preferences.default_claude_approval_mode = ClaudeApprovalMode::AutoApprove;
        inner.preferences.default_claude_effort = ClaudeEffortLevel::Max;
        let child = inner.create_session(
            Agent::Claude,
            Some("Claude Delegation Defaults".to_owned()),
            workdir,
            None,
            None,
        );
        let child_session_id = child.session.id.clone();
        let child_index = inner
            .find_session_index(&child_session_id)
            .expect("child session should be indexed");
        let child_record = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");

        configure_delegation_child_prompt_settings(child_record, &DelegationWritePolicy::ReadOnly);
        child_session_id
    };

    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == child_session_id)
        .expect("child session should exist");
    assert_eq!(
        child.session.claude_approval_mode,
        Some(ClaudeApprovalMode::AutoApprove)
    );
    assert_eq!(child.session.claude_effort, Some(ClaudeEffortLevel::Max));
}

#[test]
fn claude_write_delegation_child_keeps_app_default_approval_mode_and_effort() {
    let state = test_app_state();
    let workdir = std::env::temp_dir().to_string_lossy().into_owned();
    let write_policies = [
        DelegationWritePolicy::SharedWorktree {
            owned_paths: vec!["src".to_owned()],
        },
        DelegationWritePolicy::IsolatedWorktree {
            owned_paths: vec!["src".to_owned()],
            worktree_path: Some(
                std::env::temp_dir()
                    .join(format!("termal-claude-write-delegation-{}", Uuid::new_v4()))
                    .to_string_lossy()
                    .into_owned(),
            ),
        },
    ];

    for write_policy in write_policies {
        let child_session_id = {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            inner.preferences.default_claude_approval_mode = ClaudeApprovalMode::AutoApprove;
            inner.preferences.default_claude_effort = ClaudeEffortLevel::Max;
            let child = inner.create_session(
                Agent::Claude,
                Some("Claude Write Delegation Defaults".to_owned()),
                workdir.clone(),
                None,
                None,
            );
            let child_session_id = child.session.id.clone();
            let child_index = inner
                .find_session_index(&child_session_id)
                .expect("child session should be indexed");
            let child_record = inner
                .session_mut_by_index(child_index)
                .expect("child session index should be valid");

            configure_delegation_child_prompt_settings(child_record, &write_policy);
            child_session_id
        };

        let inner = state.inner.lock().expect("state mutex poisoned");
        let child = inner
            .sessions
            .iter()
            .find(|record| record.session.id == child_session_id)
            .expect("child session should exist");
        assert_eq!(
            child.session.claude_approval_mode,
            Some(ClaudeApprovalMode::AutoApprove)
        );
        assert_eq!(child.session.claude_effort, Some(ClaudeEffortLevel::Max));
    }
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

fn assert_persist_delta_contains_delegation_status(
    delta: &PersistDelta,
    delegation_id: &str,
    status: DelegationStatus,
) {
    let delegations = delta
        .changed_delegations
        .as_ref()
        .expect("delegation transition should be persisted");
    let delegation = delegations
        .iter()
        .find(|delegation| delegation.id == delegation_id)
        .expect("updated delegation should be present");
    assert_eq!(delegation.status, status);
}

#[test]
fn delegation_create_and_completion_are_included_in_persist_delta() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let before_create = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner.last_mutation_stamp
    };

    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Check persistence delta coverage.".to_owned(),
                title: Some("Persist Delta".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let create_watermark = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let delta = inner.collect_persist_delta(before_create);
        let delegations = delta
            .changed_delegations
            .as_ref()
            .expect("created delegation should be persisted");
        assert!(
            delegations
                .iter()
                .any(|delegation| delegation.id == created.delegation.id)
        );
        delta.watermark
    };
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        assert!(
            inner
                .collect_persist_delta(create_watermark)
                .changed_delegations
                .is_none(),
            "delegations should not be rewritten again after the watermark catches up"
        );
    }

    let before_completion = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner.last_mutation_stamp
    };
    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nDelegation complete.",
    );
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete delegation");

    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let delta = inner.collect_persist_delta(before_completion);
    let delegations = delta
        .changed_delegations
        .as_ref()
        .expect("completed delegation should be persisted");
    let delegation = delegations
        .iter()
        .find(|delegation| delegation.id == created.delegation.id)
        .expect("completed delegation should be present");
    assert_eq!(delegation.status, DelegationStatus::Completed);
}

#[test]
fn sqlite_empty_delegation_table_preserves_embedded_delegations_for_migration() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Persisted before dedicated delegation table.".to_owned(),
                title: Some("Embedded Delegation".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let mut persisted = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        PersistedState::from_inner(&inner)
    };

    let loaded_from_table = apply_sqlite_delegation_records(&mut persisted, Vec::new());

    assert!(!loaded_from_table);
    assert!(
        persisted
            .delegations
            .iter()
            .any(|delegation| delegation.id == created.delegation.id),
        "empty dedicated table must not wipe embedded delegation metadata"
    );

    let mut inner = persisted
        .into_inner()
        .expect("embedded delegation state should load");
    inner.mark_loaded_delegations_for_sqlite_migration();
    let delta = inner.collect_persist_delta(0);
    let delegations = delta
        .changed_delegations
        .as_ref()
        .expect("embedded delegations should be migrated into the dedicated table");
    assert!(
        delegations
            .iter()
            .any(|delegation| delegation.id == created.delegation.id)
    );
}

#[test]
fn pure_delegation_update_is_included_in_persist_delta() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Check pure delegation persistence delta coverage.".to_owned(),
                title: Some("Initial Delegation Title".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let create_watermark = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.collect_persist_delta(0).watermark
    };

    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter_mut()
        .find(|delegation| delegation.id == created.delegation.id)
        .expect("delegation should exist");
    delegation.title = "Pure Delegation Update".to_owned();
    let delegation_index = inner
        .find_delegation_index(&created.delegation.id)
        .expect("delegation should exist");
    inner.mark_delegation_mutated(delegation_index);

    let delta = inner.collect_persist_delta(create_watermark);
    assert!(
        delta.changed_sessions.is_empty(),
        "pure delegation updates should not depend on a restamped session"
    );
    let delegations = delta
        .changed_delegations
        .as_ref()
        .expect("pure delegation update should be persisted");
    assert_eq!(delegations.len(), 1);
    let delegation = delegations
        .iter()
        .find(|delegation| delegation.id == created.delegation.id)
        .expect("updated delegation should be present");
    assert_eq!(delegation.title, "Pure Delegation Update");
}

#[test]
fn delegation_persist_delta_includes_only_changed_delegation_rows() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let first = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "First unchanged delegation.".to_owned(),
                title: Some("Unchanged Delegation".to_owned()),
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
                prompt: "Second changed delegation.".to_owned(),
                title: Some("Changed Delegation".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("second delegation should be created");
    let watermark = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.collect_persist_delta(0).watermark
    };

    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let delegation_index = inner
        .find_delegation_index(&second.delegation.id)
        .expect("second delegation should exist");
    inner.delegations[delegation_index].title = "Only This Row Changed".to_owned();
    inner.mark_delegation_mutated(delegation_index);

    let delta = inner.collect_persist_delta(watermark);
    let delegations = delta
        .changed_delegations
        .as_ref()
        .expect("changed delegation should be persisted");
    assert_eq!(
        delegations
            .iter()
            .map(|delegation| delegation.id.as_str())
            .collect::<Vec<_>>(),
        vec![second.delegation.id.as_str()]
    );
    assert!(
        !delegations
            .iter()
            .any(|delegation| delegation.id == first.delegation.id),
        "unchanged delegation rows should not be rewritten"
    );
}

#[test]
fn delegation_persist_delta_includes_removed_delegation_tombstones() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Delegation to remove from persistence.".to_owned(),
                title: Some("Removed Delegation".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let watermark = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.collect_persist_delta(0).watermark
    };

    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let delegation_index = inner
        .find_delegation_index(&created.delegation.id)
        .expect("delegation should exist");
    inner.remove_delegation_at(delegation_index);

    let delta = inner.collect_persist_delta(watermark);
    assert_eq!(
        delta.removed_delegation_ids,
        vec![created.delegation.id.clone()]
    );
    assert!(inner.removed_delegation_ids.is_empty());
    assert!(
        delta.changed_delegations.is_none(),
        "removed delegation rows should not be re-upserted"
    );
    let drained_tombstones = delta.drained_delegation_tombstones.clone();

    inner.restore_drained_delegation_tombstones(&drained_tombstones);
    let retry_delta = inner.collect_persist_delta(watermark);
    assert_eq!(
        retry_delta.removed_delegation_ids,
        vec![created.delegation.id.clone()]
    );
}

#[test]
fn delegation_status_transitions_are_included_in_persist_delta() {
    let (state, created, watermark) = create_delegation_for_persist_transition("Running Delta");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let delegation_index = inner
            .find_delegation_index(&created.delegation.id)
            .expect("delegation should exist");
        inner.delegations[delegation_index].status = DelegationStatus::Queued;
        let lifecycle_delta = refresh_delegation_from_child_locked(&mut inner, delegation_index)
            .expect("queued delegation with active child should transition to running");
        assert!(matches!(
            lifecycle_delta,
            DelegationLifecycleDelta::Updated {
                status: DelegationStatus::Running,
                ..
            }
        ));
        let delta = inner.collect_persist_delta(watermark);
        assert_persist_delta_contains_delegation_status(
            &delta,
            &created.delegation.id,
            DelegationStatus::Running,
        );
    }

    let (state, created, watermark) = create_delegation_for_persist_transition("Failed Delta");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let delegation_index = inner
            .find_delegation_index(&created.delegation.id)
            .expect("delegation should exist");
        mark_delegation_failed_locked(&mut inner, delegation_index, "transition failed")
            .expect("running delegation should transition to failed");
        let delta = inner.collect_persist_delta(watermark);
        assert_persist_delta_contains_delegation_status(
            &delta,
            &created.delegation.id,
            DelegationStatus::Failed,
        );
    }

    let (state, created, watermark) = create_delegation_for_persist_transition("Canceled Delta");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let delegation_index = inner
            .find_delegation_index(&created.delegation.id)
            .expect("delegation should exist");
        mark_delegation_canceled_locked(
            &mut inner,
            delegation_index,
            Some("transition canceled".to_owned()),
        )
        .expect("running delegation should transition to canceled");
        let delta = inner.collect_persist_delta(watermark);
        assert_persist_delta_contains_delegation_status(
            &delta,
            &created.delegation.id,
            DelegationStatus::Canceled,
        );
    }
}

#[cfg(any(unix, windows))]
fn create_test_dir_symlink(target: &FsPath, link: &FsPath) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link)
    }
}

#[cfg(windows)]
fn windows_symlink_privilege_unavailable(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::PermissionDenied || err.raw_os_error() == Some(1314)
}

fn expect_delegation_cwd_rejected(
    state: &AppState,
    parent_session_id: &str,
    cwd: String,
) -> ApiError {
    match state.create_read_only_delegation(
        parent_session_id,
        CreateDelegationRequest {
            prompt: "Validate delegation cwd.".to_owned(),
            title: Some("Cwd Validation".to_owned()),
            cwd: Some(cwd),
            agent: Some(Agent::Codex),
            model: None,
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("invalid delegation cwd should be rejected"),
        Err(err) => err,
    }
}

fn install_delegation_codex_runtime(state: &AppState, runtime_id: &str) {
    let (runtime, input_rx, _process) = test_shared_codex_runtime(runtime_id);
    *state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned") = Some(runtime);
    std::thread::spawn(move || while input_rx.recv().is_ok() {});
}

fn install_delegation_acp_runtime(
    state: &AppState,
    agent: AcpAgent,
    runtime_id: &str,
) -> mpsc::Receiver<AcpRuntimeCommand> {
    let (runtime, input_rx) = test_acp_runtime_handle(agent, runtime_id);
    state.install_test_acp_runtime_override(agent, runtime);
    input_rx
}

fn push_delegation_child_assistant_text_without_finishing(
    state: &AppState,
    child_session_id: &str,
    text: &str,
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
            text: text.to_owned(),
            expanded_text: None,
        },
    );
    child.session.preview = text.lines().last().unwrap_or_default().to_owned();
    state.commit_locked(&mut inner).unwrap();
}

fn runtime_token_for_session(state: &AppState, session_id: &str) -> RuntimeToken {
    let inner = state.inner.lock().expect("state mutex poisoned");
    let index = inner
        .find_session_index(session_id)
        .expect("session should exist");
    inner.sessions[index]
        .runtime
        .runtime_token()
        .expect("session should have an attached runtime")
}

fn queue_delegation_child_prompt(state: &AppState, child_session_id: &str, text: &str) {
    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let child_index = inner
        .find_session_index(child_session_id)
        .expect("child session should exist");
    let message_id = inner.next_message_id();
    inner.sessions[child_index]
        .queued_prompts
        .push_back(QueuedPromptRecord {
            source: QueuedPromptSource::User,
            attachments: Vec::new(),
            pending_prompt: PendingPrompt {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                text: text.to_owned(),
                expanded_text: None,
            },
        });
    sync_pending_prompts(&mut inner.sessions[child_index]);
    state.commit_locked(&mut inner).unwrap();
}

fn shared_codex_runtime_for_state(state: &AppState) -> SharedCodexRuntime {
    state
        .shared_codex_runtime
        .lock()
        .expect("shared Codex runtime mutex poisoned")
        .clone()
        .expect("test should install a shared Codex runtime")
}

fn parent_delegation_card_has_status(
    inner: &StateInner,
    parent_session_id: &str,
    delegation_id: &str,
    status: ParallelAgentStatus,
) -> bool {
    inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .map(|parent| {
            parent.session.messages.iter().any(|message| {
                matches!(
                    message,
                    Message::ParallelAgents { agents, .. }
                        if agents
                            .iter()
                            .any(|agent| agent.id == delegation_id
                                && agent.source == ParallelAgentSource::Delegation
                                && agent.status == status)
                )
            })
        })
        .unwrap_or(false)
}

fn assert_single_parent_delegation_agent_status(
    inner: &StateInner,
    parent_session_id: &str,
    delegation_id: &str,
    status: ParallelAgentStatus,
) {
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent session should exist");
    let matching_agents: Vec<&ParallelAgentProgress> = parent
        .session
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::ParallelAgents { agents, .. } => Some(agents),
            _ => None,
        })
        .flat_map(|agents| agents.iter())
        .filter(|agent| agent.id == delegation_id)
        .collect();
    assert_eq!(
        matching_agents.len(),
        1,
        "parent card should have exactly one row for the delegation id"
    );
    let agent = matching_agents[0];
    assert_eq!(
        agent.source,
        ParallelAgentSource::Delegation,
        "parent card row for a delegation id must be delegation-sourced"
    );
    assert_eq!(agent.status, status);
}

#[test]
fn delegation_parent_card_update_ignores_tool_source_id_collision() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Exercise parent card source matching.".to_owned(),
                title: Some("Source Collision".to_owned()),
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
            .expect("parent session should exist");
        let parent = inner
            .session_mut_by_index(parent_index)
            .expect("parent session index should be valid");
        let parent_agents = parent
            .session
            .messages
            .iter_mut()
            .rev()
            .find_map(|message| match message {
                Message::ParallelAgents { agents, .. } => Some(agents),
                _ => None,
            })
            .expect("parent delegation card should exist");
        parent_agents.insert(
            0,
            ParallelAgentProgress {
                detail: Some("Tool row must not be touched".to_owned()),
                id: created.delegation.id.clone(),
                source: ParallelAgentSource::Tool,
                status: ParallelAgentStatus::Running,
                title: "Tool collision".to_owned(),
            },
        );

        let delegation_index = inner
            .find_delegation_index(&created.delegation.id)
            .expect("delegation should exist");
        mark_delegation_failed_locked(&mut inner, delegation_index, "delegation failed")
            .expect("running delegation should fail");

        let parent = inner
            .sessions
            .iter()
            .find(|record| record.session.id == parent_session_id)
            .expect("parent session should exist");
        let agents: Vec<&ParallelAgentProgress> = parent
            .session
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::ParallelAgents { agents, .. } => Some(agents),
                _ => None,
            })
            .flat_map(|agents| agents.iter())
            .filter(|agent| agent.id == created.delegation.id)
            .collect();
        let tool_agent = agents
            .iter()
            .find(|agent| agent.source == ParallelAgentSource::Tool)
            .expect("tool-source collision row should remain");
        assert_eq!(tool_agent.status, ParallelAgentStatus::Running);
        assert_eq!(
            tool_agent.detail.as_deref(),
            Some("Tool row must not be touched")
        );
        let delegation_agent = agents
            .iter()
            .find(|agent| agent.source == ParallelAgentSource::Delegation)
            .expect("delegation-source row should remain");
        assert_eq!(delegation_agent.status, ParallelAgentStatus::Error);
        assert_eq!(
            delegation_agent.detail.as_deref(),
            Some("delegation failed")
        );
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_records_persist_and_reload_with_child_link() {
    let (project_root, persistence_path, templates_path) = temp_delegation_state_paths();
    let delegation_id;
    let child_session_id;
    {
        let state = AppState::new_with_paths(
            project_root.to_string_lossy().into_owned(),
            persistence_path.clone(),
            templates_path.clone(),
        )
        .expect("state should boot");
        install_delegation_codex_runtime(&state, "delegation-persist-runtime");
        let parent_session_id = test_session_id(&state, Agent::Codex);
        let created = state
            .create_read_only_delegation(
                &parent_session_id,
                CreateDelegationRequest {
                    prompt: "Inspect the backend persistence shape.".to_owned(),
                    title: Some("Persistence Review".to_owned()),
                    cwd: None,
                    agent: Some(Agent::Codex),
                    model: None,
                    mode: Some(DelegationMode::Reviewer),
                    write_policy: Some(DelegationWritePolicy::ReadOnly),
                },
            )
            .expect("delegation should be created");
        delegation_id = created.delegation.id.clone();
        child_session_id = created.delegation.child_session_id.clone();
        assert_eq!(created.delegation.parent_session_id, parent_session_id);
        assert_eq!(created.delegation.status, DelegationStatus::Running);
        assert_eq!(
            created.child_session.parent_delegation_id.as_deref(),
            Some(delegation_id.as_str())
        );
        state.shutdown_persist_blocking();
    }

    let restarted = AppState::new_with_paths(
        project_root.to_string_lossy().into_owned(),
        persistence_path.clone(),
        templates_path.clone(),
    )
    .expect("state should reload");
    let inner = restarted.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|record| record.id == delegation_id)
        .expect("delegation should reload");
    assert_eq!(delegation.child_session_id, child_session_id);
    assert_eq!(delegation.status, DelegationStatus::Failed);
    assert!(
        delegation
            .result
            .as_ref()
            .expect("recovered delegation should have a result")
            .summary
            .contains("TermAl restarted")
    );
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == child_session_id)
        .expect("child session should reload");
    assert_eq!(
        child.session.parent_delegation_id.as_deref(),
        Some(delegation_id.as_str())
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

#[tokio::test]
async fn delegation_routes_create_status_and_unavailable_result() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "prompt": "Read-only route check",
        "title": "Route Delegation",
        "mode": "reviewer",
        "writePolicy": { "kind": "readOnly" }
    }))
    .expect("delegation request should serialize");

    let (create_status, created): (StatusCode, DelegationResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{parent_session_id}/delegations"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(create_status, StatusCode::CREATED);
    assert_eq!(created.delegation.parent_session_id, parent_session_id);
    assert_eq!(created.delegation.status, DelegationStatus::Running);
    assert_eq!(
        created.child_session.parent_delegation_id.as_deref(),
        Some(created.delegation.id.as_str())
    );

    let (get_status, fetched): (StatusCode, DelegationStatusResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/sessions/{parent_session_id}/delegations/{}",
                created.delegation.id
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(get_status, StatusCode::OK);
    assert_eq!(fetched.delegation, created.delegation);

    let (result_status, result_body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/sessions/{parent_session_id}/delegations/{}/result",
                created.delegation.id
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(result_status, StatusCode::CONFLICT);
    assert!(
        result_body["error"]
            .as_str()
            .expect("error response should include a message")
            .contains("result is not available yet")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn delegation_result_route_uses_camel_case_json_shape() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Return route result metadata.".to_owned(),
                title: Some("Route Result Shape".to_owned()),
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
        let command_id = inner.next_message_id();
        let files_id = inner.next_message_id();
        let result_id = inner.next_message_id();
        let child = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");
        push_message_on_record(
            child,
            Message::Command {
                id: command_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                command: "cargo test delegations".to_owned(),
                command_language: Some("shell".to_owned()),
                output: "ok".to_owned(),
                output_language: Some("text".to_owned()),
                status: CommandStatus::Success,
            },
        );
        push_message_on_record(
            child,
            Message::FileChanges {
                id: files_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Agent changed 1 file".to_owned(),
                files: vec![FileChangeSummaryEntry {
                    path: "src/main.rs".to_owned(),
                    kind: WorkspaceFileChangeKind::Modified,
                }],
            },
        );
        push_message_on_record(
            child,
            Message::Text {
                attachments: Vec::new(),
                id: result_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "## Result\n\nStatus: completed\n\nSummary:\nRoute shape pinned.\n\nFindings:\n- Medium src/main.rs:42 - Route result carries review findings.\n\nNotes:\n- Result packet metadata inspected.\n\nFiles Inspected:\n- src/main.rs".to_owned(),
                expanded_text: None,
            },
        );
        child.session.status = SessionStatus::Idle;
        child.session.preview = "Route shape pinned.".to_owned();
        state.commit_locked(&mut inner).unwrap();
    }

    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete");
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/sessions/{parent_session_id}/delegations/{}/result",
                created.delegation.id
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(body["revision"].is_u64());
    assert!(body["serverInstanceId"].is_string());
    let result = &body["result"];
    assert_eq!(result["delegationId"], created.delegation.id);
    assert_eq!(
        result["childSessionId"],
        created.delegation.child_session_id
    );
    assert_eq!(result["status"], "completed");
    assert_eq!(result["summary"], "Route shape pinned.");
    assert_eq!(
        result["findings"],
        json!([{
            "severity": "Medium",
            "file": "src/main.rs",
            "line": 42,
            "message": "Route result carries review findings."
        }])
    );
    assert_eq!(result["changedFiles"], json!(["src/main.rs"]));
    assert_eq!(
        result["commandsRun"],
        json!([{
            "command": "cargo test delegations",
            "status": "success"
        }])
    );
    assert_eq!(
        result["notes"],
        json!(["Result packet metadata inspected.", "Inspected src/main.rs"])
    );
    assert!(result.get("delegation_id").is_none());
    assert!(result.get("child_session_id").is_none());
    assert!(result.get("changed_files").is_none());
    assert!(result.get("commands_run").is_none());

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn delegation_routes_reject_wrong_parent() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let unrelated_parent_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "prompt": "Parent-scoped route check",
        "title": "Scoped Delegation",
        "mode": "reviewer",
        "writePolicy": { "kind": "readOnly" }
    }))
    .expect("delegation request should serialize");

    let (create_status, created): (StatusCode, DelegationResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{parent_session_id}/delegations"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(create_status, StatusCode::CREATED);

    let wrong_parent_status_path = format!(
        "/api/sessions/{unrelated_parent_id}/delegations/{}",
        created.delegation.id
    );
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(wrong_parent_status_path)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "delegation not found");

    let wrong_parent_result_path = format!(
        "/api/sessions/{unrelated_parent_id}/delegations/{}/result",
        created.delegation.id
    );
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(wrong_parent_result_path)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "delegation not found");

    let wrong_parent_cancel_path = format!(
        "/api/sessions/{unrelated_parent_id}/delegations/{}/cancel",
        created.delegation.id
    );
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(wrong_parent_cancel_path)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "delegation not found");

    let (status, fetched): (StatusCode, DelegationStatusResponse) = request_json(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!(
                "/api/sessions/{parent_session_id}/delegations/{}",
                created.delegation.id
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched.delegation.status, DelegationStatus::Running);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_creation_dispatches_child_prompt_through_runtime_channel() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("delegation-dispatch-runtime");
    let parent_session_id = test_session_id(&state, Agent::Codex);

    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Inspect dispatch wiring.".to_owned(),
                title: Some("Dispatch Wiring".to_owned()),
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
        .expect("delegation child prompt should be delivered to runtime")
    {
        CodexRuntimeCommand::Prompt {
            session_id,
            command,
        } => {
            assert_eq!(session_id, created.delegation.child_session_id);
            assert_eq!(command.approval_policy, CodexApprovalPolicy::Never);
            assert_eq!(command.sandbox_mode, CodexSandboxMode::ReadOnly);
            assert_eq!(command.cwd, created.delegation.cwd);
            assert!(command.prompt.contains("Inspect dispatch wiring."));
        }
        _ => panic!("delegation should dispatch a Codex prompt command"),
    }

    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child session should exist");
    assert_eq!(child.session.status, SessionStatus::Active);
    assert!(matches!(child.runtime, SessionRuntime::Codex(_)));
    assert!(
        child.session.messages.iter().any(|message| matches!(
            message,
            Message::Text {
                author: Author::You,
                text,
                ..
            } if text.contains("Inspect dispatch wiring.")
        )),
        "production dispatch should append the child prompt message"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn isolated_worktree_delegation_materializes_dirty_state_and_uses_workspace_write() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("isolated-delegation-runtime");
    let unique = Uuid::new_v4();
    let repo_root = std::env::temp_dir().join(format!("termal-isolated-source-{unique}"));
    let worktree_root = std::env::temp_dir().join(format!("termal-isolated-child-{unique}"));
    fs::create_dir_all(&repo_root).expect("source repo root should be created");
    fs::write(repo_root.join("README.md"), "base\n").expect("base file should write");
    run_git_test_command(&repo_root, &["init"]);
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);
    fs::write(repo_root.join("README.md"), "staged\n").expect("staged content should write");
    run_git_test_command(&repo_root, &["add", "README.md"]);
    fs::write(repo_root.join("README.md"), "unstaged\n").expect("unstaged content should write");

    let parent_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Isolated Parent".to_owned()),
            repo_root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        state.commit_locked(&mut inner).unwrap();
        session_id
    };

    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Run build-gated review.".to_owned(),
                title: Some("Isolated Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::IsolatedWorktree {
                    owned_paths: vec!["README.md".to_owned()],
                    worktree_path: Some(worktree_root.to_string_lossy().into_owned()),
                }),
            },
        )
        .expect("isolated worktree delegation should be created");

    match input_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("delegation child prompt should be delivered to runtime")
    {
        CodexRuntimeCommand::Prompt {
            session_id,
            command,
        } => {
            assert_eq!(session_id, created.delegation.child_session_id);
            assert_eq!(command.approval_policy, CodexApprovalPolicy::Never);
            assert_eq!(command.sandbox_mode, CodexSandboxMode::WorkspaceWrite);
            assert_eq!(command.cwd, created.delegation.cwd);
            assert!(command.prompt.contains("Write policy: isolated worktree"));
            assert!(command.prompt.contains("README.md"));
        }
        _ => panic!("isolated delegation should dispatch a Codex prompt command"),
    }

    assert_eq!(created.child_session.project_id, None);
    assert_eq!(
        created.child_session.sandbox_mode,
        Some(CodexSandboxMode::WorkspaceWrite)
    );
    assert_eq!(
        fs::read_to_string(worktree_root.join("README.md"))
            .unwrap()
            .replace("\r\n", "\n"),
        "unstaged\n"
    );
    let staged_diff =
        run_git_test_command_output(&worktree_root, &["diff", "--cached", "--", "README.md"]);
    assert!(staged_diff.contains("-base"));
    assert!(staged_diff.contains("+staged"));
    let unstaged_diff = run_git_test_command_output(&worktree_root, &["diff", "--", "README.md"]);
    assert!(unstaged_diff.contains("-staged"));
    assert!(unstaged_diff.contains("+unstaged"));

    let _ = git_command()
        .arg("-C")
        .arg(&repo_root)
        .args(["worktree", "remove", "--force"])
        .arg(&worktree_root)
        .output();
    let _ = fs::remove_dir_all(&repo_root);
    let _ = fs::remove_dir_all(&worktree_root);
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn isolated_worktree_delegation_route_generates_termal_owned_path_when_omitted() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("generated-isolated-delegation-runtime");
    let unique = Uuid::new_v4();
    let repo_root = std::env::temp_dir().join(format!("termal-isolated-auto-source-{unique}"));
    fs::create_dir_all(&repo_root).expect("source repo root should be created");
    fs::write(repo_root.join("README.md"), "base\n").expect("base file should write");
    run_git_test_command(&repo_root, &["init"]);
    run_git_test_command(&repo_root, &["config", "user.email", "termal@example.com"]);
    run_git_test_command(&repo_root, &["config", "user.name", "TermAl"]);
    run_git_test_command(&repo_root, &["add", "README.md"]);
    run_git_test_command(&repo_root, &["commit", "-m", "init"]);
    fs::write(repo_root.join("README.md"), "changed\n").expect("changed file should write");

    let parent_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Auto Isolated Parent".to_owned()),
            repo_root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        state.commit_locked(&mut inner).unwrap();
        session_id
    };

    let app = app_router(state.clone());
    let body = serde_json::to_vec(&json!({
        "prompt": "Run generated isolated review.",
        "title": "Generated Isolated Review",
        "mode": "reviewer",
        "writePolicy": {
            "kind": "isolatedWorktree",
            "ownedPaths": []
        }
    }))
    .expect("delegation request should serialize");
    let (status, created): (StatusCode, DelegationResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{parent_session_id}/delegations"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    let _ = input_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("delegation child prompt should be delivered to runtime");

    let DelegationWritePolicy::IsolatedWorktree { worktree_path, .. } =
        &created.delegation.write_policy
    else {
        panic!("created delegation should preserve isolated write policy");
    };
    let worktree_path = worktree_path
        .as_ref()
        .expect("generated worktree path should be stored");
    assert!(worktree_path.contains(&created.delegation.id));
    assert_eq!(
        fs::read_to_string(FsPath::new(worktree_path).join("README.md"))
            .unwrap()
            .replace("\r\n", "\n"),
        "changed\n"
    );

    let _ = git_command()
        .arg("-C")
        .arg(&repo_root)
        .args(["worktree", "remove", "--force"])
        .arg(worktree_path)
        .output();
    let _ = fs::remove_dir_all(&repo_root);
    let _ = fs::remove_dir_all(
        FsPath::new(worktree_path)
            .parent()
            .unwrap_or_else(|| FsPath::new(worktree_path)),
    );
    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn terminal_delegation_child_dispatch_is_blocked_before_runtime_start() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("delegation-terminal-dispatch");
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let (delegation_id, child_session_id) = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let delegation_id = inner.next_delegation_id();
        let child_record = inner.create_session(
            Agent::Codex,
            Some("Canceled Delegation Child".to_owned()),
            "/tmp".to_owned(),
            None,
            None,
        );
        let child_session_id = child_record.session.id.clone();
        let child_index = inner
            .find_session_index(&child_session_id)
            .expect("just-created child session should be indexed");
        let child = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");
        child.codex_approval_policy = CodexApprovalPolicy::Never;
        child.codex_sandbox_mode = CodexSandboxMode::ReadOnly;
        child.session.approval_policy = Some(CodexApprovalPolicy::Never);
        child.session.sandbox_mode = Some(CodexSandboxMode::ReadOnly);
        child.session.parent_delegation_id = Some(delegation_id.clone());

        let now = stamp_now();
        inner.delegations.push(DelegationRecord {
            id: delegation_id.clone(),
            parent_session_id: parent_session_id.clone(),
            child_session_id: child_session_id.clone(),
            mode: DelegationMode::Reviewer,
            status: DelegationStatus::Canceled,
            title: "Canceled delegation".to_owned(),
            prompt: "Do not dispatch".to_owned(),
            cwd: "/tmp".to_owned(),
            agent: Agent::Codex,
            model: Some(Agent::Codex.default_model().to_owned()),
            write_policy: DelegationWritePolicy::ReadOnly,
            created_at: now.clone(),
            started_at: Some(now.clone()),
            completed_at: Some(now),
            result: None,
        });
        state.commit_locked(&mut inner).unwrap();
        (delegation_id, child_session_id)
    };

    let err = match state.dispatch_turn(
        &child_session_id,
        SendMessageRequest {
            text: "This prompt should not reach the runtime.".to_owned(),
            expanded_text: None,
            attachments: Vec::new(),
        },
    ) {
        Ok(_) => panic!("terminal delegated child dispatch should be rejected"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::CONFLICT);
    assert_eq!(err.message, DELEGATION_NO_LONGER_STARTABLE_MESSAGE);
    assert!(
        matches!(input_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "blocked terminal delegation dispatch must not reach the runtime"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == child_session_id)
        .expect("child should still exist");
    assert!(matches!(child.runtime, SessionRuntime::None));
    assert!(child.session.messages.is_empty());
    assert!(inner
        .delegations
        .iter()
        .any(|record| record.id == delegation_id
            && record.status == DelegationStatus::Canceled));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn create_delegation_terminalized_before_start_does_not_dispatch_child_prompt() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("delegation-create-start-race");
    let parent_session_id = test_session_id(&state, Agent::Codex);

    let response = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: format!("Simulate race. {TEST_CANCEL_DELEGATION_BEFORE_START_PROMPT}"),
                title: Some("Create Start Race".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("terminalized delegation create should return current state");

    assert_eq!(response.delegation.status, DelegationStatus::Canceled);
    assert!(
        matches!(input_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "terminalized create/start race must not dispatch a child runtime prompt"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == response.delegation.child_session_id)
        .expect("child session should remain visible");
    assert!(matches!(child.runtime, SessionRuntime::None));
    assert!(child.session.messages.is_empty());
    assert_eq!(child.session.status, SessionStatus::Idle);
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegated_child_completion_refreshes_through_production_lifecycle_hook() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Complete through lifecycle.".to_owned(),
                title: Some("Lifecycle Completion".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let mut delta_events = state.subscribe_delta_events();
    let child_token = runtime_token_for_session(&state, &created.delegation.child_session_id);
    push_delegation_child_assistant_text_without_finishing(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nLifecycle hook completed.",
    );

    state
        .finish_turn_ok_if_runtime_matches(&created.delegation.child_session_id, &child_token)
        .expect("production completion lifecycle should succeed");

    let mut saw_completed_delta = false;
    let mut saw_parent_update = false;
    while let Ok(payload) = delta_events.try_recv() {
        let event: DeltaEvent =
            serde_json::from_str(&payload).expect("delta event should deserialize");
        saw_completed_delta |= matches!(
            &event,
            DeltaEvent::DelegationCompleted {
                delegation_id,
                result,
                ..
            } if delegation_id == &created.delegation.id
                && result.summary.contains("Lifecycle hook completed")
        );
        saw_parent_update |= matches!(
            event,
            DeltaEvent::ParallelAgentsUpdate {
                session_id,
                agents,
                ..
            } if session_id == parent_session_id
                && agents.iter().any(|agent|
                    agent.id == created.delegation.id
                        && agent.source == ParallelAgentSource::Delegation
                        && agent.status == ParallelAgentStatus::Completed
                )
        );
    }
    assert!(
        saw_completed_delta,
        "completion hook should publish delegation completion"
    );
    assert!(
        saw_parent_update,
        "completion hook should update the parent card"
    );

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
        Some("Lifecycle hook completed.")
    );
    assert_single_parent_delegation_agent_status(
        &inner,
        &parent_session_id,
        &created.delegation.id,
        ParallelAgentStatus::Completed,
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegated_child_failure_refreshes_through_production_lifecycle_hook() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Fail through lifecycle.".to_owned(),
                title: Some("Lifecycle Failure".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let mut delta_events = state.subscribe_delta_events();
    let child_token = runtime_token_for_session(&state, &created.delegation.child_session_id);

    state
        .fail_turn_if_runtime_matches(
            &created.delegation.child_session_id,
            &child_token,
            "delegated child failed through lifecycle",
        )
        .expect("production failure lifecycle should succeed");

    let mut saw_failed_delta = false;
    let mut saw_parent_update = false;
    while let Ok(payload) = delta_events.try_recv() {
        let event: DeltaEvent =
            serde_json::from_str(&payload).expect("delta event should deserialize");
        saw_failed_delta |= matches!(
            &event,
            DeltaEvent::DelegationFailed {
                delegation_id,
                result,
                ..
            } if delegation_id == &created.delegation.id
                && result.summary.contains("delegated child failed")
        );
        saw_parent_update |= matches!(
            event,
            DeltaEvent::ParallelAgentsUpdate {
                session_id,
                agents,
                ..
            } if session_id == parent_session_id
                && agents.iter().any(|agent|
                    agent.id == created.delegation.id
                        && agent.status == ParallelAgentStatus::Error
                )
        );
    }
    assert!(
        saw_failed_delta,
        "failure hook should publish delegation failure"
    );
    assert!(
        saw_parent_update,
        "failure hook should update the parent card"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|record| record.id == created.delegation.id)
        .expect("delegation should exist");
    assert_eq!(delegation.status, DelegationStatus::Failed);
    assert!(
        delegation
            .result
            .as_ref()
            .is_some_and(|result| result.summary.contains("delegated child failed"))
    );
    assert!(parent_delegation_card_has_status(
        &inner,
        &parent_session_id,
        &created.delegation.id,
        ParallelAgentStatus::Error,
    ));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegated_child_runtime_exit_refreshes_through_production_lifecycle_hook() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Exit through lifecycle.".to_owned(),
                title: Some("Lifecycle Exit".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let mut delta_events = state.subscribe_delta_events();
    let child_token = runtime_token_for_session(&state, &created.delegation.child_session_id);

    state
        .handle_runtime_exit_if_matches(
            &created.delegation.child_session_id,
            &child_token,
            Some("delegated child runtime exited"),
        )
        .expect("production runtime-exit lifecycle should succeed");

    let mut saw_failed_delta = false;
    let mut saw_parent_update = false;
    while let Ok(payload) = delta_events.try_recv() {
        let event: DeltaEvent =
            serde_json::from_str(&payload).expect("delta event should deserialize");
        saw_failed_delta |= matches!(
            &event,
            DeltaEvent::DelegationFailed {
                delegation_id,
                result,
                ..
            } if delegation_id == &created.delegation.id
                && result.summary.contains("delegated child runtime exited")
        );
        saw_parent_update |= matches!(
            event,
            DeltaEvent::ParallelAgentsUpdate {
                session_id,
                agents,
                ..
            } if session_id == parent_session_id
                && agents.iter().any(|agent|
                    agent.id == created.delegation.id
                        && agent.status == ParallelAgentStatus::Error
                )
        );
    }
    assert!(
        saw_failed_delta,
        "runtime-exit hook should publish delegation failure"
    );
    assert!(
        saw_parent_update,
        "runtime-exit hook should update the parent card"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|record| record.id == created.delegation.id)
        .expect("delegation should exist");
    assert_eq!(delegation.status, DelegationStatus::Failed);
    assert!(
        delegation
            .result
            .as_ref()
            .is_some_and(|result| result.summary.contains("delegated child runtime exited"))
    );
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child session should exist");
    assert!(matches!(child.runtime, SessionRuntime::None));
    assert!(parent_delegation_card_has_status(
        &inner,
        &parent_session_id,
        &created.delegation.id,
        ParallelAgentStatus::Error,
    ));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn production_completion_clears_queued_child_prompt_before_dispatch() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("delegation-queued-complete");
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Complete with queued child prompt.".to_owned(),
                title: Some("Queued Completion".to_owned()),
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

    queue_delegation_child_prompt(
        &state,
        &created.delegation.child_session_id,
        "queued prompt must not run after terminal result",
    );
    let child_token = runtime_token_for_session(&state, &created.delegation.child_session_id);
    push_delegation_child_assistant_text_without_finishing(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nQueued completion done.",
    );

    state
        .finish_turn_ok_if_runtime_matches(&created.delegation.child_session_id, &child_token)
        .expect("production completion lifecycle should succeed");
    assert!(
        matches!(input_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "terminal completion should clear queued child prompts before dispatch"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|record| record.id == created.delegation.id)
        .expect("delegation should exist");
    assert_eq!(delegation.status, DelegationStatus::Completed);
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child session should exist");
    assert!(child.queued_prompts.is_empty());
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn production_failure_clears_queued_child_prompt_before_dispatch() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("delegation-queued-failure");
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Fail with queued child prompt.".to_owned(),
                title: Some("Queued Failure".to_owned()),
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

    queue_delegation_child_prompt(
        &state,
        &created.delegation.child_session_id,
        "queued prompt must not run after failed result",
    );
    let child_token = runtime_token_for_session(&state, &created.delegation.child_session_id);

    state
        .fail_turn_if_runtime_matches(
            &created.delegation.child_session_id,
            &child_token,
            "queued failure should terminalize",
        )
        .expect("production failure lifecycle should succeed");
    assert!(
        matches!(input_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "terminal failure should clear queued child prompts before dispatch"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|record| record.id == created.delegation.id)
        .expect("delegation should exist");
    assert_eq!(delegation.status, DelegationStatus::Failed);
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child session should exist");
    assert!(child.queued_prompts.is_empty());
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn delegation_route_json_rejections_use_api_error_shape() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());

    for body in [
        r#"{}"#,
        r#"{"prompt":"valid shape but no content-type"}"#,
        r#"{"prompt":"unterminated"#,
    ] {
        let mut builder = Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{parent_session_id}/delegations"));
        if body != r#"{"prompt":"valid shape but no content-type"}"# {
            builder = builder.header("content-type", "application/json");
        }
        let (status, response): (StatusCode, Value) =
            request_json(&app, builder.body(Body::from(body)).unwrap()).await;

        if body == r#"{"prompt":"valid shape but no content-type"}"# {
            assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        } else {
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        }
        assert!(
            response["error"]
                .as_str()
                .expect("error response should include a message")
                .contains("invalid delegation request JSON")
        );
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_result_is_derived_from_completed_child_session() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Summarize the current test shape.".to_owned(),
                title: Some("Result Review".to_owned()),
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
        let running_command_id = inner.next_message_id();
        let success_command_id = inner.next_message_id();
        let error_command_id = inner.next_message_id();
        let message_id = inner.next_message_id();
        let child = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");
        push_message_on_record(
            child,
            Message::Command {
                id: running_command_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                command: "cargo check".to_owned(),
                command_language: Some("shell".to_owned()),
                output: String::new(),
                output_language: Some("text".to_owned()),
                status: CommandStatus::Running,
            },
        );
        push_message_on_record(
            child,
            Message::Command {
                id: success_command_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                command: "cargo test delegations".to_owned(),
                command_language: Some("shell".to_owned()),
                output: "ok".to_owned(),
                output_language: Some("text".to_owned()),
                status: CommandStatus::Success,
            },
        );
        push_message_on_record(
            child,
            Message::Command {
                id: error_command_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                command: "false".to_owned(),
                command_language: Some("shell".to_owned()),
                output: "failed".to_owned(),
                output_language: Some("text".to_owned()),
                status: CommandStatus::Error,
            },
        );
        push_message_on_record(
            child,
            Message::Text {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "## Result\n\nStatus: completed\n\nSummary:\nNo issues found.".to_owned(),
                expanded_text: None,
            },
        );
        child.session.status = SessionStatus::Idle;
        child.session.preview = "No issues found.".to_owned();
        state.commit_locked(&mut inner).unwrap();
    }

    let response = state
        .get_delegation_result(&parent_session_id, &created.delegation.id)
        .expect("result polling should refresh completed child state");
    assert_eq!(response.result.status, DelegationStatus::Completed);
    assert!(response.result.summary.contains("No issues found"));
    assert_eq!(
        response
            .result
            .commands_run
            .iter()
            .map(|command| command.status.as_str())
            .collect::<Vec<_>>(),
        ["running", "success", "error"]
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|record| record.id == created.delegation.id)
        .expect("delegation should still exist");
    assert_eq!(delegation.status, DelegationStatus::Completed);
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should still exist");
    assert!(
        parent.session.messages.iter().any(|message| matches!(
            message,
            Message::ParallelAgents { agents, .. }
                if agents.iter().any(|agent|
                    agent.id == created.delegation.id
                        && agent.source == ParallelAgentSource::Delegation
                        && agent.status == ParallelAgentStatus::Completed
                )
        )),
        "parent delegation card should reflect completion"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_result_completion_clears_child_follow_up_queue() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Finish before queued child follow-up.".to_owned(),
                title: Some("Queued Follow-up".to_owned()),
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
        inner.sessions[child_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::User,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-child-follow-up".to_owned(),
                    timestamp: stamp_now(),
                    text: "unrelated follow-up".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[child_index]);
    }

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nDelegated turn done.",
    );
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete despite queued follow-up");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|record| record.id == created.delegation.id)
        .expect("delegation should still exist");
    assert_eq!(delegation.status, DelegationStatus::Completed);
    assert_eq!(
        delegation
            .result
            .as_ref()
            .map(|result| result.summary.as_str()),
        Some("Delegated turn done.")
    );
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child should still exist");
    assert!(
        child.queued_prompts.is_empty(),
        "terminal delegation refresh should not leave queued child prompts dispatchable"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_failed_result_clears_child_follow_up_queue() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Fail before queued child follow-up.".to_owned(),
                title: Some("Queued Failed Follow-up".to_owned()),
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
        inner.sessions[child_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::User,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-child-follow-up".to_owned(),
                    timestamp: stamp_now(),
                    text: "do not dispatch after failure".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[child_index]);
    }

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: failed\n\nSummary:\nDelegated turn failed.",
    );
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should fail delegation despite queued follow-up");

    let inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|record| record.id == created.delegation.id)
        .expect("delegation should still exist");
    assert_eq!(delegation.status, DelegationStatus::Failed);
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child should still exist");
    assert!(
        child.queued_prompts.is_empty(),
        "failed delegation refresh should not leave queued child prompts dispatchable"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_parent_card_changes_emit_transcript_deltas() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let mut delta_events = state.subscribe_delta_events();
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Summarize parent-card live updates.".to_owned(),
                title: Some("Parent Card Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let mut parent_message_id = None;
    while let Ok(payload) = delta_events.try_recv() {
        let event: DeltaEvent =
            serde_json::from_str(&payload).expect("delta event should deserialize");
        if let DeltaEvent::MessageCreated {
            session_id,
            message_id,
            message,
            session_mutation_stamp,
            ..
        } = event
        {
            if session_id == parent_session_id {
                assert!(session_mutation_stamp.is_some());
                match message {
                    Message::ParallelAgents { agents, .. } => {
                        assert!(agents.iter().any(|agent| {
                            agent.id == created.delegation.id
                                && agent.status == ParallelAgentStatus::Running
                        }));
                    }
                    _ => panic!("parent card should be a parallel-agents message"),
                }
                parent_message_id = Some(message_id);
                break;
            }
        }
    }
    let parent_message_id =
        parent_message_id.expect("parent MessageCreated delta should be published");

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nReady.",
    );
    let _ = state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete");
    let mut saw_parent_update = false;
    while let Ok(payload) = delta_events.try_recv() {
        let event: DeltaEvent =
            serde_json::from_str(&payload).expect("delta event should deserialize");
        saw_parent_update |= matches!(
            event,
            DeltaEvent::ParallelAgentsUpdate {
                session_id,
                message_id,
                agents,
                session_mutation_stamp,
                ..
            } if session_id == parent_session_id
                && message_id == parent_message_id
                && session_mutation_stamp.is_some()
                && agents.iter().any(|agent|
                    agent.id == created.delegation.id
                        && agent.status == ParallelAgentStatus::Completed
                )
        );
    }
    assert!(
        saw_parent_update,
        "parent ParallelAgentsUpdate delta should be published"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_limit_allows_new_children_after_terminal_states() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let mut created = Vec::new();
    for index in 0..MAX_RUNNING_DELEGATIONS_PER_PARENT {
        created.push(
            state
                .create_read_only_delegation(
                    &parent_session_id,
                    CreateDelegationRequest {
                        prompt: format!("Create child {index}."),
                        title: Some(format!("Limited Child {index}")),
                        cwd: None,
                        agent: Some(Agent::Codex),
                        model: None,
                        mode: Some(DelegationMode::Reviewer),
                        write_policy: Some(DelegationWritePolicy::ReadOnly),
                    },
                )
                .expect("delegation under limit should be created"),
        );
    }

    let err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: "This should exceed the active limit.".to_owned(),
            title: Some("Too Many".to_owned()),
            cwd: None,
            agent: Some(Agent::Codex),
            model: None,
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("fifth active delegation should be rejected"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::CONFLICT);
    assert!(
        err.message
            .contains("parent session already has 4 active delegations")
    );

    finish_delegation_child_with_assistant_text(
        &state,
        &created[0].delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nFreed capacity.",
    );
    state
        .refresh_delegation_for_child_session(&created[0].delegation.child_session_id)
        .expect("completed delegation should refresh");
    state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Completion should free one slot.".to_owned(),
                title: Some("After Completion".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("completed delegation should not count against active limit");

    state
        .cancel_delegation(&parent_session_id, &created[1].delegation.id)
        .expect("running delegation should cancel");
    state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Cancellation should free one slot.".to_owned(),
                title: Some("After Cancellation".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("canceled delegation should not count against active limit");

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_nesting_depth_rejects_fourth_generation_child() {
    let state = test_app_state();
    let root_session_id = test_session_id(&state, Agent::Codex);
    let first = state
        .create_read_only_delegation(
            &root_session_id,
            CreateDelegationRequest {
                prompt: "Create depth one.".to_owned(),
                title: Some("Depth One".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("first generation should be created");
    let second = state
        .create_read_only_delegation(
            &first.delegation.child_session_id,
            CreateDelegationRequest {
                prompt: "Create depth two.".to_owned(),
                title: Some("Depth Two".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("second generation should be created");
    let third = state
        .create_read_only_delegation(
            &second.delegation.child_session_id,
            CreateDelegationRequest {
                prompt: "Create depth three.".to_owned(),
                title: Some("Depth Three".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("third generation should be created");

    let err = match state.create_read_only_delegation(
        &third.delegation.child_session_id,
        CreateDelegationRequest {
            prompt: "Create depth four.".to_owned(),
            title: Some("Depth Four".to_owned()),
            cwd: None,
            agent: Some(Agent::Codex),
            model: None,
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("fourth generation should exceed nesting depth"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::CONFLICT);
    assert!(
        err.message
            .contains("delegation nesting depth is limited to 3")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn failed_delegation_start_keeps_child_session_as_error_transcript() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: format!("{TEST_FORCE_DELEGATION_START_FAILURE_PROMPT}\nforce failure"),
                title: Some("Failing Delegation".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("start failure should return a failed delegation response");

    assert_eq!(created.delegation.status, DelegationStatus::Failed);
    assert_eq!(created.child_session.status, SessionStatus::Error);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("failed start response should reference a durable child session");
    assert_eq!(child.session.status, SessionStatus::Error);
    assert!(child.queued_prompts.is_empty());
    let stored = inner
        .delegations
        .iter()
        .find(|delegation| delegation.id == created.delegation.id)
        .expect("failed delegation record should remain");
    assert_eq!(stored.status, DelegationStatus::Failed);
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should still exist");
    assert!(
        parent.session.messages.iter().any(|message| matches!(
            message,
            Message::ParallelAgents { agents, .. }
                if agents.iter().any(|agent|
                    agent.id == created.delegation.id
                        && agent.status == ParallelAgentStatus::Error
                )
        )),
        "parent delegation card should show the failed start"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn delegation_route_failed_start_response_matches_durable_child_state() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());
    let (status, response): (StatusCode, DelegationResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{parent_session_id}/delegations"))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "prompt": format!("{TEST_FORCE_DELEGATION_START_FAILURE_PROMPT}\nforce failure"),
                    "title": "Route Failed Start",
                    "mode": "reviewer",
                    "writePolicy": { "kind": "readOnly" }
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(response.delegation.status, DelegationStatus::Failed);
    assert_eq!(response.child_session.status, SessionStatus::Error);
    assert_eq!(
        response.child_session.id,
        response.delegation.child_session_id
    );
    assert!(
        response
            .child_session
            .preview
            .contains("child session failed to start")
    );
    assert_eq!(
        response
            .delegation
            .result
            .as_ref()
            .map(|result| result.summary.as_str()),
        Some("child session failed to start")
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(response.revision, inner.revision);
    let stored_delegation = inner
        .delegations
        .iter()
        .find(|delegation| delegation.id == response.delegation.id)
        .expect("failed delegation should be durable");
    assert_eq!(stored_delegation, &response.delegation);
    let stored_child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == response.delegation.child_session_id)
        .expect("failed child session should be durable");
    assert_eq!(stored_child.session.status, response.child_session.status);
    assert_eq!(stored_child.session.preview, response.child_session.preview);
    assert_eq!(
        stored_child.session.parent_delegation_id.as_deref(),
        Some(response.delegation.id.as_str())
    );
    assert!(stored_child.queued_prompts.is_empty());
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_creation_publishes_parent_card_message_delta() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let mut delta_events = state.subscribe_delta_events();
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Publish parent card delta.".to_owned(),
                title: Some("Parent Delta".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let mut saw_parent_card_delta = false;
    while let Ok(payload) = delta_events.try_recv() {
        let event: DeltaEvent =
            serde_json::from_str(&payload).expect("delta event should deserialize");
        if let DeltaEvent::MessageCreated {
            session_id,
            message,
            ..
        } = event
        {
            // OR-coalesce so an unrelated MessageCreated arriving after the
            // parent-card delta does not flip the assertion back to false.
            saw_parent_card_delta = saw_parent_card_delta
                || (session_id == parent_session_id
                    && matches!(
                        message,
                        Message::ParallelAgents { agents, .. }
                            if agents.iter().any(|agent| agent.id == created.delegation.id)
                    ));
        }
    }

    assert!(saw_parent_card_delta);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_status_get_refreshes_child_state() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Read current state only.".to_owned(),
                title: Some("Read-only status".to_owned()),
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
        "## Result\n\nStatus: completed\n\nSummary:\nReady.",
    );
    let revision_before_get = {
        let inner = state.inner.lock().expect("state mutex poisoned");
        inner.revision
    };

    let response = state
        .get_delegation(&parent_session_id, &created.delegation.id)
        .expect("status should be readable");

    assert!(
        response.revision > revision_before_get,
        "polling status should persist the terminal child refresh"
    );
    assert_eq!(response.delegation.status, DelegationStatus::Completed);
    assert!(response.delegation.result.is_some());

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_status_poll_consumes_satisfied_wait_for_busy_parent() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Return a result packet.".to_owned(),
                title: Some("Polling Review".to_owned()),
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
                title: Some("Poll fan-in".to_owned()),
            },
        )
        .expect("running delegation wait should be accepted");
    assert!(!wait.resume_prompt_queued);
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

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nPolling should observe this result.",
    );

    let response = state
        .get_delegation(&parent_session_id, &created.delegation.id)
        .expect("status polling should refresh completed child state");

    assert_eq!(response.delegation.status, DelegationStatus::Completed);
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.delegation_waits.is_empty());
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should exist");
    assert_eq!(parent.session.status, SessionStatus::Active);
    assert_eq!(parent.queued_prompts.len(), 1);
    let resume_prompt = &parent.queued_prompts[0].pending_prompt.text;
    assert!(resume_prompt.contains("Poll fan-in"));
    assert!(resume_prompt.contains("Polling should observe this result."));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_result_poll_consumes_satisfied_wait_for_busy_parent() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Return a result packet.".to_owned(),
                title: Some("Result Polling Review".to_owned()),
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
                title: Some("Result poll fan-in".to_owned()),
            },
        )
        .expect("running delegation wait should be accepted");
    assert!(!wait.resume_prompt_queued);
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

    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nResult polling should observe this result.",
    );

    let response = state
        .get_delegation_result(&parent_session_id, &created.delegation.id)
        .expect("result polling should refresh completed child state");

    assert_eq!(response.result.status, DelegationStatus::Completed);
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(inner.delegation_waits.is_empty());
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should exist");
    assert_eq!(parent.session.status, SessionStatus::Active);
    assert_eq!(parent.queued_prompts.len(), 1);
    let resume_prompt = &parent.queued_prompts[0].pending_prompt.text;
    assert!(resume_prompt.contains("Result poll fan-in"));
    assert!(resume_prompt.contains("Result polling should observe this result."));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_status_poll_does_not_consume_unrelated_satisfied_wait() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let polled = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Poll this delegation.".to_owned(),
                title: Some("Polled Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("polled delegation should be created");
    let unrelated = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Leave this wait pending.".to_owned(),
                title: Some("Unrelated Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("unrelated delegation should be created");

    let unrelated_wait = state
        .create_delegation_wait(
            &parent_session_id,
            CreateDelegationWaitRequest {
                delegation_ids: vec![unrelated.delegation.id.clone()],
                mode: DelegationWaitMode::All,
                title: Some("Unrelated fan-in".to_owned()),
            },
        )
        .expect("running unrelated wait should be accepted");

    finish_delegation_child_with_assistant_text(
        &state,
        &unrelated.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nUnrelated wait is satisfied.",
    );
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_delegation_index(&unrelated.delegation.id)
            .expect("unrelated delegation should exist");
        assert!(refresh_delegation_from_child_locked(&mut inner, index).is_some());
        state
            .commit_locked(&mut inner)
            .expect("manual refresh should persist");
    }
    finish_delegation_child_with_assistant_text(
        &state,
        &polled.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nPolled delegation is complete.",
    );

    let response = state
        .get_delegation(&parent_session_id, &polled.delegation.id)
        .expect("status polling should refresh the polled delegation");

    assert_eq!(response.delegation.status, DelegationStatus::Completed);
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(inner.delegation_waits.len(), 1);
    assert_eq!(inner.delegation_waits[0].id, unrelated_wait.wait.id);
    let parent = inner
        .sessions
        .iter()
        .find(|record| record.session.id == parent_session_id)
        .expect("parent should exist");
    assert!(parent.queued_prompts.is_empty());

    let _ = fs::remove_file(state.persistence_path.as_path());
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
fn delegation_result_packet_accepts_preamble_and_case_drift() {
    let parsed = parse_delegation_result_packet(
        "Done, here is the packet:\n\n## result\n\nstatus: completed\n\nsummary:\nReady.",
    )
    .expect("packet with preamble and lowercase labels should parse");
    assert_eq!(parsed.status, DelegationStatus::Completed);
    assert_eq!(parsed.summary, "Ready.");

    let parsed = parse_delegation_result_packet("## RESULT\n\nSTATUS: failed\n\nSummary:\nNope.")
        .expect("uppercase status label should parse");
    assert_eq!(parsed.status, DelegationStatus::Failed);
    assert_eq!(parsed.summary, "Nope.");
}

#[test]
fn delegation_result_packet_summary_allows_colon_terminated_text_lines() {
    let parsed = parse_delegation_result_packet(
        "## Result\n\nStatus: completed\n\nSummary:\nThe issue is here:\n  detail\n\nNotes:\nignored",
    )
    .expect("summary text ending in colon should not terminate the summary");

    assert_eq!(parsed.status, DelegationStatus::Completed);
    assert_eq!(parsed.summary, "The issue is here:\n  detail");
}

#[test]
fn delegation_result_packet_summary_preserves_status_labeled_text() {
    let parsed = parse_delegation_result_packet(
        "## Result\n\nStatus: completed\n\nSummary:\nStatus: the inspected path is stable.\nNo changes needed.",
    )
    .expect("summary text containing Status: should not reset packet metadata");

    assert_eq!(parsed.status, DelegationStatus::Completed);
    assert_eq!(
        parsed.summary,
        "Status: the inspected path is stable.\nNo changes needed."
    );
}

#[test]
fn delegation_result_packet_parses_findings_notes_and_inspected_files() {
    let parsed = parse_delegation_result_packet(
        "## Result\n\nStatus: completed\n\nSummary:\nReady.\n\nFindings:\n- High src/delegations.rs:1413 - Resume prompt drops findings.\n- Note docs/features/agent-delegation-sessions.md - Document the fan-in path.\n\nNotes:\n- Checked backend wait dispatch.\n\nFiles Inspected:\n- src/delegations.rs\n- ui/src/delegation-commands.ts",
    )
    .expect("packet with findings and notes should parse");

    assert_eq!(parsed.status, DelegationStatus::Completed);
    assert_eq!(parsed.summary, "Ready.");
    assert_eq!(
        parsed.findings,
        vec![
            DelegationFinding {
                severity: "High".to_owned(),
                file: Some("src/delegations.rs".to_owned()),
                line: Some(1413),
                message: "Resume prompt drops findings.".to_owned(),
            },
            DelegationFinding {
                severity: "Note".to_owned(),
                file: Some("docs/features/agent-delegation-sessions.md".to_owned()),
                line: None,
                message: "Document the fan-in path.".to_owned(),
            },
        ]
    );
    assert_eq!(
        parsed.notes,
        vec![
            "Checked backend wait dispatch.".to_owned(),
            "Inspected src/delegations.rs".to_owned(),
            "Inspected ui/src/delegation-commands.ts".to_owned(),
        ]
    );
}

#[test]
fn delegation_result_packet_drops_trailing_colon_from_invalid_finding_line() {
    let parsed = parse_delegation_result_packet(
        "## Result\n\nStatus: completed\n\nSummary:\nReady.\n\nFindings:\n- Low src/foo.rs: - Missing line number.",
    )
    .expect("packet should parse finding with invalid line suffix");

    assert_eq!(
        parsed.findings,
        vec![DelegationFinding {
            severity: "Low".to_owned(),
            file: Some("src/foo.rs".to_owned()),
            line: None,
            message: "Missing line number.".to_owned(),
        }]
    );
}

#[test]
fn delegation_result_packet_filters_none_findings() {
    let parsed = parse_delegation_result_packet(
        "## Result\n\nStatus: completed\n\nSummary:\nReady.\n\nFindings:\n- None",
    )
    .expect("packet with explicit no-findings marker should parse");

    assert!(parsed.findings.is_empty());
}

#[test]
fn delegation_result_packet_no_separator_finding_uses_note_fallback() {
    let parsed = parse_delegation_result_packet(
        "## Result\n\nStatus: completed\n\nSummary:\nReady.\n\nFindings:\n- Missing separator but still useful.",
    )
    .expect("packet with fallback finding should parse");

    assert_eq!(
        parsed.findings,
        vec![DelegationFinding {
            severity: "Note".to_owned(),
            file: None,
            line: None,
            message: "Missing separator but still useful.".to_owned(),
        }]
    );
}

#[test]
fn delegation_result_packet_parses_multi_word_finding_severity() {
    let parsed = parse_delegation_result_packet(
        "## Result\n\nStatus: completed\n\nSummary:\nReady.\n\nFindings:\n- Code Style src/foo.rs:42 - Use repo formatting.",
    )
    .expect("packet with multi-word finding severity should parse");

    assert_eq!(
        parsed.findings,
        vec![DelegationFinding {
            severity: "Code Style".to_owned(),
            file: Some("src/foo.rs".to_owned()),
            line: Some(42),
            message: "Use repo formatting.".to_owned(),
        }]
    );
}

#[test]
fn delegation_result_packet_parses_multi_word_severity_with_backticked_location() {
    let parsed = parse_delegation_result_packet(
        "## Result\n\nStatus: completed\n\nSummary:\nReady.\n\nFindings:\n- Code Style `src/foo.rs:42` - Use repo formatting.",
    )
    .expect("packet with backticked multi-word finding location should parse");

    assert_eq!(
        parsed.findings,
        vec![DelegationFinding {
            severity: "Code Style".to_owned(),
            file: Some("src/foo.rs".to_owned()),
            line: Some(42),
            message: "Use repo formatting.".to_owned(),
        }]
    );
}

#[test]
fn delegation_public_result_summary_is_capped() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Return a long result.".to_owned(),
                title: Some("Long Summary".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let long_summary = "x".repeat(MAX_DELEGATION_PUBLIC_SUMMARY_CHARS + 128);
    finish_delegation_child_with_assistant_text(
        &state,
        &created.delegation.child_session_id,
        &format!("## Result\n\nStatus: completed\n\nSummary:\n{long_summary}"),
    );
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete");

    let result = state
        .get_delegation_result(&parent_session_id, &created.delegation.id)
        .expect("full result should be available");
    assert_eq!(result.result.summary, long_summary);

    let snapshot = state.snapshot();
    let public_summary = snapshot
        .delegations
        .iter()
        .find(|delegation| delegation.id == created.delegation.id)
        .and_then(|delegation| delegation.result.as_ref())
        .map(|result| result.summary.as_str())
        .expect("public result summary should be present");
    assert_eq!(
        public_summary.chars().count(),
        MAX_DELEGATION_PUBLIC_SUMMARY_CHARS + 3
    );
    assert!(public_summary.ends_with("..."));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn cancel_preserves_completed_delegation_result() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Finish before cancel.".to_owned(),
                title: Some("Cancel Race".to_owned()),
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
        "## Result\n\nStatus: completed\n\nSummary:\nAlready done.",
    );
    let mut delta_events = state.subscribe_delta_events();
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should terminalize delegation before cancel");
    let mut saw_completed_delta = false;
    while let Ok(payload) = delta_events.try_recv() {
        let Ok(event) = serde_json::from_str::<DeltaEvent>(&payload) else {
            continue;
        };
        if matches!(
            event,
            DeltaEvent::DelegationCompleted {
                delegation_id,
                ..
            } if delegation_id == created.delegation.id
        ) {
            saw_completed_delta = true;
        }
    }
    assert!(
        saw_completed_delta,
        "refresh should publish the completed delegation delta before cancel"
    );
    let pre_cancel_revision = state.snapshot().revision;
    let response = state
        .cancel_delegation(&parent_session_id, &created.delegation.id)
        .expect("cancel should preserve completed state");

    assert_eq!(response.revision, pre_cancel_revision);
    assert_eq!(state.snapshot().revision, pre_cancel_revision);
    assert_eq!(response.delegation.status, DelegationStatus::Completed);
    assert_eq!(
        response
            .delegation
            .result
            .as_ref()
            .expect("completed result should be retained")
            .summary,
        "Already done."
    );
    while let Ok(payload) = delta_events.try_recv() {
        let Ok(event) = serde_json::from_str::<DeltaEvent>(&payload) else {
            continue;
        };
        assert!(
            !matches!(
                event,
                DeltaEvent::DelegationCreated { .. }
                    | DeltaEvent::DelegationUpdated { .. }
                    | DeltaEvent::DelegationCompleted { .. }
                    | DeltaEvent::DelegationFailed { .. }
                    | DeltaEvent::DelegationCanceled { .. }
            ),
            "terminal cancel should not publish delegation deltas"
        );
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn cancel_preserves_failed_delegation_result() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Fail before cancel.".to_owned(),
                title: Some("Failed Cancel".to_owned()),
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
        "## Result\n\nStatus: failed\n\nSummary:\nAlready failed.",
    );
    let response = state
        .cancel_delegation(&parent_session_id, &created.delegation.id)
        .expect("failed delegation cancel should return current terminal status");

    assert_eq!(response.delegation.status, DelegationStatus::Failed);
    assert_eq!(
        response
            .delegation
            .result
            .as_ref()
            .map(|result| result.summary.as_str()),
        Some("Already failed.")
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child session should exist");
    assert_eq!(child.session.status, SessionStatus::Error);
    assert!(parent_delegation_card_has_status(
        &inner,
        &parent_session_id,
        &created.delegation.id,
        ParallelAgentStatus::Error,
    ));
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn delegation_cancel_unknown_id_returns_not_found() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());

    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{parent_session_id}/delegations/missing-delegation/cancel"
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "delegation not found");

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn delegation_cancel_running_runtime_route_interrupts_child() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("delegation-route-cancel-runtime");
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Cancel while running.".to_owned(),
                title: Some("Running Cancel".to_owned()),
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

    let runtime = shared_codex_runtime_for_state(&state);
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            created.delegation.child_session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("delegation-cancel-thread".to_owned()),
                turn_id: Some("delegation-cancel-turn".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert(
            "delegation-cancel-thread".to_owned(),
            created.delegation.child_session_id.clone(),
        );

    let command_thread = std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("delegation cancel should interrupt the running child");
        match command {
            CodexRuntimeCommand::InterruptTurn {
                thread_id,
                turn_id,
                response_tx,
            } => {
                assert_eq!(thread_id, "delegation-cancel-thread");
                assert_eq!(turn_id, "delegation-cancel-turn");
                let _ = response_tx.send(Ok(()));
            }
            _ => panic!("expected child turn interrupt command"),
        }
    });

    let (status, response): (StatusCode, DelegationStatusResponse) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/sessions/{parent_session_id}/delegations/{}/cancel",
                created.delegation.id
            ))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    command_thread
        .join()
        .expect("delegation cancel command thread should join cleanly");

    assert_eq!(status, StatusCode::OK);
    assert_eq!(response.delegation.status, DelegationStatus::Canceled);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child session should exist");
    assert_eq!(child.session.status, SessionStatus::Idle);
    assert!(matches!(child.runtime, SessionRuntime::None));
    assert!(child.session.messages.iter().any(|message| matches!(
        message,
        Message::Text { text, .. } if text == "Turn stopped by user."
    )));
    drop(inner);
    assert!(
        !runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned")
            .contains_key(&created.delegation.child_session_id)
    );
    assert!(
        !runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("delegation-cancel-thread")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn mark_delegation_canceled_sets_child_session_idle() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Cancel through locked marker.".to_owned(),
                title: Some("Locked Cancel".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let mut inner = state.inner.lock().expect("state mutex poisoned");
    let delegation_index = inner
        .find_delegation_index(&created.delegation.id)
        .expect("delegation should exist");
    let child_index = inner
        .find_session_index(&created.delegation.child_session_id)
        .expect("child session should exist");
    inner
        .session_mut_by_index(child_index)
        .expect("child session index should be valid")
        .session
        .status = SessionStatus::Active;
    mark_delegation_canceled_locked(&mut inner, delegation_index, None)
        .expect("running delegation should transition to canceled");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child session should still exist");
    assert_eq!(child.session.status, SessionStatus::Idle);
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn failed_start_cleanup_is_noop_for_already_terminal_delegation() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Complete before a late start error.".to_owned(),
                title: Some("Late Start Error".to_owned()),
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
        "## Result\n\nStatus: completed\n\nSummary:\nAlready done.",
    );
    state
        .refresh_delegation_for_child_session(&created.delegation.child_session_id)
        .expect("refresh should complete delegation");
    let mut delta_events = state.subscribe_delta_events();
    let pre_error_revision = state.snapshot().revision;

    state
        .mark_delegation_failed_after_start_error(
            &created.delegation.id,
            &created.delegation.child_session_id,
            "late start error",
        )
        .expect("late failed-start cleanup should be a no-op");

    assert_eq!(state.snapshot().revision, pre_error_revision);
    assert!(
        delta_events.try_recv().is_err(),
        "no delegation delta should be published for terminal failed-start cleanup"
    );
    let inner = state.inner.lock().expect("state mutex poisoned");
    let stored = inner
        .delegations
        .iter()
        .find(|delegation| delegation.id == created.delegation.id)
        .expect("delegation record should remain");
    assert_eq!(stored.status, DelegationStatus::Completed);
    assert_eq!(
        stored
            .result
            .as_ref()
            .expect("completed result should remain")
            .summary,
        "Already done."
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn terminal_read_only_delegations_do_not_keep_child_session_write_blocked() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let completed = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Complete and unblock.".to_owned(),
                title: Some("Completed Delegation".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("completed delegation should be created");
    let canceled = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Cancel and unblock.".to_owned(),
                title: Some("Canceled Delegation".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("canceled delegation should be created");
    let failed = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: format!("{TEST_FORCE_DELEGATION_START_FAILURE_PROMPT}\nfail and unblock"),
                title: Some("Failed Delegation".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("failed delegation should still return a child session");
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let running_ids = inner
            .running_read_only_delegations
            .iter()
            .filter_map(|index| inner.delegations.get(*index))
            .map(|delegation| delegation.id.as_str())
            .collect::<BTreeSet<_>>();
        assert!(running_ids.contains(completed.delegation.id.as_str()));
        assert!(running_ids.contains(canceled.delegation.id.as_str()));
        assert!(!running_ids.contains(failed.delegation.id.as_str()));
    }

    finish_delegation_child_with_assistant_text(
        &state,
        &completed.delegation.child_session_id,
        "## Result\n\nStatus: completed\n\nSummary:\nDone.",
    );
    state
        .refresh_delegation_for_child_session(&completed.delegation.child_session_id)
        .expect("refresh should complete delegation");
    state
        .cancel_delegation(&parent_session_id, &canceled.delegation.id)
        .expect("cancel should terminalize delegation");

    for (child_session_id, name) in [
        (
            completed.delegation.child_session_id.as_str(),
            "completed child rename",
        ),
        (
            canceled.delegation.child_session_id.as_str(),
            "canceled child rename",
        ),
        (
            failed.delegation.child_session_id.as_str(),
            "failed child rename",
        ),
    ] {
        state
            .ensure_read_only_delegation_allows_session_write_action(
                Some(child_session_id),
                "session settings",
            )
            .expect("terminal delegation should not block child writes");
        state
            .update_session_settings(
                child_session_id,
                UpdateSessionSettingsRequest {
                    name: Some(name.to_owned()),
                    model: None,
                    approval_policy: None,
                    reasoning_effort: None,
                    sandbox_mode: None,
                    cursor_mode: None,
                    claude_approval_mode: None,
                    claude_effort: None,
                    gemini_approval_mode: None,
                },
            )
            .expect("terminal child session settings should update");
    }
    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner.running_read_only_delegations.is_empty(),
        "terminal delegation transitions should leave no running read-only index entries"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_cancel_clears_queued_child_prompts() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Queue then cancel.".to_owned(),
                title: Some("Cancel Queue".to_owned()),
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
        inner.sessions[child_index]
            .queued_prompts
            .push_back(QueuedPromptRecord {
                source: QueuedPromptSource::User,
                attachments: Vec::new(),
                pending_prompt: PendingPrompt {
                    attachments: Vec::new(),
                    id: "queued-child-follow-up".to_owned(),
                    timestamp: stamp_now(),
                    text: "do not dispatch after cancel".to_owned(),
                    expanded_text: None,
                },
            });
        sync_pending_prompts(&mut inner.sessions[child_index]);
    }

    let response = state
        .cancel_delegation(&parent_session_id, &created.delegation.id)
        .expect("cancel should terminalize delegation");

    assert_eq!(response.delegation.status, DelegationStatus::Canceled);
    let inner = state.inner.lock().expect("state mutex poisoned");
    let child = inner
        .sessions
        .iter()
        .find(|record| record.session.id == created.delegation.child_session_id)
        .expect("child session should remain visible");
    assert!(
        child.queued_prompts.is_empty(),
        "cancel should not leave queued child prompts available for dispatch"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn removing_delegation_child_or_parent_terminalizes_records() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let child_removed = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Remove child.".to_owned(),
                title: Some("Removed Child".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    state
        .kill_session(&child_removed.delegation.child_session_id)
        .expect("child removal should succeed");
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let delegation = inner
            .delegations
            .iter()
            .find(|delegation| delegation.id == child_removed.delegation.id)
            .expect("delegation record should remain");
        assert_eq!(delegation.status, DelegationStatus::Failed);
    }

    let mut delta_events = state.subscribe_delta_events();
    let parent_removed = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Remove parent.".to_owned(),
                title: Some("Removed Parent".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    while delta_events.try_recv().is_ok() {}
    state
        .kill_session(&parent_session_id)
        .expect("parent removal should succeed");
    while let Ok(payload) = delta_events.try_recv() {
        let delta: DeltaEvent = serde_json::from_str(&payload).expect("delta should deserialize");
        assert!(
            !matches!(
                delta,
                DeltaEvent::ParallelAgentsUpdate { session_id, .. }
                    if session_id == parent_session_id
            ),
            "parent removal should not publish a parent-card delta for the removed session"
        );
    }
    let inner = state.inner.lock().expect("state mutex poisoned");
    let delegation = inner
        .delegations
        .iter()
        .find(|delegation| delegation.id == parent_removed.delegation.id)
        .expect("delegation record should remain");
    assert_eq!(delegation.status, DelegationStatus::Failed);
    assert!(
        inner
            .sessions
            .iter()
            .all(|record| record.session.id != parent_removed.delegation.child_session_id),
        "parent removal should cascade-delete its delegated child session"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn removing_delegation_parent_cascades_to_nested_delegation_children() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let child = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Delegate first-level review.".to_owned(),
                title: Some("Cascade Child".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("first-level delegation should be created");
    let child_session_id = child.delegation.child_session_id.clone();
    let grandchild = state
        .create_read_only_delegation(
            &child_session_id,
            CreateDelegationRequest {
                prompt: "Delegate nested review.".to_owned(),
                title: Some("Cascade Grandchild".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("nested delegation should be created");
    let grandchild_session_id = grandchild.delegation.child_session_id.clone();
    state
        .set_external_session_id(&child_session_id, "cascade-child-thread".to_owned())
        .expect("child Codex thread id should be recorded");
    state
        .set_external_session_id(
            &grandchild_session_id,
            "cascade-grandchild-thread".to_owned(),
        )
        .expect("grandchild Codex thread id should be recorded");

    state
        .kill_session(&parent_session_id)
        .expect("parent removal should succeed");

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner.sessions.iter().all(|record| {
            record.session.id != parent_session_id
                && record.session.id != child_session_id
                && record.session.id != grandchild_session_id
        }),
        "parent removal should delete the full delegated child tree"
    );
    let child_delegation = inner
        .delegations
        .iter()
        .find(|delegation| delegation.id == child.delegation.id)
        .expect("child delegation record should remain");
    assert_eq!(child_delegation.status, DelegationStatus::Failed);
    let grandchild_delegation = inner
        .delegations
        .iter()
        .find(|delegation| delegation.id == grandchild.delegation.id)
        .expect("grandchild delegation record should remain");
    assert_eq!(grandchild_delegation.status, DelegationStatus::Failed);
    drop(inner);

    let mut reloaded_inner = load_state(state.persistence_path.as_path())
        .unwrap()
        .expect("persisted state should exist");
    assert!(
        reloaded_inner
            .ignored_discovered_codex_thread_ids
            .contains("cascade-child-thread")
    );
    assert!(
        reloaded_inner
            .ignored_discovered_codex_thread_ids
            .contains("cascade-grandchild-thread")
    );
    reloaded_inner.import_discovered_codex_threads(
        "/tmp",
        vec![
            DiscoveredCodexThread {
                approval_policy: Some(CodexApprovalPolicy::Never),
                archived: false,
                cwd: "/tmp".to_owned(),
                id: "cascade-child-thread".to_owned(),
                model: Some("gpt-5-codex".to_owned()),
                reasoning_effort: Some(CodexReasoningEffort::Medium),
                sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
                title: "Cascade child thread".to_owned(),
            },
            DiscoveredCodexThread {
                approval_policy: Some(CodexApprovalPolicy::Never),
                archived: false,
                cwd: "/tmp".to_owned(),
                id: "cascade-grandchild-thread".to_owned(),
                model: Some("gpt-5-codex".to_owned()),
                reasoning_effort: Some(CodexReasoningEffort::Medium),
                sandbox_mode: Some(CodexSandboxMode::WorkspaceWrite),
                title: "Cascade grandchild thread".to_owned(),
            },
        ],
    );
    assert!(
        reloaded_inner.sessions.iter().all(|record| {
            !matches!(
                record.external_session_id.as_deref(),
                Some("cascade-child-thread" | "cascade-grandchild-thread")
            )
        }),
        "cascade-deleted Codex child threads should not be rediscovered"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn removing_delegation_parent_reaps_hidden_claude_spare_for_child_profile() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Delegate Claude-profile review.".to_owned(),
                title: Some("Claude Profile Child".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");
    let child_session_id = created.delegation.child_session_id.clone();
    let spare_profile = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child_index = inner
            .find_session_index(&child_session_id)
            .expect("child session should exist");
        let child = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");
        child.session.agent = Agent::Claude;
        child.session.model = "claude-profile-child".to_owned();
        child.session.claude_approval_mode = Some(ClaudeApprovalMode::Plan);
        child.session.claude_effort = Some(ClaudeEffortLevel::High);
        let profile = claude_spare_profile(child);
        let spare_id = inner
            .ensure_hidden_claude_spare(
                profile.0.clone(),
                profile.1.clone(),
                profile.2.clone(),
                profile.3,
                profile.4,
            )
            .expect("hidden Claude spare should be reserved");
        assert!(
            inner.sessions.iter().any(|record| {
                record.session.id == spare_id
                    && record.hidden
                    && record.session.agent == Agent::Claude
            }),
            "test setup should create a matching hidden Claude spare"
        );
        state.commit_locked(&mut inner).unwrap();
        profile
    };

    state
        .kill_session(&parent_session_id)
        .expect("parent removal should succeed");

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .sessions
            .iter()
            .all(|record| record.session.id != child_session_id),
        "child session should be deleted"
    );
    assert!(
        inner.sessions.iter().all(|record| {
            !(record.hidden
                && record.session.agent == Agent::Claude
                && claude_spare_profile(record) == spare_profile)
        }),
        "hidden Claude spare for a cascade-deleted child should be reaped"
    );
    drop(inner);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn removing_delegation_parent_deletes_child_runtime_and_session() {
    let (state, input_rx) =
        test_app_state_with_delegation_codex_runtime("delegation-parent-remove-runtime");
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let mut delta_events = state.subscribe_delta_events();
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Remove parent while child is running.".to_owned(),
                title: Some("Parent Removed Runtime".to_owned()),
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

    let runtime = shared_codex_runtime_for_state(&state);
    runtime
        .sessions
        .lock()
        .expect("shared Codex session mutex poisoned")
        .insert(
            created.delegation.child_session_id.clone(),
            SharedCodexSessionState {
                thread_id: Some("delegation-parent-remove-thread".to_owned()),
                turn_id: Some("delegation-parent-remove-turn".to_owned()),
                ..SharedCodexSessionState::default()
            },
        );
    runtime
        .thread_sessions
        .lock()
        .expect("shared Codex thread mutex poisoned")
        .insert(
            "delegation-parent-remove-thread".to_owned(),
            created.delegation.child_session_id.clone(),
        );

    let _pending_message_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let child_index = inner
            .find_session_index(&created.delegation.child_session_id)
            .expect("child session should exist");
        let pending_message_id = inner.next_message_id();
        let queued_prompt_id = inner.next_message_id();
        let questions = vec![UserInputQuestion {
            header: "Choice".to_owned(),
            id: "choice".to_owned(),
            is_other: false,
            is_secret: false,
            options: None,
            question: "Continue?".to_owned(),
        }];
        let child = inner
            .session_mut_by_index(child_index)
            .expect("child session index should be valid");
        push_message_on_record(
            child,
            Message::UserInputRequest {
                id: pending_message_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Need input".to_owned(),
                detail: "Waiting for user input".to_owned(),
                questions: questions.clone(),
                state: InteractionRequestState::Pending,
                submitted_answers: None,
            },
        );
        child.pending_codex_user_inputs.insert(
            pending_message_id.clone(),
            CodexPendingUserInput {
                questions,
                request_id: json!("pending-user-input"),
            },
        );
        child.queued_prompts.push_back(QueuedPromptRecord {
            source: QueuedPromptSource::User,
            attachments: Vec::new(),
            pending_prompt: PendingPrompt {
                attachments: Vec::new(),
                id: queued_prompt_id,
                timestamp: stamp_now(),
                text: "do not dispatch after parent removal".to_owned(),
                expanded_text: None,
            },
        });
        sync_pending_prompts(child);
        child.deferred_stop_callbacks = vec![DeferredStopCallback::TurnCompleted];
        pending_message_id
    };
    while delta_events.try_recv().is_ok() {}

    let command_thread = std::thread::spawn(move || {
        let command = input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("parent removal should interrupt the running child");
        match command {
            CodexRuntimeCommand::InterruptTurn {
                thread_id,
                turn_id,
                response_tx,
            } => {
                assert_eq!(thread_id, "delegation-parent-remove-thread");
                assert_eq!(turn_id, "delegation-parent-remove-turn");
                let _ = response_tx.send(Ok(()));
            }
            _ => panic!("expected child turn interrupt command"),
        }
    });

    state
        .kill_session(&parent_session_id)
        .expect("parent removal should succeed");
    command_thread
        .join()
        .expect("parent removal command thread should join cleanly");

    let mut saw_child_transcript_delta = false;
    while let Ok(payload) = delta_events.try_recv() {
        let delta: DeltaEvent = serde_json::from_str(&payload).expect("delta should deserialize");
        match delta {
            DeltaEvent::MessageUpdated { session_id, .. }
            | DeltaEvent::MessageCreated { session_id, .. }
                if session_id == created.delegation.child_session_id =>
            {
                saw_child_transcript_delta = true;
            }
            DeltaEvent::ParallelAgentsUpdate { session_id, .. }
                if session_id == parent_session_id =>
            {
                panic!(
                    "parent removal should not publish a parent-card delta for the removed session"
                )
            }
            _ => {}
        }
    }
    assert!(
        !saw_child_transcript_delta,
        "deleted child sessions should not receive transcript deltas"
    );

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert!(
        inner
            .sessions
            .iter()
            .all(|record| record.session.id != created.delegation.child_session_id),
        "parent removal should delete the child session record"
    );
    drop(inner);

    assert!(
        !runtime
            .sessions
            .lock()
            .expect("shared Codex session mutex poisoned")
            .contains_key(&created.delegation.child_session_id)
    );
    assert!(
        !runtime
            .thread_sessions
            .lock()
            .expect("shared Codex thread mutex poisoned")
            .contains_key("delegation-parent-remove-thread")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn read_only_delegation_blocks_write_capable_surfaces() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do not mutate.".to_owned(),
                title: Some("Read-only Enforcement".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let err = match state.update_session_settings(
        &created.delegation.child_session_id,
        UpdateSessionSettingsRequest {
            name: Some("try rename".to_owned()),
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        },
    ) {
        Ok(_) => panic!("read-only child settings update should be rejected"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    let app = app_router(state.clone());
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "sessionId": created.delegation.child_session_id.as_str(),
                    "path": "blocked.txt",
                    "content": "blocked",
                    "overwrite": true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_read_only_delegation_error(&body, "Read-only Enforcement");

    let ssh_remote = RemoteConfig {
        id: "ssh-read-only-guard".to_owned(),
        name: "SSH Read-only Guard".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("example.com".to_owned()),
        port: Some(22),
        user: Some("alice".to_owned()),
    };
    let remote_project_id = create_test_remote_project(
        &state,
        &ssh_remote,
        "/remote/read-only-guard",
        "Remote Read-only Guard",
        "remote-read-only-guard",
    );
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "sessionId": created.delegation.child_session_id.as_str(),
                    "projectId": remote_project_id,
                    "path": "remote-bypass.txt",
                    "content": "blocked",
                    "overwrite": true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_read_only_delegation_error(&body, "Read-only Enforcement");

    let proxy_remote = RemoteConfig {
        id: "remote-read-only-bypass".to_owned(),
        name: "Remote Read-only Bypass".to_owned(),
        transport: RemoteTransport::Ssh,
        enabled: true,
        host: Some("127.0.0.1".to_owned()),
        port: Some(22),
        user: None,
    };
    let proxy_remote_project_id = create_test_remote_project(
        &state,
        &proxy_remote,
        "/remote/read-only-bypass",
        "Remote Read-only Bypass",
        "remote-project-read-only-bypass",
    );
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "sessionId": created.delegation.child_session_id.as_str(),
                    "projectId": proxy_remote_project_id,
                    "path": "remote-bypass.txt",
                    "content": "blocked before proxy",
                    "overwrite": true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_read_only_delegation_error(&body, "Read-only Enforcement");

    let child_session_id = created.delegation.child_session_id.clone();
    let child_workdir = created.child_session.workdir.clone();
    for (route, body) in [
        (
            "/api/git/file",
            json!({
                "sessionId": child_session_id.as_str(),
                "projectId": proxy_remote_project_id.as_str(),
                "workdir": child_workdir.as_str(),
                "action": "stage",
                "path": "remote-bypass.txt"
            }),
        ),
        (
            "/api/git/commit",
            json!({
                "sessionId": child_session_id.as_str(),
                "projectId": proxy_remote_project_id.as_str(),
                "workdir": child_workdir.as_str(),
                "message": "blocked commit"
            }),
        ),
        (
            "/api/git/push",
            json!({
                "sessionId": child_session_id.as_str(),
                "projectId": proxy_remote_project_id.as_str(),
                "workdir": child_workdir.as_str()
            }),
        ),
        (
            "/api/git/sync",
            json!({
                "sessionId": child_session_id.as_str(),
                "projectId": proxy_remote_project_id.as_str(),
                "workdir": child_workdir.as_str()
            }),
        ),
    ] {
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(route)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_read_only_delegation_error(&body, "Read-only Enforcement");
    }

    let review = default_review_document("read-only-review-guard");
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri(format!(
                "/api/reviews/read-only-review-guard?sessionId={}&projectId={}",
                child_session_id, proxy_remote_project_id
            ))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&review).expect("review document should serialize"),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_read_only_delegation_error(&body, "Read-only Enforcement");

    for terminal_route in ["/api/terminal/run", "/api/terminal/run/stream"] {
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(terminal_route)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "sessionId": child_session_id.as_str(),
                        "workdir": child_workdir.as_str(),
                        "command": "echo blocked"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_read_only_delegation_error(&body, "Read-only Enforcement");
    }

    for terminal_route in ["/api/terminal/run", "/api/terminal/run/stream"] {
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(terminal_route)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "sessionId": child_session_id.as_str(),
                        "projectId": proxy_remote_project_id.as_str(),
                        "workdir": child_workdir.as_str(),
                        "command": "echo blocked before proxy"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_read_only_delegation_error(&body, "Read-only Enforcement");
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[tokio::test]
async fn read_only_delegation_expired_child_link_uses_fallback_error_wording() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Leave a stale child link.".to_owned(),
                title: Some("Expired Link".to_owned()),
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
        inner
            .delegations
            .retain(|delegation| delegation.id != created.delegation.id);
        state.commit_locked(&mut inner).unwrap();
    }

    let app = app_router(state.clone());
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "sessionId": created.delegation.child_session_id.as_str(),
                    "path": "blocked-expired.txt",
                    "content": "blocked",
                    "overwrite": true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("while an expired read-only delegation is still attached")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn read_only_delegation_missing_child_session_still_blocks_scope_writes() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-read-only-missing-child-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let project_id = create_test_project(&state, &project_root, "Missing Child Scope");
    let parent_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &project_root);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Keep blocking this scope if the child disappears.".to_owned(),
                title: Some("Missing Child Delegation".to_owned()),
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
        inner.remove_session_at(child_index);
        state.commit_locked(&mut inner).unwrap();
    }

    let err = state
        .ensure_read_only_delegation_allows_write_action(
            None,
            Some(&project_id),
            None,
            "project writes",
        )
        .expect_err("missing child session should not make the delegation fail open");
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert!(
        err.message
            .contains("while read-only delegation `Missing Child Delegation` is running")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(project_root);
}

#[tokio::test]
async fn read_only_delegation_blocks_project_writes_without_session_id() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-read-only-project-{}", Uuid::new_v4()));
    fs::create_dir_all(&project_root).expect("project root should exist");
    let project_id = create_test_project(&state, &project_root, "Read-only Project");
    let parent_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &project_root);
    let _created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do not mutate project scope.".to_owned(),
                title: Some("Read-only Project Scope".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let app = app_router(state.clone());
    let blocked_file = project_root.join("blocked.txt");
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "projectId": project_id,
                    "path": blocked_file.to_string_lossy(),
                    "content": "blocked",
                    "overwrite": true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_read_only_delegation_error(&body, "Read-only Project Scope");
    assert!(!blocked_file.exists());

    let project_root_label = project_root.to_string_lossy().into_owned();
    let err = state
        .ensure_read_only_delegation_allows_write_action(
            None,
            None,
            Some(project_root_label.as_str()),
            "git file actions",
        )
        .expect_err("workdir-only writes should be blocked inside read-only delegation scope");
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    let missing_git_workdir = project_root
        .join("missing-git-worktree")
        .to_string_lossy()
        .into_owned();
    for git_route in ["/api/git/push", "/api/git/sync"] {
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(git_route)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "projectId": project_id.as_str(),
                        "workdir": missing_git_workdir.as_str()
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_read_only_delegation_error(&body, "Read-only Project Scope");
    }

    for terminal_route in ["/api/terminal/run", "/api/terminal/run/stream"] {
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(terminal_route)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "projectId": project_id.as_str(),
                        "workdir": project_root_label.as_str(),
                        "command": "echo blocked"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_read_only_delegation_error(&body, "Read-only Project Scope");
    }

    for terminal_route in ["/api/terminal/run", "/api/terminal/run/stream"] {
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(terminal_route)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "workdir": project_root_label.as_str(),
                        "command": "echo blocked"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_read_only_delegation_error(&body, "Read-only Project Scope");
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(project_root);
}

#[test]
fn read_only_delegation_blocks_bidirectional_workdir_containment() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-read-only-workdir-scope-{}", Uuid::new_v4()));
    let delegated_dir = project_root.join("delegated");
    let nested_write_dir = delegated_dir.join("nested");
    fs::create_dir_all(&nested_write_dir).expect("nested workdir should exist");
    let project_id = create_test_project(&state, &project_root, "Read-only Workdir Scope");
    let parent_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &project_root);
    let delegated_dir_label = delegated_dir.to_string_lossy().into_owned();
    let _created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do not mutate nested project scope.".to_owned(),
                title: Some("Read-only Nested Scope".to_owned()),
                cwd: Some(delegated_dir_label.clone()),
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let nested_write_dir_label = nested_write_dir.to_string_lossy().into_owned();
    let err = state
        .ensure_read_only_delegation_allows_write_action(
            None,
            None,
            Some(nested_write_dir_label.as_str()),
            "nested workdir write",
        )
        .expect_err("writes below the delegated workdir should be blocked");
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert!(
        err.message
            .contains("while read-only delegation `Read-only Nested Scope` is running")
    );

    let project_root_label = project_root.to_string_lossy().into_owned();
    let err = state
        .ensure_read_only_delegation_allows_write_action(
            None,
            None,
            Some(project_root_label.as_str()),
            "ancestor workdir write",
        )
        .expect_err("writes from an ancestor workdir should be blocked");
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert!(
        err.message
            .contains("while read-only delegation `Read-only Nested Scope` is running")
    );

    let sibling_write_dir = project_root.join("sibling");
    fs::create_dir_all(&sibling_write_dir).expect("sibling workdir should exist");
    let sibling_write_dir_label = sibling_write_dir.to_string_lossy().into_owned();
    let err = state
        .ensure_read_only_delegation_allows_write_action(
            None,
            None,
            Some(sibling_write_dir_label.as_str()),
            "sibling workdir write",
        )
        .expect_err("workdir-only writes should inherit the enclosing project root");
    assert_eq!(err.status, StatusCode::FORBIDDEN);
    assert!(
        err.message
            .contains("while read-only delegation `Read-only Nested Scope` is running")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(project_root);
}

#[test]
fn delegation_write_scope_does_not_match_empty_workdir() {
    for workdir in ["", "   \t"] {
        let scope = DelegationWriteScope {
            title: "Empty Workdir".to_owned(),
            project_id: None,
            workdir: workdir.to_owned(),
        };

        assert!(!delegation_write_scope_matches(
            &scope,
            None,
            Some("/tmp/termal-empty-workdir")
        ));
    }
}

#[tokio::test]
async fn read_only_delegation_blocks_git_repo_root_writes_from_sibling_workdir() {
    let state = test_app_state();
    let repo_root =
        std::env::temp_dir().join(format!("termal-read-only-git-root-{}", Uuid::new_v4()));
    let delegated_dir = repo_root.join("delegated");
    let sibling_dir = repo_root.join("sibling");
    fs::create_dir_all(&delegated_dir).expect("delegated workdir should exist");
    fs::create_dir_all(&sibling_dir).expect("sibling workdir should exist");
    init_git_document_test_repo(&repo_root);
    let parent_session_id = {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        let record = inner.create_session(
            Agent::Codex,
            Some("Git Parent".to_owned()),
            repo_root.to_string_lossy().into_owned(),
            None,
            None,
        );
        let session_id = record.session.id.clone();
        state.commit_locked(&mut inner).unwrap();
        session_id
    };
    let delegated_dir_label = delegated_dir.to_string_lossy().into_owned();
    let _created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do not mutate this repository.".to_owned(),
                title: Some("Read-only Git Root".to_owned()),
                cwd: Some(delegated_dir_label),
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let app = app_router(state.clone());
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/git/commit")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "workdir": sibling_dir.to_string_lossy(),
                    "message": "blocked"
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_read_only_delegation_error(&body, "Read-only Git Root");

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(repo_root);
}

#[tokio::test]
async fn read_only_delegation_blocks_project_and_workdir_writes_with_parent_session_id() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-read-only-parent-scope-{}", Uuid::new_v4()));
    let unrelated_root = std::env::temp_dir().join(format!(
        "termal-read-only-unrelated-scope-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&unrelated_root).expect("unrelated project root should exist");
    let project_id = create_test_project(&state, &project_root, "Read-only Parent Scope");
    let unrelated_project_id =
        create_test_project(&state, &unrelated_root, "Unrelated Project Scope");
    let parent_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &project_root);
    let sibling_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &project_root);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do not mutate through the parent.".to_owned(),
                title: Some("Read-only Parent Scope".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    for (session_id, name) in [
        (parent_session_id.as_str(), "parent can still rename"),
        (sibling_session_id.as_str(), "sibling can still rename"),
    ] {
        state
            .update_session_settings(
                session_id,
                UpdateSessionSettingsRequest {
                    name: Some(name.to_owned()),
                    model: None,
                    approval_policy: None,
                    reasoning_effort: None,
                    sandbox_mode: None,
                    cursor_mode: None,
                    claude_approval_mode: None,
                    claude_effort: None,
                    gemini_approval_mode: None,
                },
            )
            .expect("parent/sibling settings should not inherit project-wide write blocking");
    }
    let err = match state.update_session_settings(
        &created.delegation.child_session_id,
        UpdateSessionSettingsRequest {
            name: Some("child rename stays blocked".to_owned()),
            model: None,
            approval_policy: None,
            reasoning_effort: None,
            sandbox_mode: None,
            cursor_mode: None,
            claude_approval_mode: None,
            claude_effort: None,
            gemini_approval_mode: None,
        },
    ) {
        Ok(_) => panic!("read-only child settings should stay blocked"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    let err = state
        .ensure_read_only_delegation_allows_write_action(
            Some(&parent_session_id),
            Some(&project_id),
            None,
            "file writes",
        )
        .expect_err("parent session id should not bypass read-only project scope");
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    let project_root_label = project_root.to_string_lossy().into_owned();
    let err = state
        .ensure_read_only_delegation_allows_write_action(
            Some(&parent_session_id),
            None,
            Some(project_root_label.as_str()),
            "git file actions",
        )
        .expect_err("parent session id should not bypass read-only workdir scope");
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    let unrelated_root_label = unrelated_root.to_string_lossy().into_owned();
    let err = state
        .ensure_read_only_delegation_allows_write_action(
            Some(&sibling_session_id),
            Some(&unrelated_project_id),
            Some(unrelated_root_label.as_str()),
            "mixed-scope file writes",
        )
        .expect_err("explicit project/workdir should not hide the session-derived scope");
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    let app = app_router(state.clone());
    for (session_id, file_name) in [
        (parent_session_id.as_str(), "parent-session-only.txt"),
        (sibling_session_id.as_str(), "sibling-session-only.txt"),
    ] {
        let blocked_file = project_root.join(file_name);
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("PUT")
                .uri("/api/file")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "sessionId": session_id,
                        "path": file_name,
                        "content": "blocked",
                        "overwrite": true
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_read_only_delegation_error(&body, "Read-only Parent Scope");
        assert!(!blocked_file.exists());
    }

    let unrelated_file = unrelated_root.join("mixed-project.txt");
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "sessionId": sibling_session_id,
                    "projectId": unrelated_project_id,
                    "path": "mixed-project.txt",
                    "content": "blocked",
                    "overwrite": true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_read_only_delegation_error(&body, "Read-only Parent Scope");
    assert!(!unrelated_file.exists());

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(unrelated_root);
}

#[tokio::test]
async fn read_only_delegation_blocks_overlapping_project_id_only_writes() {
    let state = test_app_state();
    let outer_root =
        std::env::temp_dir().join(format!("termal-read-only-outer-{}", Uuid::new_v4()));
    let inner_root = outer_root.join("inner");
    let outer_sibling_root = outer_root.join("sibling");
    let unrelated_root =
        std::env::temp_dir().join(format!("termal-read-only-disjoint-{}", Uuid::new_v4()));
    fs::create_dir_all(&inner_root).expect("inner project should exist");
    fs::create_dir_all(&outer_sibling_root).expect("outer sibling project should exist");
    fs::create_dir_all(&unrelated_root).expect("unrelated project should exist");
    let outer_project_id = create_test_project(&state, &outer_root, "Read-only Outer");
    let inner_project_id = create_test_project(&state, &inner_root, "Read-only Inner");
    let unrelated_project_id = create_test_project(&state, &unrelated_root, "Unrelated Project");
    let parent_session_id =
        create_test_project_session(&state, Agent::Codex, &inner_project_id, &inner_root);
    let _created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do not mutate overlapping project scope.".to_owned(),
                title: Some("Read-only Overlap".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    let err = state
        .ensure_read_only_delegation_allows_write_action(
            None,
            Some(&outer_project_id),
            None,
            "overlapping project writes",
        )
        .expect_err("outer project id should block writes into nested delegated project");
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    let outer_sibling_root_label = outer_sibling_root.to_string_lossy().into_owned();
    let err = state
        .ensure_read_only_delegation_allows_write_action(
            None,
            Some(&outer_project_id),
            Some(outer_sibling_root_label.as_str()),
            "mixed project/workdir writes",
        )
        .expect_err("outer project root should stay checked when workdir is also supplied");
    assert_eq!(err.status, StatusCode::FORBIDDEN);

    state
        .ensure_read_only_delegation_allows_write_action(
            None,
            Some(&unrelated_project_id),
            None,
            "unrelated project writes",
        )
        .expect("unrelated project id should not be blocked");

    let app = app_router(state.clone());
    let blocked_file = inner_root.join("blocked-overlap.txt");
    let (status, body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "projectId": outer_project_id,
                    "path": blocked_file.to_string_lossy(),
                    "content": "blocked",
                    "overwrite": true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_read_only_delegation_error(&body, "Read-only Overlap");
    assert!(!blocked_file.exists());

    let allowed_file = unrelated_root.join("allowed.txt");
    let (status, _body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("PUT")
            .uri("/api/file")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "projectId": unrelated_project_id,
                    "path": allowed_file.to_string_lossy(),
                    "content": "allowed",
                    "overwrite": true
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        fs::read_to_string(&allowed_file).expect("allowed write should persist"),
        "allowed"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(outer_root);
    let _ = fs::remove_dir_all(unrelated_root);
}

#[test]
fn read_only_delegation_rejects_approval_acceptance() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Do not approve writes.".to_owned(),
                title: Some("Approval Guard".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    for decision in [
        ApprovalDecision::Accepted,
        ApprovalDecision::AcceptedForSession,
    ] {
        let err = match state.update_approval(
            &created.delegation.child_session_id,
            "approval-1",
            decision,
        ) {
            Ok(_) => panic!("read-only child approval acceptance should be rejected"),
            Err(err) => err,
        };
        assert_eq!(err.status, StatusCode::FORBIDDEN);
    }

    let err = match state.update_approval(
        &created.delegation.child_session_id,
        "approval-1",
        ApprovalDecision::Rejected,
    ) {
        Ok(_) => panic!("missing pending approval should still fail normally"),
        Err(err) => err,
    };
    assert_eq!(err.status, StatusCode::CONFLICT);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn read_only_cursor_delegation_uses_plan_mode() {
    let state = test_app_state();
    let acp_input_rx =
        install_delegation_acp_runtime(&state, AcpAgent::Cursor, "delegation-cursor-plan");
    let parent_session_id = test_session_id(&state, Agent::Cursor);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Inspect without editing.".to_owned(),
                title: Some("Cursor Plan Review".to_owned()),
                cwd: None,
                agent: Some(Agent::Cursor),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("cursor delegation should be created");

    assert_eq!(
        created.delegation.write_policy,
        DelegationWritePolicy::ReadOnly
    );
    assert_eq!(created.delegation.status, DelegationStatus::Running);
    assert_eq!(created.child_session.status, SessionStatus::Active);
    assert_eq!(created.child_session.cursor_mode, Some(CursorMode::Plan));
    match acp_input_rx
        .recv_timeout(Duration::from_millis(100))
        .expect("Cursor delegation should dispatch through the fake ACP runtime")
    {
        AcpRuntimeCommand::Prompt(command) => {
            assert_eq!(command.cwd, created.delegation.cwd);
            assert_eq!(command.cursor_mode, Some(CursorMode::Plan));
            assert_eq!(command.model, created.child_session.model);
            assert_eq!(command.resume_session_id, None);
            assert!(
                command.prompt.contains("Inspect without editing."),
                "delegation prompt should include the requested task",
            );
        }
        _ => panic!("Cursor delegation should dispatch an ACP prompt command"),
    }

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_empty_model_uses_agent_default() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Use the default model.".to_owned(),
                title: Some("Default Model".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: Some("   ".to_owned()),
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    assert_eq!(created.child_session.model, Agent::Codex.default_model());
    assert_eq!(
        created.delegation.model.as_deref(),
        Some(Agent::Codex.default_model())
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_empty_model_uses_configured_agent_default() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.preferences.default_codex_model = "gpt-5.5".to_owned();
    }

    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Use the configured default model.".to_owned(),
                title: Some("Configured Default Model".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: Some("   ".to_owned()),
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    assert_eq!(created.child_session.model, "gpt-5.5");
    assert_eq!(created.delegation.model.as_deref(), Some("gpt-5.5"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_default_model_uses_update_app_settings_normalized_value() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let updated = state
        .update_app_settings(UpdateAppSettingsRequest {
            default_codex_model: Some("  gpt-5.5  ".to_owned()),
            default_claude_model: None,
            default_cursor_model: None,
            default_gemini_model: None,
            default_codex_reasoning_effort: None,
            default_claude_approval_mode: None,
            default_claude_effort: None,
            remotes: None,
        })
        .expect("app settings should update");
    assert_eq!(updated.preferences.default_codex_model, "gpt-5.5");

    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Use the normalized configured default model.".to_owned(),
                title: Some("Normalized Configured Default Model".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: Some("   ".to_owned()),
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    assert_eq!(created.child_session.model, "gpt-5.5");
    assert_eq!(created.delegation.model.as_deref(), Some("gpt-5.5"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_omitted_model_uses_selected_agent_default_not_parent_model() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Claude);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.preferences.default_codex_model = "gpt-5.5".to_owned();
    }
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Use the selected agent default model.".to_owned(),
                title: Some("Selected Agent Default Model".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    assert_eq!(created.child_session.agent, Agent::Codex);
    assert_eq!(created.child_session.model, "gpt-5.5");
    assert_eq!(created.delegation.model.as_deref(), Some("gpt-5.5"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_omitted_agent_and_model_use_parent_agent_default_model() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.preferences.default_codex_model = "gpt-5.5".to_owned();
    }
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Use the parent agent default model.".to_owned(),
                title: Some("Parent Agent Default Model".to_owned()),
                cwd: None,
                agent: None,
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    assert_eq!(created.child_session.agent, Agent::Codex);
    assert_eq!(created.child_session.model, "gpt-5.5");
    assert_eq!(created.delegation.model.as_deref(), Some("gpt-5.5"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_explicit_model_is_preserved_verbatim() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Use the requested model.".to_owned(),
                title: Some("Explicit Model".to_owned()),
                cwd: None,
                agent: Some(Agent::Codex),
                model: Some("custom-model-string".to_owned()),
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("delegation should be created");

    assert_eq!(created.child_session.agent, Agent::Codex);
    assert_eq!(created.child_session.model, "custom-model-string");
    assert_eq!(
        created.delegation.model.as_deref(),
        Some("custom-model-string")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_write_policy_accepts_legacy_snake_case_discriminators() {
    let read_only: DelegationWritePolicy =
        serde_json::from_value(json!({ "kind": "read_only" })).expect("read_only alias");
    assert_eq!(read_only, DelegationWritePolicy::ReadOnly);

    let shared: DelegationWritePolicy = serde_json::from_value(json!({
        "kind": "shared_worktree",
        "ownedPaths": ["src"]
    }))
    .expect("shared_worktree alias");
    assert_eq!(
        shared,
        DelegationWritePolicy::SharedWorktree {
            owned_paths: vec!["src".to_owned()],
        }
    );

    let isolated: DelegationWritePolicy = serde_json::from_value(json!({
        "kind": "isolated_worktree",
        "ownedPaths": ["src"],
        "worktreePath": "C:/tmp/delegation-worktree"
    }))
    .expect("isolated_worktree alias");
    assert_eq!(
        isolated,
        DelegationWritePolicy::IsolatedWorktree {
            owned_paths: vec!["src".to_owned()],
            worktree_path: Some("C:/tmp/delegation-worktree".to_owned()),
        }
    );

    let isolated_without_path: DelegationWritePolicy = serde_json::from_value(json!({
        "kind": "isolatedWorktree",
        "ownedPaths": []
    }))
    .expect("isolatedWorktree should accept omitted worktreePath");
    assert_eq!(
        isolated_without_path,
        DelegationWritePolicy::IsolatedWorktree {
            owned_paths: Vec::new(),
            worktree_path: None,
        }
    );
}

#[test]
fn create_delegation_wait_request_defaults_to_all_mode() {
    let request: CreateDelegationWaitRequest =
        serde_json::from_value(json!({ "delegationIds": ["delegation-1"] }))
            .expect("wait request should deserialize with defaults");

    assert_eq!(request.delegation_ids, vec!["delegation-1".to_owned()]);
    assert_eq!(request.mode, DelegationWaitMode::All);
    assert_eq!(request.title, None);
}

#[test]
fn delegation_prompt_size_is_capped() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let oversized_prompt = "x".repeat(MAX_DELEGATION_PROMPT_BYTES + 1);
    let err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: oversized_prompt,
            title: None,
            cwd: None,
            agent: Some(Agent::Codex),
            model: None,
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("oversized prompt should be rejected"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("at most"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_title_size_is_capped() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let oversized_title = "x".repeat(MAX_DELEGATION_TITLE_CHARS + 1);
    let err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: "Review this change.".to_owned(),
            title: Some(oversized_title),
            cwd: None,
            agent: Some(Agent::Codex),
            model: None,
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("oversized title should be rejected"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("title"));
    assert!(err.message.contains("at most"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_title_size_accepts_trimmed_character_boundary() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let boundary_title = "界".repeat(MAX_DELEGATION_TITLE_CHARS);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review this change.".to_owned(),
                title: Some(format!("  {boundary_title}  ")),
                cwd: None,
                agent: Some(Agent::Codex),
                model: None,
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("boundary title should be accepted");

    assert_eq!(created.child_session.name, boundary_title);
    assert_eq!(created.delegation.title, boundary_title);

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_model_size_is_capped() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let oversized_model = "x".repeat(MAX_DELEGATION_MODEL_CHARS + 1);
    let err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: "Review this change.".to_owned(),
            title: None,
            cwd: None,
            agent: Some(Agent::Codex),
            model: Some(oversized_model),
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("oversized model should be rejected"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("model"));
    assert!(err.message.contains("at most"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_model_size_accepts_trimmed_character_boundary() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let boundary_model = "界".repeat(MAX_DELEGATION_MODEL_CHARS);
    let created = state
        .create_read_only_delegation(
            &parent_session_id,
            CreateDelegationRequest {
                prompt: "Review this change.".to_owned(),
                title: None,
                cwd: None,
                agent: Some(Agent::Codex),
                model: Some(format!("  {boundary_model}  ")),
                mode: Some(DelegationMode::Reviewer),
                write_policy: Some(DelegationWritePolicy::ReadOnly),
            },
        )
        .expect("boundary model should be accepted");

    assert_eq!(created.child_session.model, boundary_model);
    assert_eq!(
        created.delegation.model.as_deref(),
        Some(boundary_model.as_str())
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_metadata_size_errors_precede_phase_one_feature_gates() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let oversized_title = format!("{}界", "x".repeat(MAX_DELEGATION_TITLE_CHARS));
    let title_err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: "Review this change.".to_owned(),
            title: Some(oversized_title),
            cwd: None,
            agent: Some(Agent::Codex),
            model: None,
            mode: Some(DelegationMode::Worker),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("oversized metadata should be rejected before feature gates"),
        Err(err) => err,
    };

    assert_eq!(title_err.status, StatusCode::BAD_REQUEST);
    assert!(title_err.message.contains("title"));

    let oversized_model = format!("{}界", "x".repeat(MAX_DELEGATION_MODEL_CHARS));
    let model_err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: "Review this change.".to_owned(),
            title: None,
            cwd: None,
            agent: Some(Agent::Codex),
            model: Some(oversized_model),
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::SharedWorktree {
                owned_paths: Vec::new(),
            }),
        },
    ) {
        Ok(_) => panic!("oversized metadata should be rejected before feature gates"),
        Err(err) => err,
    };

    assert_eq!(model_err.status, StatusCode::BAD_REQUEST);
    assert!(model_err.message.contains("model"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_metadata_size_errors_precede_setup_failure_injection() {
    let state = test_app_state();
    state
        .test_agent_setup_failures
        .lock()
        .expect("test agent setup failures mutex poisoned")
        .push((
            Agent::Cursor,
            "forced Cursor setup failure for ordering test".to_owned(),
        ));
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let oversized_title = "x".repeat(MAX_DELEGATION_TITLE_CHARS + 1);
    let title_err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: "Review this change.".to_owned(),
            title: Some(oversized_title),
            cwd: None,
            agent: Some(Agent::Cursor),
            model: None,
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("oversized title should be rejected before readiness setup"),
        Err(err) => err,
    };

    assert_eq!(title_err.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        title_err.message,
        format!("delegation title must be at most {MAX_DELEGATION_TITLE_CHARS} characters")
    );
    assert!(
        !title_err
            .message
            .contains("forced Cursor setup failure for ordering test"),
        "metadata validation should run before setup failure injection: {}",
        title_err.message
    );

    let oversized_model = "x".repeat(MAX_DELEGATION_MODEL_CHARS + 1);
    let model_err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: "Review this change.".to_owned(),
            title: None,
            cwd: None,
            agent: Some(Agent::Cursor),
            model: Some(oversized_model),
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("oversized model should be rejected before readiness setup"),
        Err(err) => err,
    };

    assert_eq!(model_err.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        model_err.message,
        format!("delegation model must be at most {MAX_DELEGATION_MODEL_CHARS} characters")
    );
    assert!(
        !model_err
            .message
            .contains("forced Cursor setup failure for ordering test"),
        "metadata validation should run before setup failure injection: {}",
        model_err.message
    );

    let setup_err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: "Review this change.".to_owned(),
            title: Some("Valid metadata".to_owned()),
            cwd: None,
            agent: Some(Agent::Cursor),
            model: None,
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("forced setup failure should reject valid metadata"),
        Err(err) => err,
    };

    assert_eq!(setup_err.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        setup_err.message,
        "forced Cursor setup failure for ordering test"
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_whitespace_prompt_is_rejected() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let err = match state.create_read_only_delegation(
        &parent_session_id,
        CreateDelegationRequest {
            prompt: "   \t\n".to_owned(),
            title: None,
            cwd: None,
            agent: Some(Agent::Codex),
            model: None,
            mode: Some(DelegationMode::Reviewer),
            write_policy: Some(DelegationWritePolicy::ReadOnly),
        },
    ) {
        Ok(_) => panic!("whitespace-only prompt should be rejected"),
        Err(err) => err,
    };

    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("cannot be empty"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[test]
fn delegation_rejects_traversal_cwd_outside_project() {
    let state = test_app_state();
    let project_root =
        std::env::temp_dir().join(format!("termal-delegation-cwd-root-{}", Uuid::new_v4()));
    let inside_dir = project_root.join("inside");
    let outside_root =
        std::env::temp_dir().join(format!("termal-delegation-cwd-outside-{}", Uuid::new_v4()));
    fs::create_dir_all(&inside_dir).expect("project workdir should exist");
    fs::create_dir_all(&outside_root).expect("outside directory should exist");
    let project_id = create_test_project(&state, &project_root, "Delegation Cwd Root");
    let parent_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &inside_dir);
    let traversal_cwd = inside_dir.join("..").join("..").join(
        outside_root
            .file_name()
            .expect("outside root should have name"),
    );

    let err = expect_delegation_cwd_rejected(
        &state,
        &parent_session_id,
        traversal_cwd.to_string_lossy().into_owned(),
    );
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("must stay inside project"));

    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(outside_root);
}

#[cfg(windows)]
#[test]
fn delegation_rejects_windows_drive_relative_cwd() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let drive = std::env::current_dir()
        .expect("current dir should resolve")
        .components()
        .find_map(|component| match component {
            std::path::Component::Prefix(prefix) => match prefix.kind() {
                std::path::Prefix::Disk(drive) | std::path::Prefix::VerbatimDisk(drive) => {
                    Some((drive as char).to_ascii_uppercase())
                }
                _ => None,
            },
            _ => None,
        })
        .unwrap_or('C');
    let err = expect_delegation_cwd_rejected(
        &state,
        &parent_session_id,
        format!("{drive}:termal-drive-relative-cwd"),
    );
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("drive-relative"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[cfg(windows)]
#[test]
fn delegation_rejects_windows_unc_cwd_before_metadata_lookup() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let err = expect_delegation_cwd_rejected(
        &state,
        &parent_session_id,
        r"\\server\share\termal-delegation-cwd".to_owned(),
    );
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("UNC"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[cfg(windows)]
#[test]
fn delegation_rejects_windows_device_namespace_unc_alias_cwd() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let err = expect_delegation_cwd_rejected(
        &state,
        &parent_session_id,
        r"\\.\UNC\server\share\termal-delegation-cwd".to_owned(),
    );
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("device namespace"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[cfg(windows)]
#[test]
fn delegation_rejects_windows_globalroot_and_mup_cwd() {
    for cwd in [
        r"\\?\GLOBALROOT\Device\Mup\server\share\termal-delegation-cwd",
        r"\\?\Mup\server\share\termal-delegation-cwd",
    ] {
        let state = test_app_state();
        let parent_session_id = test_session_id(&state, Agent::Codex);
        let err = expect_delegation_cwd_rejected(&state, &parent_session_id, cwd.to_owned());
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(err.message.contains("device namespace"));

        let _ = fs::remove_file(state.persistence_path.as_path());
    }
}

#[cfg(windows)]
#[test]
fn delegation_rejects_windows_verbatim_drive_relative_cwd() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let drive = std::env::current_dir()
        .expect("current dir should resolve")
        .components()
        .find_map(|component| match component {
            std::path::Component::Prefix(prefix) => match prefix.kind() {
                std::path::Prefix::Disk(drive) | std::path::Prefix::VerbatimDisk(drive) => {
                    Some((drive as char).to_ascii_uppercase())
                }
                _ => None,
            },
            _ => None,
        })
        .unwrap_or('C');
    let err = expect_delegation_cwd_rejected(
        &state,
        &parent_session_id,
        format!(r"\\?\{drive}:termal-drive-relative-cwd"),
    );
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("drive-relative"));

    let _ = fs::remove_file(state.persistence_path.as_path());
}

#[cfg(unix)]
#[test]
fn delegation_rejects_symlinked_cwd_escape_from_project() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-delegation-cwd-link-root-{}",
        Uuid::new_v4()
    ));
    let outside_root = std::env::temp_dir().join(format!(
        "termal-delegation-cwd-link-outside-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&outside_root).expect("outside directory should exist");
    let project_id = create_test_project(&state, &project_root, "Delegation Symlink Root");
    let parent_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &project_root);
    let link_path = project_root.join("outside-link");
    create_test_dir_symlink(&outside_root, &link_path).expect("test symlink should be created");

    let err = expect_delegation_cwd_rejected(
        &state,
        &parent_session_id,
        link_path.to_string_lossy().into_owned(),
    );
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("must stay inside project"));

    let _ = fs::remove_dir(&link_path);
    let _ = fs::remove_file(&link_path);
    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(outside_root);
}

#[cfg(windows)]
#[test]
fn delegation_rejects_windows_symlinked_cwd_escape_from_project() {
    let state = test_app_state();
    let project_root = std::env::temp_dir().join(format!(
        "termal-delegation-cwd-link-root-{}",
        Uuid::new_v4()
    ));
    let outside_root = std::env::temp_dir().join(format!(
        "termal-delegation-cwd-link-outside-{}",
        Uuid::new_v4()
    ));
    fs::create_dir_all(&project_root).expect("project root should exist");
    fs::create_dir_all(&outside_root).expect("outside directory should exist");
    let project_id = create_test_project(&state, &project_root, "Delegation Symlink Root");
    let parent_session_id =
        create_test_project_session(&state, Agent::Codex, &project_id, &project_root);
    let link_path = project_root.join("outside-link");
    if let Err(err) = create_test_dir_symlink(&outside_root, &link_path) {
        let _ = fs::remove_file(state.persistence_path.as_path());
        let _ = fs::remove_dir_all(project_root);
        let _ = fs::remove_dir_all(outside_root);
        if windows_symlink_privilege_unavailable(&err) {
            eprintln!("skipping Windows symlink cwd escape test: {err}");
            return;
        }
        panic!("test symlink should be created: {err}");
    }

    let err = expect_delegation_cwd_rejected(
        &state,
        &parent_session_id,
        link_path.to_string_lossy().into_owned(),
    );
    assert_eq!(err.status, StatusCode::BAD_REQUEST);
    assert!(err.message.contains("must stay inside project"));

    let _ = fs::remove_dir(&link_path);
    let _ = fs::remove_file(&link_path);
    let _ = fs::remove_file(state.persistence_path.as_path());
    let _ = fs::remove_dir_all(project_root);
    let _ = fs::remove_dir_all(outside_root);
}

#[tokio::test]
async fn delegation_route_rejects_remote_proxy_parent_without_local_project() {
    for (remote_id, remote_session_id) in [
        (Some("ssh-review"), Some("remote-parent-1")),
        (Some("ssh-review"), None),
        (None, Some("remote-parent-1")),
    ] {
        let state = test_app_state();
        let parent_session_id = test_session_id(&state, Agent::Codex);
        {
            let mut inner = state.inner.lock().expect("state mutex poisoned");
            let parent_index = inner
                .find_visible_session_index(&parent_session_id)
                .expect("parent session should exist");
            let parent = inner
                .session_mut_by_index(parent_index)
                .expect("parent session index should be valid");
            parent.session.project_id = None;
            parent.remote_id = remote_id.map(str::to_owned);
            parent.remote_session_id = remote_session_id.map(str::to_owned);
        }

        let app = app_router(state.clone());
        let (status, body): (StatusCode, Value) = request_json(
            &app,
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{parent_session_id}/delegations"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"prompt":"review remote parent","writePolicy":{"kind":"readOnly"}}"#,
                ))
                .unwrap(),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
        assert!(
            body["error"]
                .as_str()
                .unwrap()
                .contains("delegations for remote-backed sessions are not implemented")
        );
        let inner = state.inner.lock().expect("state mutex poisoned");
        assert!(inner.delegations.is_empty());
        assert_eq!(inner.sessions.len(), 1);
        drop(inner);

        let _ = fs::remove_file(state.persistence_path.as_path());
    }
}

#[tokio::test]
async fn delegation_route_rejects_worker_and_shared_worktree_policy() {
    let state = test_app_state();
    let parent_session_id = test_session_id(&state, Agent::Codex);
    let app = app_router(state.clone());

    let (worker_status, worker_body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{parent_session_id}/delegations"))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"prompt":"try worker","mode":"worker","writePolicy":{"kind":"readOnly"}}"#,
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(worker_status, StatusCode::NOT_IMPLEMENTED);
    assert!(
        worker_body["error"]
            .as_str()
            .unwrap()
            .contains("worker delegations are not implemented")
    );

    let (policy_status, policy_body): (StatusCode, Value) = request_json(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/sessions/{parent_session_id}/delegations"))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"prompt":"try write","writePolicy":{"kind":"sharedWorktree","ownedPaths":["src"]}}"#,
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(policy_status, StatusCode::NOT_IMPLEMENTED);
    assert!(
        policy_body["error"]
            .as_str()
            .unwrap()
            .contains("sharedWorktree delegation write policy")
    );

    let _ = fs::remove_file(state.persistence_path.as_path());
}
