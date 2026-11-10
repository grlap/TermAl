//! Restart recovery uses real persisted boot state and the existing queue.
use super::*;

mod delivery;
mod ordering;
mod retry;

fn pending(id: &str, text: &str) -> PendingPrompt {
    PendingPrompt {
        id: id.to_owned(),
        timestamp: stamp_now(),
        text: text.to_owned(),
        expanded_text: None,
        source: None,
        attachments: Vec::new(),
        engram_interrupted: false,
        is_engram_retained: false,
    }
}

fn active_root(inner: &mut StateInner, root: &FsPath) -> String {
    let session = inner
        .create_session(
            Agent::Cursor,
            None,
            root.to_string_lossy().into_owned(),
            None,
            None,
        )
        .session
        .id;
    let index = inner.find_session_index(&session).unwrap();
    inner.session_mut_by_index(index).unwrap().session.status = SessionStatus::Active;
    session
}

fn finish_current_turn(state: &AppState, session: &str) {
    let token = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(session).unwrap()]
            .runtime
            .runtime_token()
            .unwrap()
    };
    state
        .finish_turn_ok_if_runtime_matches(session, &token)
        .unwrap();
}

fn marker(prompt_id: Option<&str>) -> SessionRestartRecovery {
    SessionRestartRecovery {
        notice: "An interrupted turn".to_owned(),
        restarted_at: "2026-11-10T00:00:00Z".to_owned(),
        queued_prompt_count: 0,
        prompt_id: prompt_id.map(str::to_owned),
    }
}

struct RecoveryBoot(AppState);

impl Drop for RecoveryBoot {
    fn drop(&mut self) {
        self.0.shutdown_persist_blocking();
    }
}

fn boot(root: &FsPath) -> RecoveryBoot {
    let mut state = AppState::new_before_listen_with_paths(
        root.to_string_lossy().into_owned(),
        root.join("termal.sqlite"),
        root.join("orchestrators.json"),
    )
    .unwrap();
    // Only explicitly installed test runtime handles may consume prompts.
    state.agent_runtime_spawning_enabled = false;
    RecoveryBoot(state)
}

#[test]
fn interrupted_root_gets_one_durable_recovery_opening_and_no_replay_after_restart() {
    let root = TestTempRoot::create("restart-recovery-opening");
    let mut inner = StateInner::new();
    let mut sessions = Vec::new();
    for name in [
        "active",
        "idle",
        "paused",
        "stopped",
        "operator-paused",
        "child",
        "observation-deleted",
        "indexed-child",
    ] {
        let record = inner.create_session(
            Agent::Cursor,
            Some(name.to_owned()),
            root.path().to_string_lossy().into_owned(),
            None,
            None,
        );
        sessions.push(record.session.id.clone());
    }
    for (position, session) in sessions.iter().enumerate() {
        let index = inner.find_session_index(session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.session.status = if position == 1 {
            SessionStatus::Idle
        } else {
            SessionStatus::Active
        };
        match position {
            2 => record.set_auto_dispatch_blocked(true),
            3 => {
                record.engram.stopped_prompt_id = Some("operator-stopped".to_owned());
            }
            4 => record.engram.operator_paused = true,
            5 => record.session.parent_delegation_id = Some("delegation-retained-child".to_owned()),
            6 => record.engram.observation_delete_requested = true,
            _ => {}
        }
    }
    inner.delegations.push(DelegationRecord {
        original_project_id: None,
        project_basis_recorded: false,
        id: "delegation-indexed-child".to_owned(),
        parent_session_id: sessions[1].clone(),
        child_session_id: sessions[7].clone(),
        mode: DelegationMode::Explorer,
        status: DelegationStatus::Completed,
        title: "Retained child".to_owned(),
        prompt: "Earlier child request".to_owned(),
        cwd: root.path().to_string_lossy().into_owned(),
        agent: Agent::Cursor,
        model: None,
        write_policy: DelegationWritePolicy::ReadOnly,
        created_at: stamp_now(),
        started_at: None,
        completed_at: Some(stamp_now()),
        result: None,
        submitted_review_result: None,
        post_submission_transport_error: None,
        base_revision: None,
        review_result_recovery_probe_attempt: None,
        review_result_recovery_error: None,
        review_result_schema_version: None,
        queued_followup_prompt_id: None,
        review_result_submission_attempt: 0,
        acceptance_evaluation: None,
        attempt: DelegationAttemptState::default(),
    });
    persist_state(&root.path().join("termal.sqlite"), &inner).unwrap();
    drop(inner);

    let first = boot(root.path());
    let (runtime, prompts) = test_acp_runtime_handle(AcpAgent::Cursor, "restart-recovery-first");
    first
        .0
        .install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
    // This drives inventory and post-listen recovery, not the host's real runtime.
    first.0.run_post_listen_boot().unwrap();
    let AcpRuntimeCommand::Prompt(command) = phase_sync::receive(
        &prompts,
        "the interrupted root receives its recovery opening",
    ) else {
        panic!("recovery should send a prompt");
    };
    assert!(command.prompt.contains("[TermAl restart recovery]"));
    assert!(
        command
            .prompt
            .contains("TermAl restarted before this turn finished")
    );
    assert!(command.prompt.contains("Restart time:"));
    assert!(command.prompt.contains("Queued prompts saved: 0"));
    let recovery_id = {
        let inner = first.0.inner.lock().unwrap();
        let active = &inner.sessions[inner.find_session_index(&sessions[0]).unwrap()];
        assert_eq!(active.session.status, SessionStatus::Active);
        assert!(active.queued_prompts.is_empty());
        let id = active
            .restart_recovery
            .as_ref()
            .unwrap()
            .prompt_id
            .clone()
            .unwrap();
        for session in &sessions[1..] {
            if session == &sessions[6] && inner.find_session_index(session).is_none() {
                continue; // A deleted observation root may already be retired.
            }
            let record = &inner.sessions[inner.find_session_index(session).unwrap()];
            assert!(matches!(record.runtime, SessionRuntime::None));
            assert!(
                record.queued_prompts.is_empty(),
                "idle and held roots/children get no wake"
            );
            assert!(
                record
                    .restart_recovery
                    .as_ref()
                    .is_none_or(|recovery| recovery.prompt_id.is_none()),
                "excluded sessions must not even enqueue a recovery wake"
            );
        }
        id
    };
    first.0.resume_interrupted_root_sessions_after_boot();
    assert!(
        prompts.try_recv().is_err(),
        "repeating the same recovery sends no second prompt"
    );
    // Persist while the recovery itself is active: this is still the original
    // interruption, not permission for an endless restart/resume loop.
    first.0.shutdown_persist_blocking();
    drop(first);

    let second = boot(root.path());
    let (unused_runtime, second_prompts) =
        test_acp_runtime_handle(AcpAgent::Cursor, "restart-recovery-second");
    second
        .0
        .install_test_acp_runtime_override(AcpAgent::Cursor, unused_runtime);
    second.0.run_post_listen_boot().unwrap();
    assert!(second_prompts.try_recv().is_err());
    let inner = second.0.inner.lock().unwrap();
    let active = &inner.sessions[inner.find_session_index(&sessions[0]).unwrap()];
    assert_eq!(
        active
            .restart_recovery
            .as_ref()
            .unwrap()
            .prompt_id
            .as_deref(),
        Some(recovery_id.as_str())
    );
    assert!(active.queued_prompts.is_empty());
    assert!(matches!(active.runtime, SessionRuntime::None));
}

#[test]
fn recovery_keeps_expired_approval_and_queued_work() {
    for interrupted_status in [SessionStatus::Approval, SessionStatus::Stopping] {
        let root = TestTempRoot::create("restart-recovery-approval");
        let mut inner = StateInner::new();
        let session = inner
            .create_session(
                Agent::Cursor,
                None,
                root.path().to_string_lossy().into_owned(),
                None,
                None,
            )
            .session
            .id
            .clone();
        let approval_id = inner.next_message_id();
        let queued_id = inner.next_message_id();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.session.status = interrupted_status;
        push_message_on_record(
            record,
            Message::Approval {
                id: approval_id.clone(),
                timestamp: stamp_now(),
                author: Author::Assistant,
                title: "Approval".to_owned(),
                command: "Edit file".to_owned(),
                command_language: None,
                detail: "Needs approval".to_owned(),
                decision: ApprovalDecision::Pending,
                supported_decisions: None,
            },
        );
        queue_prompt_on_record(
            record,
            PendingPrompt {
                id: queued_id.clone(),
                timestamp: stamp_now(),
                text: "queued work".to_owned(),
                expanded_text: None,
                source: None,
                attachments: Vec::new(),
                engram_interrupted: false,
                is_engram_retained: false,
            },
            Vec::new(),
        );
        persist_state(&root.path().join("termal.sqlite"), &inner).unwrap();
        drop(inner);

        let state = boot(root.path());
        let (runtime, prompts) =
            test_acp_runtime_handle(AcpAgent::Cursor, "restart-recovery-approval");
        state
            .0
            .install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
        state.0.run_post_listen_boot().unwrap();
        let AcpRuntimeCommand::Prompt(command) =
            phase_sync::receive(&prompts, "approval recovery is a fresh ordinary prompt")
        else {
            panic!("recovery should send a prompt");
        };
        assert!(
            command
                .prompt
                .contains(if interrupted_status == SessionStatus::Approval {
                    "That request expired"
                } else {
                    "TermAl restarted while this session was stopping"
                })
        );
        assert!(command.prompt.contains("Queued prompts saved: 1"));
        let mut inner = state.0.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        assert!(record.session.messages.iter().any(|message| matches!(message,
        Message::Approval { id, decision: ApprovalDecision::Interrupted, .. } if id == &approval_id)));
        assert_eq!(
            record.queued_prompts.front().unwrap().pending_prompt.id,
            queued_id
        );
    }
}
