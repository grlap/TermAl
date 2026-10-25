// First-admission, direct-send and shared-provider terminal boundaries.
// Faults affect only disposable fixture stores and in-memory command channels.
use super::*;

#[test]
fn mailbox_relay_review_r4_only_typed_scheduling_holds_are_nonfatal() {
    assert!(consume_mailbox_relay_hold::<()>(Err(mailbox_relay_admission_held("authority unavailable")))
        .unwrap().is_none());
    assert!(consume_mailbox_relay_hold::<()>(Err(anyhow!("actual persistence failure"))).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_r4_first_admission_cancel_never_starts_successor() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let (append, receipt) = append_to_central(&fixture, "first-admission-cancel");
    let input = claude_input(&fixture);
    let wake: MailboxRelayWake = {
        let json: String = fixture.central.mailbox_store.connection().unwrap().query_row(
            "SELECT wake_json FROM mailbox_relay_wakes WHERE message_id=?1", [&receipt.message_id],
            |row|row.get(0)).unwrap();
        serde_json::from_str(&json).unwrap()
    };
    let app = fixture.central.clone();
    let session = fixture.native.clone();
    let reached = Arc::new(AtomicBool::new(false));
    let observed = reached.clone();
    TEST_MAILBOX_RELAY_AFTER_FIRST_ADMISSION.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
        observed.store(true, Ordering::SeqCst);
        let carrier = {
            let mut inner = app.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].session.status = SessionStatus::Active;
            inner.sessions[index].queued_prompts.front().unwrap().pending_prompt.id.clone()
        };
        assert!(matches!(app.dispatch_turn(&session, r2::user_prompt("unrelated first-admission successor"))
            .unwrap(), DispatchTurnResult::Queued));
        app.cancel_queued_prompt(&session, &carrier).unwrap();
        let mut inner = app.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].session.status = SessionStatus::Idle;
    })));
    // The worker invokes this entry point on a blocking thread. Drive that
    // exact production entry point here so the thread-local phase seam runs
    // on its installing thread instead of silently missing the race.
    fixture.central.queue_native_mailbox_relay_wake(&wake, None).unwrap();
    assert!(reached.load(Ordering::SeqCst), "the first-admission cancellation seam must execute");
    assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "first-admission drain must retain its exact carrier owner after Cancel");
    let retained = fixture.central.mailbox_store.mailbox_relay_notification(
        &append.target, &receipt.mailbox_id, receipt.sequence).unwrap().unwrap();
    assert!(retained.queued && !retained.handed_off);
    assert_eq!(fixture.central.inner.lock().unwrap().sessions[0].queued_prompts.len(), 1);
    // The successor is eligible, not held for an unrelated reason. A NEW
    // ordinary drain may dispatch it; only the stale relay drain must not.
    let dispatch = fixture.central.dispatch_next_queued_turn(&fixture.native, false).unwrap().unwrap();
    deliver_turn_dispatch(&fixture.central, dispatch).unwrap();
    match recv_within_guard(&input, "explicit successor drain after stale relay refusal") {
        Ok(ClaudeRuntimeCommand::Prompt(prompt)) => assert_eq!(prompt.text, "unrelated first-admission successor"),
        _ => panic!("expected the independently drained successor"),
    }
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_r4_direct_send_rechecks_late_authority_before_promotion() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for refusal in ["revoked", "disabled"] {
        let fixture = Fixture::new(false).await;
        let _handles = central_handle(&fixture);
        append_to_central(&fixture, "direct-send-late-authority");
        fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
        let input = claude_input(&fixture);
        let app = fixture.central.clone();
        let peer = fixture.channel.current_authority().unwrap();
        TEST_MAILBOX_RELAY_AFTER_SEND_REVALIDATION.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            if refusal == "revoked" {
                app.transition_federation_peer(&peer, FederationMembershipState::Revoked).unwrap();
            } else { r2::set_federation_enabled(&app, false); }
        })));
        let result = fixture.central.dispatch_turn(&fixture.native, r2::user_prompt("late-authority ordinary work"))
            .expect("late relay refusal must preserve ordinary user admission");
        let dispatch = match result {
            DispatchTurnResult::Dispatched(dispatch) | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
            DispatchTurnResult::Queued => panic!("eligible idle user must pass the newly fenced carrier"),
        };
        assert!(matches!(&dispatch, TurnDispatch::PersistentClaude { mailbox_notification: None, .. }),
            "late-refused relay cannot be promoted by direct Send");
        deliver_turn_dispatch(&fixture.central, dispatch).unwrap();
        match recv_within_guard(&input, "direct ordinary work after late refusal") {
            Ok(ClaudeRuntimeCommand::Prompt(prompt)) => assert_eq!(prompt.text, "late-authority ordinary work"),
            _ => panic!("expected the ordinary user prompt"),
        }
        assert_eq!(fixture.central.inner.lock().unwrap().sessions[0].queued_prompts.len(), 1);
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_r4_queue_marker_failure_cannot_undo_live_cancel() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let (append, receipt) = append_to_central(&fixture, "queue-marker-cancel");
    let input = claude_input(&fixture);
    fixture.central.inner.lock().unwrap().sessions[0].orchestrator_auto_dispatch_blocked = true;
    fixture.central.mailbox_store.connection().unwrap().execute_batch(
        "CREATE TRIGGER reject_relay_queue_marker BEFORE UPDATE OF queued ON mailbox_relay_notifications
         WHEN NEW.queued=1 BEGIN SELECT RAISE(FAIL,'fixture queue marker rejected'); END;"
    ).unwrap();
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let carrier = fixture.central.inner.lock().unwrap().sessions[0].queued_prompts.front()
        .map(|queued| queued.pending_prompt.id.clone());
    let canceled = if let Some(carrier) = carrier {
        fixture.central.cancel_queued_prompt(&fixture.native, &carrier).unwrap();
        true
    } else { false }; // Failure before exposure owes no successful Cancel.
    fixture.central.mailbox_store.connection().unwrap().execute_batch("DROP TRIGGER reject_relay_queue_marker;").unwrap();
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    if canceled {
        assert!(fixture.central.inner.lock().unwrap().sessions[0].queued_prompts.is_empty(),
            "a visible carrier successfully canceled after marker failure must not be recreated live");
        assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)));
    } else {
        assert_eq!(fixture.central.inner.lock().unwrap().sessions[0].queued_prompts.len(), 1,
            "a pre-exposure failed admission remains eligible for retry");
    }
    let retained = fixture.central.mailbox_store.mailbox_relay_notification(
        &append.target, &receipt.mailbox_id, receipt.sequence).unwrap().unwrap();
    assert!(!retained.handed_off);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_r4_shared_codex_completed_relay_hold_keeps_reader_and_sibling_live() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    append_to_central(&fixture, "shared-completed-relay-hold");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let sibling = test_session_id(&fixture.central, Agent::Codex);
    let process = phase_sync::ParkedProcess::spawn();
    let (runtime, _commands, _) = test_shared_codex_runtime_with_process(
        "relay-terminal-shared-runtime", process.process.clone());
    *fixture.central.shared_codex_runtime_slot(SharedCodexProfile::Default).lock().unwrap() = Some(runtime.clone());
    for (session, thread, turn) in [(&fixture.native, "relay-terminal-thread", "relay-terminal-turn"),
                                   (&sibling, "relay-sibling-thread", "relay-sibling-turn")] {
        {
            let mut inner = fixture.central.inner.lock().unwrap();
            let index = inner.find_session_index(session).unwrap();
            let record = &mut inner.sessions[index];
            record.runtime = SessionRuntime::Codex(spawn_codex_runtime(
                fixture.central.clone(), session.clone(), record.session.workdir.clone(),
                SharedCodexProfile::Default).unwrap());
            record.attach_to_shared_codex_profile(SharedCodexProfile::Default);
            record.session.status = SessionStatus::Active;
            record.active_turn_generation = 7;
            record.orchestrator_auto_dispatch_blocked = false;
            set_record_external_session_id(record, Some(thread.into()));
        }
        runtime.sessions.lock().unwrap().insert(session.clone(), SharedCodexSessionState {
            thread_id: Some(thread.into()), turn_id: Some(turn.into()), active_turn_generation: Some(7),
            turn_started: true, ..Default::default()
        });
        runtime.thread_sessions.lock().unwrap().insert(thread.into(), session.clone());
    }
    fixture.central.transition_federation_peer(&fixture.channel.current_authority().unwrap(),
        FederationMembershipState::Revoked).unwrap();
    let pending = Arc::new(Mutex::new(HashMap::new()));
    for (thread, turn) in [("relay-terminal-thread", "relay-terminal-turn"),
                          ("relay-sibling-thread", "relay-sibling-turn")] {
        let result = handle_shared_codex_app_server_message(
            &json!({"method":"turn/completed", "params":{"threadId":thread,
                "turn":{"id":turn,"error":null}}}), &fixture.central, &runtime.runtime_id,
            &pending, &runtime.sessions, &runtime.thread_sessions, &runtime.input_tx);
        assert!(result.is_ok(), "expected relay hold must not escape into the shared-reader fatal branch: {result:?}");
    }
    {
        let inner = fixture.central.inner.lock().unwrap();
        for session in [&fixture.native, &sibling] {
            let record = &inner.sessions[inner.find_session_index(session).unwrap()];
            assert_eq!(record.session.status, SessionStatus::Idle);
            assert!(record.runtime.matches_runtime_token(&RuntimeToken::Codex(runtime.runtime_id.clone())));
        }
        assert_eq!(inner.sessions[0].queued_prompts.len(), 1, "refused relay remains owed");
    }
    assert!(fixture.central.shared_codex_runtime_slot(SharedCodexProfile::Default).lock().unwrap().is_some());
    fixture.finish().await;
}
