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
        let dispatch = after_promotion.then(|| {
            fixture
                .central
                .dispatch_next_queued_turn(&fixture.native, false)
                .unwrap()
                .unwrap()
        });
        drop(fixture.attested.lock().unwrap().take());
        let peer = fixture
            .central
            .federation_peer_snapshot(&fixture.channel.snapshot.peer.host_id)
            .unwrap();
        assert_eq!(peer.membership, FederationMembershipState::Active);
        assert!(fixture.channel.current_lease().is_err());
        let dispatch = dispatch.unwrap_or_else(|| {
            fixture
                .central
                .dispatch_next_queued_turn(&fixture.native, false)
                .unwrap()
                .unwrap()
        });
        deliver_turn_dispatch(&fixture.central, dispatch).unwrap();
        assert!(matches!(
            recv_within_guard(&input, "committed home-local offline wake"),
            Ok(ClaudeRuntimeCommand::Prompt(_))
        ));
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
            "revoked" => fixture
                .central
                .transition_federation_peer(
                    &fixture.channel.current_authority().unwrap(),
                    FederationMembershipState::Revoked,
                )
                .unwrap(),
            "disabled" => r2::set_federation_enabled(&fixture.central, false),
            _ => {
                fixture
                    .central
                    .mailbox_store
                    .connection()
                    .unwrap()
                    .execute("DELETE FROM mailbox_relay_notifications", [])
                    .unwrap();
            }
        }
        let result = fixture
            .central
            .dispatch_turn(&fixture.native, r2::user_prompt("ordinary local work"))
            .expect("ordinary work cannot be refused by an unrelated relay");
        let dispatch = match result {
            DispatchTurnResult::Dispatched(dispatch)
            | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
            DispatchTurnResult::Queued => {
                panic!("{refusal}: an idle unpaused root must run ordinary work")
            }
        };
        deliver_turn_dispatch(&fixture.central, dispatch).unwrap();
        match recv_within_guard(&input, "ordinary prompt ahead of held relay") {
            Ok(ClaudeRuntimeCommand::Prompt(prompt)) => {
                assert_eq!(prompt.text, "ordinary local work")
            }
            _ => panic!("expected ordinary prompt"),
        }
        assert_eq!(
            fixture.central.inner.lock().unwrap().sessions[0]
                .queued_prompts
                .len(),
            1,
            "the fenced metadata carrier remains owed, not silently canceled"
        );
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
    let dispatch = fixture
        .central
        .dispatch_next_queued_turn(&fixture.native, false)
        .unwrap()
        .unwrap();
    assert!(!matches!(
        deliver_turn_dispatch(&fixture.central, dispatch),
        TurnDispatchDeliveryOutcome::Delivered
    ));
    let retained = fixture
        .central
        .mailbox_store
        .mailbox_relay_notification(&append.target, &receipt.mailbox_id, receipt.sequence)
        .unwrap()
        .unwrap();
    assert!(retained.acknowledged && !retained.handed_off);
    let queued = fixture.central.inner.lock().unwrap().sessions[0]
        .queued_prompts
        .len();
    assert_eq!(
        queued, 1,
        "a rejected first enqueue cannot cancel an acknowledged continuation"
    );
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
    fixture.central.inner.lock().unwrap().sessions[0]
        .session
        .status = SessionStatus::Active;
    assert!(matches!(
        fixture
            .central
            .dispatch_turn(&fixture.native, r2::user_prompt("unrelated successor"))
            .unwrap(),
        DispatchTurnResult::Queued
    ));
    let carrier = {
        let mut inner = fixture.central.inner.lock().unwrap();
        inner.sessions[0].session.status = SessionStatus::Idle;
        inner.sessions[0]
            .queued_prompts
            .front()
            .unwrap()
            .pending_prompt
            .id
            .clone()
    };
    let app = fixture.central.clone();
    let session = fixture.native.clone();
    TEST_MAILBOX_RELAY_BEFORE_REDRIVE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            app.cancel_queued_prompt(&session, &carrier).unwrap();
        }))
    });
    let retained = fixture
        .central
        .mailbox_store
        .mailbox_relay_notification(&append.target, &receipt.mailbox_id, receipt.sequence)
        .unwrap()
        .unwrap();
    fixture
        .central
        .queue_native_mailbox_relay_wake(&retained.wake, None)
        .unwrap();
    assert!(
        matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "a stale relay redrive must never deliver the successor exposed by Cancel"
    );
    assert_eq!(
        fixture.central.inner.lock().unwrap().sessions[0]
            .queued_prompts
            .len(),
        1
    );
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_r3_revoke_after_authority_capture_prevents_queue_promotion() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let (append, receipt) = append_to_central(&fixture, "revoke-after-authority-capture");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let input = claude_input(&fixture);
    // Capture through the actual ordinary authority owner, then change the
    // peer before its locked revalidation/promotion. No evaluation is run.
    let captured = fixture
        .central
        .prepare_front_mailbox_relay_authority(&fixture.native)
        .unwrap()
        .expect("the exact relay head has current authority");
    fixture
        .central
        .transition_federation_peer(
            &fixture.channel.current_authority().unwrap(),
            FederationMembershipState::Revoked,
        )
        .unwrap();
    {
        let inner = fixture.central.inner.lock().unwrap();
        assert!(
            fixture
                .central
                .validate_front_mailbox_relay_authority_locked(
                    &inner,
                    &fixture.native,
                    Some(&captured),
                )
                .is_err(),
            "captured authority cannot survive revocation"
        );
    }
    let result = fixture
        .central
        .dispatch_next_queued_turn(&fixture.native, false);
    assert!(
        !matches!(result, Ok(Some(_))),
        "lost ordinary authority must prevent promotion"
    );
    assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)));
    let inner = fixture.central.inner.lock().unwrap();
    assert_ne!(inner.sessions[0].session.status, SessionStatus::Active);
    assert_eq!(inner.sessions[0].queued_prompts.len(), 1);
    drop(inner);
    let retained = fixture
        .central
        .mailbox_store
        .mailbox_relay_notification(&append.target, &receipt.mailbox_id, receipt.sequence)
        .unwrap()
        .unwrap();
    assert!(
        !retained.handed_off,
        "revocation leaves the exact relay obligation owed"
    );
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_r3_cancel_after_redrive_capture_never_promotes_successor() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let (append, receipt) = append_to_central(&fixture, "cancel-after-redrive-capture");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let input = claude_input(&fixture);
    fixture.central.inner.lock().unwrap().sessions[0]
        .session
        .status = SessionStatus::Active;
    assert!(matches!(
        fixture
            .central
            .dispatch_turn(&fixture.native, r2::user_prompt("unrelated successor"))
            .unwrap(),
        DispatchTurnResult::Queued
    ));
    let carrier = {
        let mut inner = fixture.central.inner.lock().unwrap();
        inner.sessions[0].session.status = SessionStatus::Idle;
        inner.sessions[0]
            .queued_prompts
            .front()
            .unwrap()
            .pending_prompt
            .id
            .clone()
    };
    let app = fixture.central.clone();
    let session = fixture.native.clone();
    let reached = Arc::new(AtomicBool::new(false));
    let observed = reached.clone();
    // The production redrive has captured its MailboxRelayDrainOwner before
    // this existing seam. Cancel must invalidate that owner, not lend it to B.
    TEST_MAILBOX_RELAY_BEFORE_REDRIVE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            observed.store(true, Ordering::SeqCst);
            app.cancel_queued_prompt(&session, &carrier).unwrap();
        }))
    });
    let retained = fixture
        .central
        .mailbox_store
        .mailbox_relay_notification(&append.target, &receipt.mailbox_id, receipt.sequence)
        .unwrap()
        .unwrap();
    fixture
        .central
        .queue_native_mailbox_relay_wake(&retained.wake, None)
        .unwrap();
    assert!(
        reached.load(Ordering::SeqCst),
        "the actual redrive owner cut must execute"
    );
    assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)));
    let inner = fixture.central.inner.lock().unwrap();
    assert_ne!(inner.sessions[0].session.status, SessionStatus::Active);
    assert_eq!(inner.sessions[0].queued_prompts.len(), 1);
    assert_eq!(
        inner.sessions[0].queued_prompts[0].pending_prompt.text,
        "unrelated successor"
    );
    drop(inner);
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
    fixture.central.inner.lock().unwrap().sessions[0]
        .session
        .status = SessionStatus::Active;
    assert!(matches!(
        fixture
            .central
            .dispatch_turn(&fixture.native, r2::user_prompt("first user"))
            .unwrap(),
        DispatchTurnResult::Queued
    ));
    fixture.central.inner.lock().unwrap().sessions[0]
        .session
        .status = SessionStatus::Idle;
    fixture
        .central
        .transition_federation_peer(
            &fixture.channel.current_authority().unwrap(),
            FederationMembershipState::Revoked,
        )
        .unwrap();
    let result = fixture
        .central
        .dispatch_turn(&fixture.native, r2::user_prompt("second user"))
        .unwrap();
    let dispatch = match result {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => {
            panic!("oldest ordinary user must run despite the fenced carrier")
        }
    };
    deliver_turn_dispatch(&fixture.central, dispatch).unwrap();
    match recv_within_guard(&input, "first user before newer explicit send") {
        Ok(ClaudeRuntimeCommand::Prompt(prompt)) => assert_eq!(prompt.text, "first user"),
        _ => panic!("expected first ordinary user"),
    }
    fixture
        .central
        .finish_turn_ok_if_runtime_matches(
            &fixture.native,
            &RuntimeToken::Claude("relay-review-authority".into()),
        )
        .unwrap();
    match recv_within_guard(&input, "second user before fenced carrier") {
        Ok(ClaudeRuntimeCommand::Prompt(prompt)) => assert_eq!(prompt.text, "second user"),
        _ => panic!("expected second ordinary user"),
    }
    assert_eq!(
        fixture.central.inner.lock().unwrap().sessions[0]
            .queued_prompts
            .len(),
        1
    );
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_r3_fenced_carrier_cannot_bypass_retained_or_exclusive_preparation() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for hold in ["stop", "delete", "reset"] {
        let fixture = Fixture::new(false).await;
        let _handles = central_handle(&fixture);
        append_to_central(&fixture, "fenced-exclusive-preparation");
        fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
        let input = claude_input(&fixture);
        fixture
            .central
            .transition_federation_peer(
                &fixture.channel.current_authority().unwrap(),
                FederationMembershipState::Revoked,
            )
            .unwrap();
        let project = (hold == "reset").then(|| {
            create_test_project(
                &fixture.central,
                fixture.central.test_temp_root.as_ref().unwrap().path(),
                "Relay resource fence",
            )
        });
        {
            let mut inner = fixture.central.inner.lock().unwrap();
            let record = &mut inner.sessions[0];
            match hold {
                "stop" => record.runtime_stop_in_progress = true,
                "delete" => record.engram.observation_delete_requested = true,
                "reset" => {
                    let project = project.as_ref().unwrap();
                    record.session.project_id = Some(project.clone());
                    assert!(inner.claim_engram_project_reset(project).is_some());
                }
                _ => unreachable!(),
            }
        }
        assert!(
            matches!(
                fixture
                    .central
                    .dispatch_turn(&fixture.native, r2::user_prompt("protected ordinary work"))
                    .unwrap(),
                DispatchTurnResult::Queued
            ),
            "{hold} must settle before bypass"
        );
        assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert_eq!(
            fixture.central.inner.lock().unwrap().sessions[0]
                .queued_prompts
                .len(),
            2
        );
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_r3_unknown_carrier_stays_immutable_while_fresh_user_work_runs() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let (append, receipt) = append_to_central(&fixture, "fenced-unknown-carrier");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let input = claude_input(&fixture);
    fixture
        .central
        .transition_federation_peer(
            &fixture.channel.current_authority().unwrap(),
            FederationMembershipState::Revoked,
        )
        .unwrap();
    let original = {
        let mut inner = fixture.central.inner.lock().unwrap();
        let record = &mut inner.sessions[0];
        assert_eq!(record.queued_prompts.len(), 1);
        let head = record.queued_prompts.front_mut().unwrap();
        head.promotion_disposition_known = false;
        head.engram_ordinary_handoff = true;
        serde_json::to_value(head).unwrap()
    };
    let before = fixture
        .central
        .mailbox_store
        .mailbox_relay_notification(&append.target, &receipt.mailbox_id, receipt.sequence)
        .unwrap()
        .unwrap();
    assert!(!before.handed_off);
    assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)));
    let result = fixture
        .central
        .dispatch_turn(
            &fixture.native,
            r2::user_prompt("fresh user beside an unknown carrier"),
        )
        .unwrap();
    let dispatch = match result {
        DispatchTurnResult::Dispatched(dispatch)
        | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => {
            panic!("unknown delivery history cannot hold fresh ordinary work")
        }
    };
    deliver_turn_dispatch(&fixture.central, dispatch).unwrap();
    match recv_within_guard(&input, "only fresh ordinary input reaches the provider") {
        Ok(ClaudeRuntimeCommand::Prompt(prompt)) => {
            assert_eq!(prompt.text, "fresh user beside an unknown carrier")
        }
        _ => panic!("expected the fresh user prompt"),
    }
    assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)));
    fixture
        .central
        .finish_turn_ok_if_runtime_matches(
            &fixture.native,
            &RuntimeToken::Claude("relay-review-authority".into()),
        )
        .unwrap();
    assert!(
        matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the unknown original is never replayed after the fresh turn"
    );
    {
        let inner = fixture.central.inner.lock().unwrap();
        assert_eq!(inner.sessions[0].queued_prompts.len(), 1);
        assert_eq!(
            serde_json::to_value(&inner.sessions[0].queued_prompts[0]).unwrap(),
            original
        );
    }
    let after = fixture
        .central
        .mailbox_store
        .mailbox_relay_notification(&append.target, &receipt.mailbox_id, receipt.sequence)
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            after.wake,
            after.peer,
            after.acknowledged,
            after.handed_off,
            after.queued
        ),
        (
            before.wake,
            before.peer,
            before.acknowledged,
            before.handed_off,
            before.queued
        ),
        "fresh work leaves the exact relay obligation unchanged"
    );
    fixture.finish().await;
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
        let dispatch = fixture
            .central
            .dispatch_next_queued_turn(&fixture.native, false)
            .unwrap()
            .unwrap();
        if accepted {
            deliver_turn_dispatch(&fixture.central, dispatch).unwrap();
            assert!(matches!(
                recv_within_guard(&input, "accepted first relay handoff"),
                Ok(ClaudeRuntimeCommand::Prompt(_))
            ));
        } else {
            drop(dispatch);
        } // no result/acceptance certificate
        acknowledge_at_home(&fixture, &append, &receipt);
        let notification = MailboxNotificationDelivery {
            session_id: fixture.native.clone(),
            mailbox_id: MailboxRelayLocator::encode(
                &append.conversation.home,
                &receipt.mailbox_id,
                false,
            )
            .unwrap(),
            through_sequence: receipt.sequence,
        };
        assert!(
            !fixture
                .central
                .requeue_rejected_mailbox_notification(&notification)
                .unwrap(),
            "generic failure cannot infer a rejected first handoff from ACK"
        );
        fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
        assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)));
        let retained = fixture
            .central
            .mailbox_store
            .mailbox_relay_notification(&append.target, &receipt.mailbox_id, receipt.sequence)
            .unwrap()
            .unwrap();
        assert_eq!(retained.handed_off, accepted);
        assert_eq!(
            fixture.central.inner.lock().unwrap().sessions[0]
                .queued_prompts
                .len(),
            0
        );
        fixture.finish().await;
    }
}
