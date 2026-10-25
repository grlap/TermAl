// Exact phase-cut controls for relay queue ownership, acknowledgement and
// ordinary-work isolation. Disposable stores only; no live process is stopped.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_r3_committed_home_local_wake_survives_only_link_loss() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for after_promotion in [false, true] {
        let fixture = Fixture::new(false).await;
        let _handles = central_handle(&fixture);
        append_to_central(&fixture, "committed-home-offline");
        fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
        let input = claude_input(&fixture);
        let dispatch = after_promotion.then(|| fixture.central.dispatch_next_queued_turn(
            &fixture.native, false
        ).unwrap().unwrap());
        drop(fixture.attested.lock().unwrap().take());
        let peer = fixture.central.federation_peer_snapshot(&fixture.channel.snapshot.peer.host_id).unwrap();
        assert_eq!(peer.membership, FederationMembershipState::Active);
        assert!(fixture.channel.current_lease().is_err());
        let dispatch = dispatch.unwrap_or_else(|| fixture.central.dispatch_next_queued_turn(
            &fixture.native, false
        ).unwrap().unwrap());
        deliver_turn_dispatch(&fixture.central, dispatch).unwrap();
        assert!(matches!(recv_within_guard(&input, "committed home-local offline wake"),
            Ok(ClaudeRuntimeCommand::Prompt(_))));
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_r3_unpaused_ordinary_prompt_runs_ahead_of_fenced_carrier() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for refusal in ["revoked", "missing", "disabled"] {
        let fixture = Fixture::new(false).await;
        let _handles = central_handle(&fixture);
        append_to_central(&fixture, "fenced-before-local-work");
        fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
        let input = claude_input(&fixture);
        match refusal {
            "revoked" => fixture.central.transition_federation_peer(
                &fixture.channel.current_authority().unwrap(), FederationMembershipState::Revoked
            ).unwrap(),
            "disabled" => r2::set_federation_enabled(&fixture.central, false),
            _ => { fixture.central.mailbox_store.connection().unwrap().execute(
                "DELETE FROM mailbox_relay_notifications", []
            ).unwrap(); }
        }
        let result = fixture.central.dispatch_turn(&fixture.native, r2::user_prompt("ordinary local work"))
            .expect("ordinary work cannot be refused by an unrelated relay");
        let dispatch = match result {
            DispatchTurnResult::Dispatched(dispatch) | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
            DispatchTurnResult::Queued => panic!("{refusal}: an idle unpaused root must run ordinary work"),
        };
        deliver_turn_dispatch(&fixture.central, dispatch).unwrap();
        match recv_within_guard(&input, "ordinary prompt ahead of held relay") {
            Ok(ClaudeRuntimeCommand::Prompt(prompt)) => assert_eq!(prompt.text, "ordinary local work"),
            _ => panic!("expected ordinary prompt"),
        }
        assert_eq!(fixture.central.inner.lock().unwrap().sessions[0].queued_prompts.len(), 1,
            "the fenced metadata carrier remains owed, not silently canceled");
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_r3_ack_before_rejected_first_handoff_remains_owed() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let (append, receipt) = append_to_central(&fixture, "ack-before-rejected-first-handoff");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    acknowledge_at_home(&fixture, &append, &receipt);
    drop(claude_input(&fixture)); // close the actual provider command receiver
    let dispatch = fixture.central.dispatch_next_queued_turn(&fixture.native, false).unwrap().unwrap();
    assert!(!matches!(deliver_turn_dispatch(&fixture.central, dispatch), TurnDispatchDeliveryOutcome::Delivered));
    let retained = fixture.central.mailbox_store.mailbox_relay_notification(
        &append.target, &receipt.mailbox_id, receipt.sequence
    ).unwrap().unwrap();
    assert!(retained.acknowledged && !retained.handed_off);
    let queued = fixture.central.inner.lock().unwrap().sessions[0].queued_prompts.len();
    assert_eq!(queued, 1, "a rejected first enqueue cannot cancel an acknowledged continuation");
    let input = claude_input(&fixture);
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    match recv_within_guard(&input, "recovered acknowledged continuation") {
        Ok(ClaudeRuntimeCommand::Prompt(prompt)) => {
            assert!(!prompt.text.contains(&append.message));
            assert!(prompt.text.contains("already acknowledged"));
        }
        _ => panic!("expected recovered continuation"),
    }
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_r3_cancel_between_redrive_capture_and_drain_never_starts_successor() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let (append, receipt) = append_to_central(&fixture, "redrive-owner-cancel");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let input = claude_input(&fixture);
    fixture.central.inner.lock().unwrap().sessions[0].session.status = SessionStatus::Active;
    assert!(matches!(fixture.central.dispatch_turn(&fixture.native, r2::user_prompt("unrelated successor"))
        .unwrap(), DispatchTurnResult::Queued));
    let carrier = {
        let mut inner = fixture.central.inner.lock().unwrap();
        inner.sessions[0].session.status = SessionStatus::Idle;
        inner.sessions[0].queued_prompts.front().unwrap().pending_prompt.id.clone()
    };
    let app = fixture.central.clone();
    let session = fixture.native.clone();
    TEST_MAILBOX_RELAY_BEFORE_REDRIVE.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
        app.cancel_queued_prompt(&session, &carrier).unwrap();
    })));
    let retained = fixture.central.mailbox_store.mailbox_relay_notification(
        &append.target, &receipt.mailbox_id, receipt.sequence
    ).unwrap().unwrap();
    fixture.central.queue_native_mailbox_relay_wake(&retained.wake, None).unwrap();
    assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "a stale relay redrive must never deliver the successor exposed by Cancel");
    assert_eq!(fixture.central.inner.lock().unwrap().sessions[0].queued_prompts.len(), 1);
    fixture.finish().await;
}

fn enable_relay_control(fixture: &Fixture) -> Arc<ScriptedEngramControlTransport> {
    fixture.central.select_test_scripted_engram_budget_clock();
    let root = fixture.central.test_temp_root.as_ref().unwrap().path().join("relay-controlled-project");
    fs::create_dir_all(&root).unwrap();
    let project = create_test_project(&fixture.central, &root, "Controlled relay");
    {
        let mut inner = fixture.central.inner.lock().unwrap();
        let record = &mut inner.sessions[0];
        record.session.project_id = Some(project.clone());
        record.session.workdir = root.to_string_lossy().into_owned();
        record.engram.context_nudge_pending = false;
    }
    crate::tests::engram_host_adapter::enable_test_project_engram(&fixture.central, &project, &root);
    let transport = ScriptedEngramControlTransport::new([
        ScriptedEngramControlResponse::Reply(Ok(json!({"routing_token":"relay-token","status":{"phase":"ready"}}))),
        ScriptedEngramControlResponse::Reply(Ok(json!({"decision":"grant","grant":{"grant_id":"relay-evaluated-grant"}}))),
    ]);
    fixture.central.install_control_test_transport(transport.clone());
    transport
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_r3_revoke_after_evaluate_prevents_queue_promotion() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    append_to_central(&fixture, "revoke-after-evaluation");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let _input = claude_input(&fixture);
    let transport = enable_relay_control(&fixture);
    let app = fixture.central.clone();
    let peer = fixture.channel.current_authority().unwrap();
    let reached = Arc::new(AtomicBool::new(false));
    let observed = reached.clone();
    TEST_MAILBOX_RELAY_AFTER_EVALUATE.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
        observed.store(true, Ordering::SeqCst);
        app.transition_federation_peer(&peer, FederationMembershipState::Revoked).unwrap();
    })));
    let result = fixture.central.dispatch_next_queued_turn(&fixture.native, false);
    assert!(reached.load(Ordering::SeqCst), "the authority change must follow an actual off-lock evaluation");
    assert!(transport.requests().iter().any(|request| request.request["operation"] == "turn_evaluate"));
    assert!(!matches!(result, Ok(Some(_))), "authority lost during evaluation must prevent promotion, not merely provider enqueue");
    let snapshot = {
        let inner = fixture.central.inner.lock().unwrap();
        (inner.sessions[0].session.status, inner.sessions[0].queued_prompts.len())
    };
    assert_ne!(snapshot.0, SessionStatus::Active);
    assert_eq!(snapshot.1, 1);
    assert!(fixture.central.inner.lock().unwrap().sessions[0].engram.rebind_required,
        "the issued evaluation must retain its existing rebind settlement obligation");
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_r3_cancel_after_redrive_evaluate_never_evaluates_successor() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let (append, receipt) = append_to_central(&fixture, "cancel-after-redrive-evaluate");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let input = claude_input(&fixture);
    fixture.central.inner.lock().unwrap().sessions[0].session.status = SessionStatus::Active;
    assert!(matches!(fixture.central.dispatch_turn(&fixture.native, r2::user_prompt("unevaluated successor"))
        .unwrap(), DispatchTurnResult::Queued));
    let carrier = {
        let mut inner = fixture.central.inner.lock().unwrap();
        inner.sessions[0].session.status = SessionStatus::Idle;
        inner.sessions[0].queued_prompts.front().unwrap().pending_prompt.id.clone()
    };
    let transport = enable_relay_control(&fixture);
    let app = fixture.central.clone();
    let session = fixture.native.clone();
    TEST_MAILBOX_RELAY_AFTER_EVALUATE.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
        app.cancel_queued_prompt(&session, &carrier).unwrap();
    })));
    let retained = fixture.central.mailbox_store.mailbox_relay_notification(
        &append.target, &receipt.mailbox_id, receipt.sequence
    ).unwrap().unwrap();
    fixture.central.queue_native_mailbox_relay_wake(&retained.wake, None).unwrap();
    assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)));
    let requests = transport.requests();
    assert_eq!(requests.iter().filter(|request| request.request["operation"] == "turn_evaluate").count(), 1,
        "an old redrive cannot evaluate a successor after its captured owner is canceled");
    let (status, queued) = {
        let inner = fixture.central.inner.lock().unwrap();
        (inner.sessions[0].session.status, inner.sessions[0].queued_prompts.len())
    };
    assert_ne!(status, SessionStatus::Active);
    assert_eq!(queued, 1);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_r3_fenced_carrier_bypass_keeps_ordinary_user_fifo() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    append_to_central(&fixture, "fenced-user-fifo");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let input = claude_input(&fixture);
    fixture.central.inner.lock().unwrap().sessions[0].session.status = SessionStatus::Active;
    assert!(matches!(fixture.central.dispatch_turn(&fixture.native, r2::user_prompt("first user"))
        .unwrap(), DispatchTurnResult::Queued));
    fixture.central.inner.lock().unwrap().sessions[0].session.status = SessionStatus::Idle;
    fixture.central.transition_federation_peer(&fixture.channel.current_authority().unwrap(),
        FederationMembershipState::Revoked).unwrap();
    let result = fixture.central.dispatch_turn(&fixture.native, r2::user_prompt("second user")).unwrap();
    let dispatch = match result {
        DispatchTurnResult::Dispatched(dispatch) | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("oldest ordinary user must run despite the fenced carrier"),
    };
    deliver_turn_dispatch(&fixture.central, dispatch).unwrap();
    match recv_within_guard(&input, "first user before newer explicit send") {
        Ok(ClaudeRuntimeCommand::Prompt(prompt)) => assert_eq!(prompt.text, "first user"),
        _ => panic!("expected first ordinary user"),
    }
    fixture.central.finish_turn_ok_if_runtime_matches(&fixture.native,
        &RuntimeToken::Claude("relay-review-authority".into())).unwrap();
    match recv_within_guard(&input, "second user before fenced carrier") {
        Ok(ClaudeRuntimeCommand::Prompt(prompt)) => assert_eq!(prompt.text, "second user"),
        _ => panic!("expected second ordinary user"),
    }
    assert_eq!(fixture.central.inner.lock().unwrap().sessions[0].queued_prompts.len(), 1);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_r3_fenced_carrier_cannot_bypass_retained_or_exclusive_preparation() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for hold in ["retained", "admitting", "source-preparing", "uncertain"] {
        let fixture = Fixture::new(false).await;
        let _handles = central_handle(&fixture);
        append_to_central(&fixture, "fenced-exclusive-preparation");
        fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
        let input = claude_input(&fixture);
        fixture.central.transition_federation_peer(&fixture.channel.current_authority().unwrap(),
            FederationMembershipState::Revoked).unwrap();
        {
            let mut inner = fixture.central.inner.lock().unwrap();
            let record = &mut inner.sessions[0];
            match hold {
                "retained" => record.queued_prompts.front_mut().unwrap().engram_interrupted = true,
                "admitting" => record.engram.admission_in_progress = Some(Arc::new(AtomicBool::new(false))),
                "source-preparing" => record.engram.source_observation_preparations = 1,
                _ => record.engram.uncertain_grant_id = Some("unresolved-begin".into()),
            }
        }
        assert!(matches!(fixture.central.dispatch_turn(&fixture.native, r2::user_prompt("protected ordinary work"))
            .unwrap(), DispatchTurnResult::Queued), "{hold} must settle before bypass");
        assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert_eq!(fixture.central.inner.lock().unwrap().sessions[0].queued_prompts.len(), 2);
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_r3_ack_is_not_rejection_or_permission_to_replay_accepted_handoff() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for accepted in [false, true] {
        let fixture = Fixture::new(false).await;
        let _handles = central_handle(&fixture);
        let (append, receipt) = append_to_central(&fixture, "ack-not-rejection");
        fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
        let input = claude_input(&fixture);
        let dispatch = fixture.central.dispatch_next_queued_turn(&fixture.native, false).unwrap().unwrap();
        if accepted {
            deliver_turn_dispatch(&fixture.central, dispatch).unwrap();
            assert!(matches!(recv_within_guard(&input, "accepted first relay handoff"),
                Ok(ClaudeRuntimeCommand::Prompt(_))));
        } else { drop(dispatch); } // no result/acceptance certificate
        acknowledge_at_home(&fixture, &append, &receipt);
        let notification = MailboxNotificationDelivery {
            session_id: fixture.native.clone(),
            mailbox_id: MailboxRelayLocator::encode(&append.conversation.home, &receipt.mailbox_id, false).unwrap(),
            through_sequence: receipt.sequence,
        };
        assert!(!fixture.central.requeue_rejected_mailbox_notification(&notification).unwrap(),
            "generic failure cannot infer a rejected first handoff from ACK");
        fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
        assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)));
        let retained = fixture.central.mailbox_store.mailbox_relay_notification(
            &append.target, &receipt.mailbox_id, receipt.sequence
        ).unwrap().unwrap();
        assert_eq!(retained.handed_off, accepted);
        assert_eq!(fixture.central.inner.lock().unwrap().sessions[0].queued_prompts.len(), 0);
        fixture.finish().await;
    }
}
