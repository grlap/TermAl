//! Restart and failure paths retain the identity of the original recovery.
use super::*;

#[test]
fn a_recovery_rejected_after_its_real_durability_ack_survives_the_next_boot() {
    let root = TestTempRoot::create("restart-recovery-ack-rejection");
    let mut inner = StateInner::new();
    let session = active_root(&mut inner, root.path());
    persist_state(&root.path().join("termal.sqlite"), &inner).unwrap();
    drop(inner);

    let first = boot(root.path());
    let (runtime, closed_prompts) = test_acp_runtime_handle(AcpAgent::Cursor, "reject-recovery");
    drop(closed_prompts); // The real send proves that no command was accepted.
    first
        .0
        .install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
    let (ack_tx, ack_rx) = mpsc::channel();
    let (released_tx, released_rx) = mpsc::channel();
    let observed_state = first.0.clone();
    let observed_session = session.clone();
    TEST_QUEUED_PROMPT_SETTLEMENT.with(|slot| {
        *slot.borrow_mut() = Some(Arc::new(move |phase| match phase {
            "acknowledged" => {
                let inner = observed_state.inner.lock().unwrap();
                let record = &inner.sessions[inner.find_session_index(&observed_session).unwrap()];
                let entry = record.queued_prompts.front().unwrap();
                assert!(
                    record.queued_prompt_durability_holds[&entry.pending_prompt.id].acknowledged
                );
                ack_tx
                    .send((
                        entry.pending_prompt.id.clone(),
                        entry.pending_prompt.text.clone(),
                    ))
                    .unwrap();
            }
            "released" => {
                released_tx.send(()).unwrap();
            }
            _ => {}
        }));
    });
    // Use the boot producer and real persist worker's content fence, with no
    // orphan-dispatch pass that could mask the initial ACK drain's rejection.
    first.0.resume_interrupted_root_sessions_after_boot();
    let (rejected_id, original_text) =
        phase_sync::receive(&ack_rx, "the real recovery fence acknowledges");
    phase_sync::receive(
        &released_rx,
        "the ACK drain finishes its rejected runtime send",
    );
    let requeued_id = {
        let inner = first.0.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(record.session.status, SessionStatus::Error);
        assert!(matches!(record.runtime, SessionRuntime::None));
        assert_eq!(
            record.queued_prompts.len(),
            1,
            "the proven-unsent opening is retained"
        );
        let entry = &record.queued_prompts[0];
        assert_eq!(entry.pending_prompt.text, original_text);
        assert_ne!(entry.pending_prompt.id, rejected_id);
        assert_eq!(
            record.restart_recovery.as_ref().unwrap().prompt_id.as_ref(),
            Some(&entry.pending_prompt.id)
        );
        entry.pending_prompt.id.clone()
    };
    drop(first); // Flush the requeued identity and marker before the second boot.

    let second = boot(root.path());
    let (runtime, prompts) = test_acp_runtime_handle(AcpAgent::Cursor, "deliver-retained-recovery");
    second
        .0
        .install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
    second.0.run_post_listen_boot().unwrap();
    let AcpRuntimeCommand::Prompt(command) =
        phase_sync::receive(&prompts, "the retained recovery reaches a fresh runtime")
    else {
        panic!("expected retained recovery");
    };
    assert_eq!(command.prompt, original_text);
    second.0.resume_interrupted_root_sessions_after_boot();
    second.0.dispatch_orphaned_workflow_prompts();
    assert!(
        prompts.try_recv().is_err(),
        "accepted recovery is not replayed"
    );
    {
        let inner = second.0.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(
            record.restart_recovery.as_ref().unwrap().prompt_id.as_ref(),
            Some(&requeued_id)
        );
        assert!(record.queued_prompts.is_empty());
    }
    drop(second);

    let third = boot(root.path());
    let (runtime, prompts) =
        test_acp_runtime_handle(AcpAgent::Cursor, "no-replay-after-acceptance");
    third
        .0
        .install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
    third.0.run_post_listen_boot().unwrap();
    assert!(
        prompts.try_recv().is_err(),
        "another boot cannot replay the accepted opening"
    );
    let inner = third.0.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(
        record.restart_recovery.as_ref().unwrap().prompt_id.as_ref(),
        Some(&requeued_id)
    );
    assert!(record.queued_prompts.is_empty());
    assert!(record.queued_prompt_durability_holds.is_empty());
}

#[test]
fn an_unpaused_restart_recovers_the_root_without_replaying_a_settled_receipt() {
    for disposition in [
        PromptDeliveryDisposition::Accepted,
        PromptDeliveryDisposition::Rejected,
    ] {
        let root = TestTempRoot::create("restart-recovery-settled-receipt");
        let mut inner = StateInner::new();
        let session = active_root(&mut inner, root.path());
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        queue_receipted_orchestrator_prompt_on_record(
            record,
            pending("settled-original", "Original receipted request"),
            Vec::new(),
        );
        let original = record.queued_prompts.pop_front().unwrap();
        let delivery = record
            .prompt_deliveries
            .get_mut("settled-original")
            .unwrap();
        delivery.disposition = disposition;
        delivery.entry = (disposition == PromptDeliveryDisposition::Rejected).then_some(original);
        sync_pending_prompts(record);
        persist_state(&root.path().join("termal.sqlite"), &inner).unwrap();
        drop(inner);

        let state = boot(root.path());
        let (runtime, prompts) =
            test_acp_runtime_handle(AcpAgent::Cursor, "settled-receipt-recovery");
        state
            .0
            .install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
        state.0.run_post_listen_boot().unwrap();
        let AcpRuntimeCommand::Prompt(command) =
            phase_sync::receive(&prompts, "unpaused root gets a distinct recovery opening")
        else {
            panic!("expected root recovery");
        };
        assert!(command.prompt.contains("[TermAl restart recovery]"));
        assert!(!command.prompt.contains("Original receipted request"));
        {
            let inner = state.0.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert_eq!(
                record.prompt_deliveries["settled-original"].disposition,
                disposition
            );
            assert_ne!(
                record
                    .restart_recovery
                    .as_ref()
                    .unwrap()
                    .prompt_id
                    .as_deref(),
                Some("settled-original")
            );
            assert!(
                record.queued_prompts.is_empty(),
                "the old receipt is not requeued"
            );
        }
        finish_current_turn(&state.0, &session);
        assert!(
            prompts.try_recv().is_err(),
            "completing recovery must not replay the old receipt"
        );
        let inner = state.0.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(
            record.prompt_deliveries["settled-original"].disposition,
            disposition
        );
        assert!(record.queued_prompts.is_empty());
    }
}

#[test]
fn an_undelivered_saved_recovery_is_delivered_once_after_another_boot() {
    let root = TestTempRoot::create("restart-recovery-queued");
    let mut inner = StateInner::new();
    let session = active_root(&mut inner, root.path());
    let index = inner.find_session_index(&session).unwrap();
    let record = inner.session_mut_by_index(index).unwrap();
    record.session.status = SessionStatus::Error;
    record.restart_recovery = Some(marker(Some("saved-recovery")));
    queue_durable_orchestrator_prompt_on_record(
        record,
        pending("saved-recovery", "The saved recovery opening"),
        Vec::new(),
    );
    persist_state(&root.path().join("termal.sqlite"), &inner).unwrap();
    drop(inner);

    let state = boot(root.path());
    let (runtime, prompts) = test_acp_runtime_handle(AcpAgent::Cursor, "saved-recovery");
    state
        .0
        .install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
    state.0.run_post_listen_boot().unwrap();
    let AcpRuntimeCommand::Prompt(command) =
        phase_sync::receive(&prompts, "saved recovery reaches its runtime")
    else {
        panic!("expected saved recovery");
    };
    assert_eq!(command.prompt, "The saved recovery opening");
    state.0.resume_interrupted_root_sessions_after_boot();
    state.0.dispatch_orphaned_workflow_prompts();
    assert!(
        prompts.try_recv().is_err(),
        "the saved recovery is not duplicated"
    );
    let inner = state.0.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(
        record
            .restart_recovery
            .as_ref()
            .unwrap()
            .prompt_id
            .as_deref(),
        Some("saved-recovery")
    );
    assert!(record.queued_prompts.is_empty());
}

#[test]
fn rejected_recovery_requeues_keep_the_marker_through_the_next_restart() {
    for receipted in [false, true] {
        let root = TestTempRoot::create("restart-recovery-rejected");
        let mut inner = StateInner::new();
        let session = active_root(&mut inner, root.path());
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.session.status = SessionStatus::Error;
        record.restart_recovery = Some(marker(Some("rejected-recovery")));
        let prompt = pending("rejected-recovery", "The rejected recovery opening");
        if receipted {
            queue_receipted_orchestrator_prompt_on_record(record, prompt, Vec::new());
        } else {
            queue_orchestrator_prompt_on_record(record, prompt, Vec::new());
        }
        let rejected = record.queued_prompts.pop_front().unwrap();
        if receipted {
            let delivery = record
                .prompt_deliveries
                .get_mut("rejected-recovery")
                .unwrap();
            delivery.disposition = PromptDeliveryDisposition::Rejected;
            delivery.entry = Some(rejected.clone());
        }
        sync_pending_prompts(record);
        persist_state(&root.path().join("termal.sqlite"), &inner).unwrap();
        drop(inner);

        let first = boot(root.path());
        let (runtime, prompts) = test_acp_runtime_handle(AcpAgent::Cursor, "requeued-recovery");
        first
            .0
            .install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
        first
            .0
            .requeue_rejected_orphaned_workflow_prompt(&session, rejected)
            .unwrap();
        first.0.run_post_listen_boot().unwrap();
        let AcpRuntimeCommand::Prompt(command) =
            phase_sync::receive(&prompts, "requeued recovery is delivered")
        else {
            panic!("expected requeued recovery");
        };
        assert_eq!(command.prompt, "The rejected recovery opening");
        let new_id = {
            let inner = first.0.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            let new_id = record
                .restart_recovery
                .as_ref()
                .unwrap()
                .prompt_id
                .clone()
                .unwrap();
            assert_ne!(new_id, "rejected-recovery");
            assert!(record.queued_prompts.is_empty());
            new_id
        };
        drop(first);

        let second = boot(root.path());
        let (runtime, prompts) =
            test_acp_runtime_handle(AcpAgent::Cursor, "after-requeued-recovery");
        second
            .0
            .install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
        second.0.run_post_listen_boot().unwrap();
        assert!(
            prompts.try_recv().is_err(),
            "restart of requeued recovery must not replay it"
        );
        let inner = second.0.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_eq!(
            record.restart_recovery.as_ref().unwrap().prompt_id.as_ref(),
            Some(&new_id)
        );
        assert!(record.queued_prompts.is_empty());
    }
}

#[test]
fn a_failed_ordinary_promotion_restores_the_previous_recovery_marker() {
    let state = test_app_state();
    let session = {
        let mut inner = state.inner.lock().unwrap();
        let session = active_root(&mut inner, FsPath::new(&state.default_workdir));
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.session.status = SessionStatus::Idle;
        record.restart_recovery = Some(marker(Some("completed-recovery")));
        queue_prompt_on_record(
            record,
            pending("ordinary-successor", "A distinct later request"),
            Vec::new(),
        );
        // A real commit error after promotion, before any runtime handoff.
        inner.revision = u64::MAX;
        session
    };
    let (runtime, prompts) = test_acp_runtime_handle(AcpAgent::Cursor, "rollback-recovery-marker");
    state.install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
    assert!(state.dispatch_next_queued_turn(&session, false).is_err());
    assert!(prompts.try_recv().is_err());
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(
        record
            .restart_recovery
            .as_ref()
            .unwrap()
            .prompt_id
            .as_deref(),
        Some("completed-recovery")
    );
    assert_eq!(
        record.queued_prompts.front().unwrap().pending_prompt.id,
        "ordinary-successor"
    );
    assert_eq!(record.session.status, SessionStatus::Idle);
}

#[test]
fn a_recovery_commit_error_still_requests_the_exact_content_fence() {
    let mut state = test_app_state();
    let (tx, requests) = mpsc::channel();
    state.persist_tx = tx;
    let session = {
        let mut inner = state.inner.lock().unwrap();
        let session = active_root(&mut inner, FsPath::new(&state.default_workdir));
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.session.status = SessionStatus::Error;
        record.restart_recovery = Some(marker(None));
        inner.revision = u64::MAX;
        session
    };
    let (settled_tx, settled_rx) = mpsc::channel();
    TEST_QUEUED_PROMPT_SETTLEMENT.with(|slot| {
        *slot.borrow_mut() = Some(Arc::new(move |phase| {
            if phase == "released" {
                let _ = settled_tx.send(());
            }
        }));
    });
    state.resume_interrupted_root_sessions_after_boot();
    let PersistRequest::Fence(fence) = phase_sync::receive(
        &requests,
        "recovery requests a fence despite the commit error",
    ) else {
        panic!("expected content fence");
    };
    let id = {
        let mut inner = state.inner.lock().unwrap();
        inner.revision = 0;
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        let id = record
            .restart_recovery
            .as_ref()
            .unwrap()
            .prompt_id
            .clone()
            .unwrap();
        let hold = &record.queued_prompt_durability_holds[&id];
        assert!(!hold.acknowledged);
        assert!(
            matches!(&fence.target, PersistFenceTarget::QueuedPromptDurability {
            session_id, prompt_id, content,
        } if session_id == &session && prompt_id == &id
            && content == &queued_prompt_durability_fence_content(&hold.nonce, &hold.content))
        );
        // The fake writer resolves only this exact request; a user pause
        // prevents its ACK from attempting a runtime handoff.
        record.set_auto_dispatch_blocked(true);
        id
    };
    fence.finish(Ok(()));
    phase_sync::receive(
        &settled_rx,
        "the exact fence settles and its release attempt finishes",
    );
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(record.queued_prompt_durability_holds[&id].acknowledged);
    assert_eq!(record.queued_prompts.front().unwrap().pending_prompt.id, id);
    assert!(matches!(record.runtime, SessionRuntime::None));
}
