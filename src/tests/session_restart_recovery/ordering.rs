//! Persisted mailbox order, ordinary queue draining and distinct interruptions.
use super::*;

#[test]
fn restarting_during_leading_mail_keeps_the_original_queued_recovery() {
    let root = TestTempRoot::create("restart-recovery-mail-active");
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
            body: "Read this before continuing".to_owned(),
            idempotency_key: "mail-active-across-restart".to_owned(),
            topic: None,
            state_stamp: None,
        })
        .unwrap();
    let first = boot(root.path());
    let (runtime, prompts) = test_acp_runtime_handle(AcpAgent::Cursor, "mail-active-first");
    first
        .0
        .install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
    first.0.run_post_listen_boot().unwrap();
    let AcpRuntimeCommand::Prompt(mail) =
        phase_sync::receive(&prompts, "mail runs before the queued recovery")
    else {
        panic!("expected leading mail");
    };
    assert!(mail.prompt.contains(&mailbox.mailbox_id));
    let recovery_id = {
        let inner = first.0.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&target).unwrap()];
        assert_eq!(record.session.status, SessionStatus::Active);
        assert_eq!(record.queued_prompts.len(), 1);
        let id = record.queued_prompts[0].pending_prompt.id.clone();
        assert_eq!(
            record.restart_recovery.as_ref().unwrap().prompt_id.as_ref(),
            Some(&id)
        );
        id
    };
    drop(first); // Persist Active mail while the original recovery is still queued.

    let second = boot(root.path());
    let (runtime, prompts) = test_acp_runtime_handle(AcpAgent::Cursor, "mail-active-second");
    second
        .0
        .install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
    second.0.run_post_listen_boot().unwrap();
    let AcpRuntimeCommand::Prompt(mail) = phase_sync::receive(
        &prompts,
        "unread mail is recovered again before the same opening",
    ) else {
        panic!("expected restored mail");
    };
    assert!(mail.prompt.contains(&mailbox.mailbox_id));
    {
        let inner = second.0.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&target).unwrap()];
        assert_eq!(
            record.queued_prompts.len(),
            1,
            "no second recovery was minted"
        );
        assert_eq!(record.queued_prompts[0].pending_prompt.id, recovery_id);
        assert_eq!(
            record.restart_recovery.as_ref().unwrap().prompt_id.as_ref(),
            Some(&recovery_id)
        );
    }
    finish_current_turn(&second.0, &target);
    let AcpRuntimeCommand::Prompt(recovery) =
        phase_sync::receive(&prompts, "the original queued opening runs once")
    else {
        panic!("expected recovery");
    };
    assert!(recovery.prompt.contains("[TermAl restart recovery]"));
    finish_current_turn(&second.0, &target);
    assert!(prompts.try_recv().is_err());
}

#[test]
fn persisted_mailbox_precedes_recovery_and_saved_work_then_queue_drains() {
    let root = TestTempRoot::create("restart-recovery-mail-order");
    let mut inner = StateInner::new();
    let target = active_root(&mut inner, root.path());
    let sender = active_root(&mut inner, root.path());
    let sender_index = inner.find_session_index(&sender).unwrap();
    inner
        .session_mut_by_index(sender_index)
        .unwrap()
        .session
        .status = SessionStatus::Idle;
    let index = inner.find_session_index(&target).unwrap();
    queue_prompt_on_record(
        inner.session_mut_by_index(index).unwrap(),
        pending("saved-work", "Continue the saved request"),
        Vec::new(),
    );
    let path = root.path().join("termal.sqlite");
    persist_state(&path, &inner).unwrap();
    drop(inner);
    let mailbox = {
        let store = MailboxStore::open(&resolve_coordination_persistence_path(&path)).unwrap();
        store
            .append(&MailboxAppendInput {
                sender_session_id: sender,
                sender_name: "Sender".to_owned(),
                target_session_id: target.clone(),
                target_name: "Target".to_owned(),
                body: "A correction to read before resuming saved work".to_owned(),
                idempotency_key: "persisted-recovery-order".to_owned(),
                topic: None,
                state_stamp: None,
            })
            .unwrap()
    };

    let state = boot(root.path());
    let (runtime, prompts) = test_acp_runtime_handle(AcpAgent::Cursor, "mail-before-recovery");
    state
        .0
        .install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
    state.0.run_post_listen_boot().unwrap();
    let AcpRuntimeCommand::Prompt(mail) =
        phase_sync::receive(&prompts, "restored mail precedes recovery")
    else {
        panic!("expected mailbox prompt");
    };
    assert!(mail.prompt.contains(&mailbox.mailbox_id));
    assert!(!mail.prompt.contains("[TermAl restart recovery]"));
    {
        let inner = state.0.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&target).unwrap()];
        assert_eq!(record.queued_prompts.len(), 2);
        assert_eq!(record.queued_prompts[1].pending_prompt.id, "saved-work");
    }
    finish_current_turn(&state.0, &target);
    let AcpRuntimeCommand::Prompt(recovery) =
        phase_sync::receive(&prompts, "recovery follows restored mail")
    else {
        panic!("expected recovery prompt");
    };
    assert!(recovery.prompt.contains("[TermAl restart recovery]"));
    finish_current_turn(&state.0, &target);
    let AcpRuntimeCommand::Prompt(work) =
        phase_sync::receive(&prompts, "ordinary saved work drains after recovery")
    else {
        panic!("expected saved work");
    };
    assert_eq!(work.prompt, "Continue the saved request");
    let inner = state.0.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&target).unwrap()];
    assert!(
        record.restart_recovery.is_none(),
        "ordinary work owns a new interruption"
    );
    assert!(record.queued_prompts.is_empty());
}

#[test]
fn a_later_ordinary_turn_gets_its_own_recovery_after_another_restart() {
    let root = TestTempRoot::create("restart-recovery-new-interruption");
    let mut inner = StateInner::new();
    let session = active_root(&mut inner, root.path());
    let index = inner.find_session_index(&session).unwrap();
    queue_prompt_on_record(
        inner.session_mut_by_index(index).unwrap(),
        pending("new-work", "A later ordinary request"),
        Vec::new(),
    );
    persist_state(&root.path().join("termal.sqlite"), &inner).unwrap();
    drop(inner);
    let first = boot(root.path());
    let (runtime, prompts) = test_acp_runtime_handle(AcpAgent::Cursor, "first-interruption");
    first
        .0
        .install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
    first.0.run_post_listen_boot().unwrap();
    let AcpRuntimeCommand::Prompt(_) = phase_sync::receive(&prompts, "first recovery") else {
        panic!("expected recovery");
    };
    let first_id = {
        let inner = first.0.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session).unwrap()]
            .restart_recovery
            .as_ref()
            .unwrap()
            .prompt_id
            .clone()
            .unwrap()
    };
    finish_current_turn(&first.0, &session);
    let AcpRuntimeCommand::Prompt(work) = phase_sync::receive(&prompts, "later ordinary turn")
    else {
        panic!("expected work");
    };
    assert_eq!(work.prompt, "A later ordinary request");
    drop(first);

    let second = boot(root.path());
    let (runtime, prompts) = test_acp_runtime_handle(AcpAgent::Cursor, "second-interruption");
    second
        .0
        .install_test_acp_runtime_override(AcpAgent::Cursor, runtime);
    second.0.run_post_listen_boot().unwrap();
    let AcpRuntimeCommand::Prompt(recovery) =
        phase_sync::receive(&prompts, "new interruption gets one recovery")
    else {
        panic!("expected new recovery");
    };
    assert!(recovery.prompt.contains("[TermAl restart recovery]"));
    let inner = second.0.inner.lock().unwrap();
    let id = inner.sessions[inner.find_session_index(&session).unwrap()]
        .restart_recovery
        .as_ref()
        .unwrap()
        .prompt_id
        .as_ref()
        .unwrap();
    assert_ne!(id, &first_id);
}
