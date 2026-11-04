// Native relay queue-order regressions beside relay_review.rs. Uses its real
// home/target fixture and disposable stores; owns neither dispatch nor recovery.
use super::*;

fn append_from_new_sender(fixture: &Fixture, key: &str) -> MailboxAppendReceipt {
    let sender_id = test_session_id(&fixture.satellite, Agent::Codex);
    let sender = fixture
        .satellite
        .native_mailbox_relay_descriptor(&sender_id)
        .unwrap();
    let target = fixture
        .central
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap();
    let append = MailboxRelayAppend {
        requested_target: target.principal.participant_key(),
        conversation: MailboxRelayConversation::new(
            fixture.central.native_mailbox_relay_home().unwrap(),
            sender.principal.clone(),
            target.principal.clone(),
        )
        .unwrap(),
        sender: sender.principal,
        target: target.principal,
        sender_name: sender.name,
        target_name: target.name,
        message: format!("durable body {key}"),
        idempotency_key: key.into(),
        topic: None,
        state_stamp: None,
    };
    serde_json::from_value(
        fixture
            .central
            .execute_mailbox_relay(
                MailboxRelayOperation::Send {
                    append: append.clone(),
                },
                &fixture.channel.current_lease().unwrap(),
                &append.sender.host,
            )
            .unwrap(),
    )
    .unwrap()
}

fn queued_order(fixture: &Fixture) -> Vec<String> {
    let inner = fixture.central.inner.lock().unwrap();
    let root = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
    root.queued_prompts
        .iter()
        .map(|queued| {
            queued
                .pending_prompt
                .source
                .as_ref()
                .and_then(|source| source.mailbox.as_ref())
                .map(|mailbox| mailbox.mailbox_id.clone())
                .unwrap_or_else(|| queued.pending_prompt.text.clone())
        })
        .collect()
}

fn public_mailbox(fixture: &Fixture, receipt: &MailboxAppendReceipt) -> String {
    MailboxRelayLocator::encode(
        &fixture.central.native_mailbox_relay_home().unwrap(),
        &receipt.mailbox_id,
        false,
    )
    .unwrap()
}

fn queue_user(fixture: &Fixture) {
    let mut inner = fixture.central.inner.lock().unwrap();
    let id = inner.next_message_id();
    let root = inner.session_mut(&fixture.native).unwrap();
    queue_prompt_on_record(
        root,
        PendingPrompt {
            id,
            timestamp: stamp_now(),
            text: "user barrier".into(),
            expanded_text: None,
            source: None,
            attachments: Vec::new(),
            engram_interrupted: false,
            is_engram_retained: false,
        },
        Vec::new(),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_mailbox_prefix_refreshes_after_a_human_barrier_is_handed_off() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let a = append_from_new_sender(&fixture, "fenced-behind-human");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    queue_user(&fixture);
    {
        let mut inner = fixture.central.inner.lock().unwrap();
        let root = inner.session_mut(&fixture.native).unwrap();
        root.queued_prompts.swap(0, 1);
        sync_pending_prompts(root);
    }
    fixture.central.mailbox_store.connection().unwrap().execute(
        "DELETE FROM mailbox_relay_notifications", []
    ).unwrap();
    let input = claude_input(&fixture);
    let b = append_from_new_sender(&fixture, "valid-after-human");
    let raw: String = fixture.central.mailbox_store.connection().unwrap().query_row(
        "SELECT wake_json FROM mailbox_relay_wakes WHERE message_id=?1",
        [&b.message_id], |row| row.get(0)
    ).unwrap();
    let wake: MailboxRelayWake = serde_json::from_str(&raw).unwrap();
    let app = fixture.central.clone();
    let session = fixture.native.clone();
    TEST_MAILBOX_AFTER_PREFIX_OBSERVATION.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            // Actual human handoff removes U after B observed [U,A].
            let dispatch = app.dispatch_next_queued_turn(&session, false).unwrap().unwrap();
            deliver_turn_dispatch(&app, dispatch).unwrap();
            app.finish_turn_ok_if_runtime_matches(&session,
                &RuntimeToken::Claude("relay-review-authority".into())).unwrap();
        }));
    });
    assert!(fixture.central.queue_native_mailbox_relay_wake(&wake, None).unwrap());
    match input.try_recv() {
        Ok(ClaudeRuntimeCommand::Prompt(prompt)) => assert_eq!(prompt.text, "user barrier"),
        _ => panic!("the human barrier must be handed off first"),
    }
    match input.try_recv() {
        Ok(ClaudeRuntimeCommand::Prompt(prompt)) => assert!(prompt.text.contains(&public_mailbox(&fixture, &b))),
        _ => panic!("valid B must run after the newly exposed fenced A is observed"),
    }
    assert_eq!(queued_order(&fixture), vec![public_mailbox(&fixture, &a)]);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_mailbox_arrival_drains_a_dormant_local_head_in_fifo_order() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let sender = test_session_id(&fixture.central, Agent::Codex);
    let local = fixture.central.append_native_mailbox_message_and_notify(&sender,
        SendMailboxMessageRequest {
            target_session_id: fixture.native.clone(),
            message: "local durable body".into(), idempotency_key: "dormant-local".into(),
            topic: None, state_stamp: None, class: Some("routine".into()),
        }, NativeMailboxNamespace::Local).unwrap();
    assert_eq!(queued_order(&fixture), vec![local.mailbox_id.clone()]);
    let input = claude_input(&fixture);
    let b = append_from_new_sender(&fixture, "relay-after-local");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    match input.try_recv() {
        Ok(ClaudeRuntimeCommand::Prompt(prompt)) => assert!(prompt.text.contains(&local.mailbox_id)),
        _ => panic!("relay arrival must drive the dormant local head first"),
    }
    fixture.central.finish_turn_ok_if_runtime_matches(&fixture.native,
        &RuntimeToken::Claude("relay-review-authority".into())).unwrap();
    match recv_within_guard(&input, "relay following dormant local head") {
        Ok(ClaudeRuntimeCommand::Prompt(prompt)) => assert!(prompt.text.contains(&public_mailbox(&fixture, &b))),
        _ => panic!("relay B must follow local L without another arrival"),
    }
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_mailbox_dormant_local_head_keeps_session_dispatch_holds() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for hold in ["pause", "stop", "approval", "active", "capacity"] {
        let fixture = Fixture::new(false).await;
        let _handles = central_handle(&fixture);
        let sender = test_session_id(&fixture.central, Agent::Codex);
        let local = fixture.central.append_native_mailbox_message_and_notify(&sender,
            SendMailboxMessageRequest {
                target_session_id: fixture.native.clone(),
                message: "held local body".into(), idempotency_key: format!("local-{hold}"),
                topic: None, state_stamp: None, class: Some("routine".into()),
            }, NativeMailboxNamespace::Local).unwrap();
        let input = claude_input(&fixture);
        {
            let mut inner = fixture.central.inner.lock().unwrap();
            let root = inner.session_mut(&fixture.native).unwrap();
            match hold {
                "pause" => {
                    root.set_auto_dispatch_blocked(true);
                    root.engram.operator_paused = true;
                    assert!(root.session.queue_paused);
                }
                "stop" => root.orchestrator_auto_dispatch_blocked = true,
                "approval" => root.session.status = SessionStatus::Approval,
                "active" => root.session.status = SessionStatus::Active,
                "capacity" => {
                    root.session.provider_capacity_waiting = true;
                    root.orchestrator_auto_dispatch_blocked = true;
                }
                _ => unreachable!(),
            }
        }
        let b = append_from_new_sender(&fixture, &format!("relay-held-{hold}"));
        fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
        assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)), "{hold}");
        assert_eq!(queued_order(&fixture), vec![local.mailbox_id, public_mailbox(&fixture, &b)], "{hold}");
        fixture.finish().await;
    }
}

async fn assert_queued_redrive_drains_local_head(status: SessionStatus) {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let sender = test_session_id(&fixture.central, Agent::Codex);
    let local = fixture.central.append_native_mailbox_message_and_notify(&sender,
        SendMailboxMessageRequest {
            target_session_id: fixture.native.clone(),
            message: "retained local body".into(), idempotency_key: "retained-local".into(),
            topic: None, state_stamp: None, class: Some("routine".into()),
        }, NativeMailboxNamespace::Local).unwrap();
    let b = append_from_new_sender(&fixture, "retained-relay-after-local");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let target = fixture.central.native_mailbox_relay_descriptor(&fixture.native).unwrap().principal;
    let retained = fixture.central.mailbox_store.mailbox_relay_notification(
        &target, &b.mailbox_id, b.sequence,
    ).unwrap().unwrap();
    assert!(retained.queued && !retained.handed_off,
        "the retry must exercise an already admitted, unhanded relay wake");
    let expected = vec![local.mailbox_id.clone(), public_mailbox(&fixture, &b)];
    assert_eq!(queued_order(&fixture), expected);
    let input = claude_input(&fixture);
    {
        // Restore the dispatchable queue shape without admitting another wake.
        // Error is the status used for a turn interrupted by host restart.
        let mut inner = fixture.central.inner.lock().unwrap();
        let root = inner.session_mut(&fixture.native).unwrap();
        root.session.status = status;
        root.set_auto_dispatch_blocked(false);
    }
    assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)));
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    match input.try_recv() {
        Ok(ClaudeRuntimeCommand::Prompt(prompt)) => assert!(prompt.text.contains(&local.mailbox_id)),
        _ => panic!("queued relay redrive must hand off retained local L without a new arrival"),
    }
    fixture.central.finish_turn_ok_if_runtime_matches(&fixture.native,
        &RuntimeToken::Claude("relay-review-authority".into())).unwrap();
    match recv_within_guard(&input, "retained relay following local redrive") {
        Ok(ClaudeRuntimeCommand::Prompt(prompt)) => assert!(prompt.text.contains(&public_mailbox(&fixture, &b))),
        _ => panic!("retained relay B must follow L without another arrival"),
    }
    assert!(queued_order(&fixture).is_empty());
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_mailbox_queued_redrive_drains_local_head_on_idle_root() {
    assert_queued_redrive_drains_local_head(SessionStatus::Idle).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_mailbox_queued_redrive_drains_local_head_on_recovered_error_root() {
    assert_queued_redrive_drains_local_head(SessionStatus::Error).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_mailbox_queued_local_head_redrive_keeps_cancel_pause_and_stop_fences() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for fence in ["cancel-before", "cancel-after", "pause", "stop"] {
        let fixture = Fixture::new(false).await;
        let _handles = central_handle(&fixture);
        let sender = test_session_id(&fixture.central, Agent::Codex);
        let local = fixture.central.append_native_mailbox_message_and_notify(&sender,
            SendMailboxMessageRequest {
                target_session_id: fixture.native.clone(),
                message: "local head kept owed".into(), idempotency_key: format!("local-{fence}"),
                topic: None, state_stamp: None, class: Some("routine".into()),
            }, NativeMailboxNamespace::Local).unwrap();
        let b = append_from_new_sender(&fixture, &format!("queued-relay-{fence}"));
        fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
        let target = fixture.central.native_mailbox_relay_descriptor(&fixture.native).unwrap().principal;
        let retained = fixture.central.mailbox_store.mailbox_relay_notification(
            &target, &b.mailbox_id, b.sequence,
        ).unwrap().unwrap();
        assert!(retained.queued && !retained.handed_off);
        let input = claude_input(&fixture);
        let carrier = {
            let mut inner = fixture.central.inner.lock().unwrap();
            let root = inner.session_mut(&fixture.native).unwrap();
            root.set_auto_dispatch_blocked(false);
            if fence == "pause" || fence == "stop" {
                root.set_auto_dispatch_blocked(true);
                root.engram.operator_paused = fence == "pause";
            }
            root.queued_prompts.back().unwrap().pending_prompt.id.clone()
        };
        if fence == "cancel-before" {
            fixture.central.cancel_queued_prompt(&fixture.native, &carrier).unwrap();
        } else if fence == "cancel-after" {
            let app = fixture.central.clone();
            let session = fixture.native.clone();
            TEST_MAILBOX_RELAY_BEFORE_REDRIVE.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    app.cancel_queued_prompt(&session, &carrier).unwrap();
                }));
            });
        }
        fixture.central.queue_native_mailbox_relay_wake(&retained.wake, None).unwrap();
        assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)), "{fence}");
        let expected = if fence.starts_with("cancel") {
            vec![local.mailbox_id]
        } else {
            vec![local.mailbox_id, public_mailbox(&fixture, &b)]
        };
        assert_eq!(queued_order(&fixture), expected, "{fence}");
        if fence == "cancel-after" {
            assert!(TEST_MAILBOX_RELAY_BEFORE_REDRIVE.with(|hook| hook.borrow().is_none()),
                "the cancellation must run after owner capture");
        }
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_mailbox_valid_arrival_runs_ahead_of_an_unrelated_fenced_carrier() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let a = append_from_new_sender(&fixture, "fenced-older-carrier");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let a_mailbox = public_mailbox(&fixture, &a);
    assert_eq!(queued_order(&fixture), vec![a_mailbox.clone()]);
    // Remove only A's authority witness; B will retain its own valid witness.
    fixture.central.mailbox_store.connection().unwrap().execute(
        "DELETE FROM mailbox_relay_notifications", []
    ).unwrap();
    let input = claude_input(&fixture);
    let b = append_from_new_sender(&fixture, "valid-unrelated-arrival");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    match input.try_recv() {
        Ok(ClaudeRuntimeCommand::Prompt(prompt)) => {
            assert!(prompt.text.contains(&public_mailbox(&fixture, &b)));
            assert!(!prompt.text.contains(&a_mailbox));
        }
        _ => panic!("valid B must be handed off despite unrelated fenced A"),
    }
    assert_eq!(queued_order(&fixture), vec![a_mailbox],
        "fenced A remains owed and must never be handed off or canceled");
    assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)));
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_mailbox_arrivals_preserve_the_leading_fifo_before_a_user_barrier() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let a = append_from_new_sender(&fixture, "oldest");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    queue_user(&fixture);
    let b = append_from_new_sender(&fixture, "later");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    assert_eq!(
        queued_order(&fixture),
        vec![
            public_mailbox(&fixture, &a),
            public_mailbox(&fixture, &b),
            "user barrier".into()
        ],
        "a later native wake must wait behind the older leading mailbox wake"
    );
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_mailbox_fifo_does_not_reorder_a_wake_already_behind_a_user_barrier() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let a = append_from_new_sender(&fixture, "behind-user");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    queue_user(&fixture);
    {
        // A restored queue may already have a user prompt ahead of a wake.
        let mut inner = fixture.central.inner.lock().unwrap();
        let root = inner.session_mut(&fixture.native).unwrap();
        root.queued_prompts.swap(0, 1);
        sync_pending_prompts(root);
    }
    let b = append_from_new_sender(&fixture, "new-before-user");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    assert_eq!(
        queued_order(&fixture),
        vec![
            public_mailbox(&fixture, &b),
            "user barrier".into(),
            public_mailbox(&fixture, &a)
        ],
        "FIFO is bounded by the existing non-mailbox barrier"
    );
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_mailbox_ordinary_coalescing_does_not_overtake_an_older_mailbox() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let a = append_from_new_sender(&fixture, "older-queued");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let (mut append, b) = append_to_central(&fixture, "ordinary-queued");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let expected = vec![public_mailbox(&fixture, &a), public_mailbox(&fixture, &b)];
    {
        // Isolate ordinary coalescing from new-arrival ordering.
        let mut inner = fixture.central.inner.lock().unwrap();
        let root = inner.session_mut(&fixture.native).unwrap();
        if root.queued_prompts[0]
            .pending_prompt
            .source
            .as_ref()
            .unwrap()
            .mailbox
            .as_ref()
            .unwrap()
            .mailbox_id
            != expected[0]
        {
            root.queued_prompts.swap(0, 1);
        }
        sync_pending_prompts(root);
    }
    append.idempotency_key = "ordinary-coalesced".into();
    append.message = "newer boundary in the same mailbox".into();
    fixture
        .central
        .execute_mailbox_relay(
            MailboxRelayOperation::Send {
                append: append.clone(),
            },
            &fixture.channel.current_lease().unwrap(),
            &append.sender.host,
        )
        .unwrap();
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    assert_eq!(
        queued_order(&fixture),
        expected,
        "ordinary coalescing retains its original position"
    );
    {
        let inner = fixture.central.inner.lock().unwrap();
        let root = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
        assert_eq!(
            root.queued_prompts[1]
                .pending_prompt
                .source
                .as_ref()
                .unwrap()
                .mailbox
                .as_ref()
                .unwrap()
                .sequence,
            b.sequence + 1
        );
    }
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_mailbox_fifo_survives_new_arrivals_between_provider_turns() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let a = append_from_new_sender(&fixture, "first-opportunity");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let b = append_from_new_sender(&fixture, "second-opportunity");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let input = claude_input(&fixture);
    for (step, expected) in [&a, &b].into_iter().enumerate() {
        {
            let mut inner = fixture.central.inner.lock().unwrap();
            inner
                .session_mut(&fixture.native)
                .unwrap()
                .orchestrator_auto_dispatch_blocked = false;
        }
        let dispatch = fixture
            .central
            .dispatch_next_queued_turn(&fixture.native, false)
            .unwrap()
            .expect("a runnable mailbox remains");
        deliver_turn_dispatch(&fixture.central, dispatch).unwrap();
        match recv_within_guard(&input, "FIFO relay provider handoff") {
            Ok(ClaudeRuntimeCommand::Prompt(prompt)) => assert!(
                prompt.text.contains(&public_mailbox(&fixture, expected)),
                "oldest mailbox must receive dispatch opportunity {step}: {}",
                prompt.text
            ),
            _ => panic!("expected the exact relay prompt"),
        }
        {
            let mut inner = fixture.central.inner.lock().unwrap();
            inner
                .session_mut(&fixture.native)
                .unwrap()
                .orchestrator_auto_dispatch_blocked = true;
        }
        fixture
            .central
            .finish_turn_ok_if_runtime_matches(
                &fixture.native,
                &RuntimeToken::Claude("relay-review-authority".into()),
            )
            .unwrap();
        assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)));
        // Each completed turn gives a different sender a chance to arrive.
        append_from_new_sender(&fixture, &format!("new-after-{step}"));
        fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    }
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_mailbox_fifo_preserves_active_approval_stop_and_pause_holds() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for hold in [
        "active", "approval", "stopping", "stopped", "capacity", "pause",
    ] {
        let fixture = Fixture::new(false).await;
        let _handles = central_handle(&fixture);
        let input = claude_input(&fixture);
        {
            let mut inner = fixture.central.inner.lock().unwrap();
            let root = inner.session_mut(&fixture.native).unwrap();
            match hold {
                "active" => root.session.status = SessionStatus::Active,
                "approval" => root.session.status = SessionStatus::Approval,
                "stopping" => root.runtime_stop_in_progress = true,
                "stopped" => root.orchestrator_auto_dispatch_blocked = true,
                "capacity" => {
                    root.session.provider_capacity_waiting = true;
                    root.orchestrator_auto_dispatch_blocked = true;
                }
                "pause" => {
                    root.engram.operator_paused = true;
                    root.orchestrator_auto_dispatch_blocked = true;
                }
                _ => unreachable!(),
            }
        }
        let a = append_from_new_sender(&fixture, &format!("held-old-{hold}"));
        fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
        let b = append_from_new_sender(&fixture, &format!("held-new-{hold}"));
        fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
        assert!(
            fixture
                .central
                .dispatch_next_queued_turn(&fixture.native, false)
                .unwrap()
                .is_none(),
            "{hold}"
        );
        assert!(
            matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "{hold}"
        );
        assert_eq!(
            queued_order(&fixture),
            vec![public_mailbox(&fixture, &a), public_mailbox(&fixture, &b)],
            "{hold}"
        );
        fixture.finish().await;
    }
}
