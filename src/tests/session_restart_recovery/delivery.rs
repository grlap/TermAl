//! Queue-drain rejection recovery and the held remote-Resume boundary.
use super::*;

#[test]
fn recovery_rejected_after_leading_mail_is_saved_for_the_next_boot() {
    let root = TestTempRoot::create("restart-recovery-after-mail-rejection");
    let mut inner = StateInner::new();
    let target = active_root(&mut inner, root.path());
    let sender = active_root(&mut inner, root.path());
    let index = inner.find_session_index(&sender).unwrap();
    inner.session_mut_by_index(index).unwrap().session.status = SessionStatus::Idle;
    let path = root.path().join("termal.sqlite");
    persist_state(&path, &inner).unwrap();
    drop(inner);
    let mailbox = MailboxStore::open(&resolve_coordination_persistence_path(&path))
        .unwrap()
        .append(&MailboxAppendInput {
            sender_session_id: sender,
            sender_name: "Sender".to_owned(),
            target_session_id: target.clone(),
            target_name: "Target".to_owned(),
            body: "Read the correction before recovery".to_owned(),
            idempotency_key: "mail-before-rejected-recovery".to_owned(),
            topic: None,
            state_stamp: None,
        })
        .unwrap();
    let first = boot(root.path());
    let (runtime, prompts) = test_acp_runtime_handle(AcpAgent::Cursor, "mail-before-rejection");
    first
        .0
        .install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
    let (settled_tx, settled_rx) = mpsc::channel();
    TEST_QUEUED_PROMPT_SETTLEMENT.with(|slot| {
        *slot.borrow_mut() = Some(Arc::new(move |phase| {
            if phase == "released" {
                settled_tx.send(()).unwrap();
            }
        }));
    });
    first.0.run_post_listen_boot().unwrap();
    let AcpRuntimeCommand::Prompt(mail) =
        phase_sync::receive(&prompts, "leading mailbox wake reaches the runtime")
    else {
        panic!("expected leading mail");
    };
    assert!(mail.prompt.contains(&mailbox.mailbox_id));
    phase_sync::receive(
        &settled_rx,
        "recovery ACK drain finishes while leading mail is active",
    );
    // Read and acknowledge that mail so the next boot tests the saved
    // recovery, rather than another legitimate wake for unread mail.
    first
        .0
        .mailbox_store
        .read_range(&target, &mailbox.mailbox_id, None, 10)
        .unwrap();
    first
        .0
        .mailbox_store
        .acknowledge(&target, &mailbox.mailbox_id, 0, mailbox.sequence)
        .unwrap();
    let (token, original_id, original_text) = {
        let inner = first.0.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&target).unwrap()];
        let recovery = record.queued_prompts.front().unwrap();
        assert_eq!(record.queued_prompts.len(), 1);
        (
            record.runtime.runtime_token().unwrap(),
            recovery.pending_prompt.id.clone(),
            recovery.pending_prompt.text.clone(),
        )
    };
    drop(prompts); // The next send, from the real completion drain, is rejected.
    let error = first
        .0
        .finish_turn_ok_if_runtime_matches(&target, &token)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("failed to deliver queued turn dispatch")
    );
    let replacement_id = {
        let inner = first.0.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&target).unwrap()];
        assert_eq!(record.session.status, SessionStatus::Error);
        assert_eq!(record.queued_prompts.len(), 1);
        let entry = &record.queued_prompts[0];
        assert_eq!(entry.pending_prompt.text, original_text);
        assert_ne!(entry.pending_prompt.id, original_id);
        assert_eq!(
            record.restart_recovery.as_ref().unwrap().prompt_id.as_ref(),
            Some(&entry.pending_prompt.id)
        );
        entry.pending_prompt.id.clone()
    };
    drop(first);

    let second = boot(root.path());
    let (runtime, prompts) =
        test_acp_runtime_handle(AcpAgent::Cursor, "after-completion-rejection");
    second
        .0
        .install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
    second.0.run_post_listen_boot().unwrap();
    let AcpRuntimeCommand::Prompt(recovery) = phase_sync::receive(
        &prompts,
        "the completion drain's rejected recovery survives boot",
    ) else {
        panic!("expected retained recovery");
    };
    assert_eq!(recovery.prompt, original_text);
    finish_current_turn(&second.0, &target);
    assert!(prompts.try_recv().is_err());
    let inner = second.0.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&target).unwrap()];
    assert!(record.queued_prompts.is_empty());
    assert!(record.queued_prompt_durability_holds.is_empty());
    assert_eq!(
        record.restart_recovery.as_ref().unwrap().prompt_id.as_ref(),
        Some(&replacement_id)
    );
}

#[test]
fn restart_recovery_does_not_resume_a_held_remote_prompt() {
    let root = TestTempRoot::create("restart-recovery-held-remote-resume");
    let mut inner = StateInner::new();
    let session = active_root(&mut inner, root.path());
    let index = inner.find_session_index(&session).unwrap();
    let record = inner.session_mut_by_index(index).unwrap();
    let source = remote_prompt_journal::intent("held-with-recovery", "Held remote request");
    let (basis, reference, key, text) = source.components();
    let mut basis = basis.clone();
    basis.target = remote_prompt_authority::NativeRoot::new(
        Uuid::from_u128(2),
        1,
        session.clone(),
        Uuid::parse_str(&record.federation_native_creation_id).unwrap(),
    )
    .unwrap();
    let intent =
        remote_prompt_authority::Intent::new(basis, reference, key.into(), text.into()).unwrap();
    let remote_id = RemotePromptPrimaryAssociation::new(&intent)
        .unwrap()
        .prompt_id();
    queue_receipted_orchestrator_prompt_on_record(record, pending(&remote_id, text), Vec::new());
    associate_remote_prompt_receipt(record, &intent).unwrap();
    persist_state(&root.path().join("termal.sqlite"), &inner).unwrap();
    drop(inner);

    let state = boot(root.path());
    let (runtime, prompts) = test_acp_runtime_handle(AcpAgent::Cursor, "held-remote-recovery");
    state
        .0
        .install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
    state.0.run_post_listen_boot().unwrap();
    let AcpRuntimeCommand::Prompt(recovery) =
        phase_sync::receive(&prompts, "the interrupted root receives its own recovery")
    else {
        panic!("expected root recovery");
    };
    assert!(recovery.prompt.contains("[TermAl restart recovery]"));
    assert!(!recovery.prompt.contains("Held remote request"));
    finish_current_turn(&state.0, &session);
    let marker_id = {
        let mut inner = state.0.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(record.queued_prompts[0].pending_prompt.id, remote_id);
        record.set_auto_dispatch_blocked(true);
        let id = record.restart_recovery.as_ref().unwrap().prompt_id.clone();
        state.0.commit_locked(&mut inner).unwrap();
        id
    };
    let refusal = match state.0.resume_local_session_queue(&session, None) {
        Err(refusal) => refusal,
        Ok(_) => panic!("held remote Resume must be refused"),
    };
    assert_eq!(refusal.status, StatusCode::CONFLICT);
    assert_eq!(
        refusal.message,
        "This remote prompt is held and cannot be resumed"
    );
    assert!(prompts.try_recv().is_err());
    let inner = state.0.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(record.orchestrator_auto_dispatch_blocked);
    assert_eq!(record.queued_prompts.len(), 1);
    assert_eq!(record.queued_prompts[0].pending_prompt.id, remote_id);
    assert_eq!(
        record.prompt_deliveries[&remote_id].disposition,
        PromptDeliveryDisposition::Pending
    );
    assert_eq!(record.prompt_deliveries[&remote_id].attempt, 0);
    assert_eq!(
        record.restart_recovery.as_ref().unwrap().prompt_id,
        marker_id
    );
}
