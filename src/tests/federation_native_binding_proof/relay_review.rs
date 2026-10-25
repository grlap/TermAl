// Review counterexamples exercised through the real relay home and native queue.
// Disposable SQLite and restored state model boot; no live host is restarted.
use super::*;

#[path = "relay_review_r2.rs"]
mod r2;

#[path = "relay_review_r3.rs"]
mod r3;

#[path = "relay_review_r4.rs"]
mod r4;

fn claude_input(fixture: &Fixture) -> mpsc::Receiver<ClaudeRuntimeCommand> {
    claude_input_for(&fixture.central, &fixture.native)
}

fn claude_input_for(app: &AppState, session: &str) -> mpsc::Receiver<ClaudeRuntimeCommand> {
    let (runtime, input) = test_claude_runtime_handle("relay-review-authority");
    let mut inner = app.inner.lock().unwrap();
    let index = inner.find_session_index(session).unwrap();
    let root = &mut inner.sessions[index];
    root.session.agent = Agent::Claude;
    root.session.claude_approval_mode = Some(default_claude_approval_mode());
    root.session.claude_effort = Some(default_claude_effort());
    root.session.approval_policy = None;
    root.session.reasoning_effort = None;
    root.session.sandbox_mode = None;
    root.session.codex_thread_state = None;
    root.session.status = SessionStatus::Idle;
    root.runtime = SessionRuntime::Claude(runtime);
    root.orchestrator_auto_dispatch_blocked = false;
    input
}

fn central_handle(fixture: &Fixture) -> RelayHandleGuard {
    let peer = fixture.channel.current_authority().unwrap();
    fixture
        .central
        .federation_peer_handles
        .lock()
        .unwrap()
        .insert(
            peer.peer.host_id.clone(),
            Arc::new(FederationPeerHandle {
                channel: fixture.channel.clone(),
                rpc: fixture.rpc.clone(),
            }),
        );
    let mut inner = fixture.central.inner.lock().unwrap();
    let index = inner.find_session_index(&fixture.native).unwrap();
    inner.sessions[index].orchestrator_auto_dispatch_blocked = true;
    RelayHandleGuard(vec![fixture.central.clone()])
}

fn append_to_central(fixture: &Fixture, key: &str) -> (MailboxRelayAppend, MailboxAppendReceipt) {
    append_to_central_target(fixture, key, &fixture.native)
}

fn append_to_central_target(
    fixture: &Fixture,
    key: &str,
    target_id: &str,
) -> (MailboxRelayAppend, MailboxAppendReceipt) {
    let sender = fixture
        .satellite
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap();
    let target = fixture
        .central
        .native_mailbox_relay_descriptor(target_id)
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
        message: "real home body, never a provider prompt".into(),
        idempotency_key: key.into(),
        topic: None,
        state_stamp: None,
    };
    let receipt = serde_json::from_value(
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
    .unwrap();
    (append, receipt)
}

fn acknowledge_at_home(
    fixture: &Fixture,
    append: &MailboxRelayAppend,
    receipt: &MailboxAppendReceipt,
) {
    let lease = fixture.channel.current_lease().unwrap();
    let page: MailboxReadResponse = serde_json::from_value(
        fixture
            .central
            .execute_mailbox_relay(
                MailboxRelayOperation::Read {
                    actor: append.target.clone(),
                    conversation: append.conversation.clone(),
                    mailbox_id: receipt.mailbox_id.clone(),
                    after_sequence: None,
                    limit: 20,
                    issue_receipt: true,
                },
                &lease,
                &append.target.host,
            )
            .unwrap(),
    )
    .unwrap();
    fixture
        .central
        .execute_mailbox_relay(
            MailboxRelayOperation::Acknowledge {
                actor: append.target.clone(),
                conversation: append.conversation.clone(),
                mailbox_id: receipt.mailbox_id.clone(),
                receipt: page.receipt.unwrap(),
            },
            &lease,
            &append.target.host,
        )
        .unwrap();
    fixture
        .central
        .refresh_native_mailbox_relay_acknowledgement(
            &fixture.native,
            &MailboxRelayLocator {
                version: MAILBOX_RELAY_VERSION,
                home: append.conversation.home.clone(),
                id: receipt.mailbox_id.clone(),
            },
            receipt.sequence,
        )
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_ack_before_first_home_batch_keeps_the_owed_continuation() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let (append, receipt) = append_to_central(&fixture, "ack-before-first-batch");
    acknowledge_at_home(&fixture, &append, &receipt);
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let queued_texts = {
        let inner = fixture.central.inner.lock().unwrap();
        let root = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
        root.queued_prompts
            .iter()
            .map(|queued| queued.pending_prompt.text.clone())
            .collect::<Vec<_>>()
    };
    let text = queued_texts
        .first()
        .expect("ACK is not cancellation of an owed wake");
    assert!(text.contains("already acknowledged"), "{text}");
    assert!(!text.contains("has 1 unread"));
    assert!(!text.contains(&append.message));
    assert_eq!(queued_texts.len(), 1);
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    assert_eq!(
        fixture.central.inner.lock().unwrap().sessions[0]
            .queued_prompts
            .len(),
        1
    );
    fixture.finish().await;
}

async fn receiver_boot_after_handoff(acknowledged: bool) {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let (append, receipt) = append_to_central(&fixture, "boot-after-handoff");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let (runtime, input) = test_claude_runtime_handle("relay-review-boot");
    {
        let mut inner = fixture.central.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        let root = &mut inner.sessions[index];
        root.orchestrator_auto_dispatch_blocked = false;
        root.session.agent = Agent::Claude;
        root.session.claude_approval_mode = Some(default_claude_approval_mode());
        root.session.claude_effort = Some(default_claude_effort());
        root.session.approval_policy = None;
        root.session.reasoning_effort = None;
        root.session.sandbox_mode = None;
        root.session.codex_thread_state = None;
        root.session.status = SessionStatus::Idle;
        root.runtime = SessionRuntime::Claude(runtime);
    }
    let dispatch = fixture
        .central
        .dispatch_next_queued_turn(&fixture.native, false)
        .unwrap()
        .unwrap();
    deliver_turn_dispatch(&fixture.central, dispatch).unwrap();
    assert!(matches!(
        recv_within_guard(&input, "accepted relay handoff"),
        Ok(ClaudeRuntimeCommand::Prompt(_))
    ));
    assert!(
        fixture
            .central
            .mailbox_store
            .mailbox_relay_notification(&append.target, &receipt.mailbox_id, receipt.sequence)
            .unwrap()
            .unwrap()
            .handed_off
    );
    if acknowledged {
        acknowledge_at_home(&fixture, &append, &receipt);
    }
    let persisted = {
        let inner = fixture.central.inner.lock().unwrap();
        PersistedState::from_inner(&inner)
    };
    let restored = persisted.into_inner().unwrap();
    let booted = AppState {
        inner: Arc::new(StateMutex::new(restored)),
        mailbox_store: Arc::new(MailboxStore::open(&fixture.central.mailbox_store.path).unwrap()),
        ..fixture.central.clone()
    };
    {
        let mut inner = booted.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.native).unwrap();
        let root = &mut inner.sessions[index];
        assert!(root.active_turn_mailbox_notification.is_none());
        assert!(root.queued_prompts.is_empty());
        root.orchestrator_auto_dispatch_blocked = true;
    }
    let mut worker = MailboxRelayWakeWorker::default();
    worker.tick(&booted, true).await;
    worker.shutdown().await;
    let queued_texts = {
        let inner = booted.inner.lock().unwrap();
        let root = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
        root.queued_prompts
            .iter()
            .map(|queued| queued.pending_prompt.text.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        queued_texts.len(),
        usize::from(!acknowledged),
        "boot recovers only the owed unread boundary, not a second runtime admission for acknowledged mail"
    );
    assert!(
        queued_texts
            .iter()
            .all(|text| !text.contains(&append.message))
    );
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_receiver_boot_recovers_unread_after_runtime_handoff() {
    receiver_boot_after_handoff(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_receiver_boot_does_not_rewake_an_acknowledged_handoff() {
    receiver_boot_after_handoff(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_boot_pages_do_not_let_a_retired_target_block_healthy_boundaries() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let retired = test_session_id(&fixture.central, Agent::Codex);
    {
        let mut inner = fixture.central.inner.lock().unwrap();
        let index = inner.find_session_index(&retired).unwrap();
        inner.sessions[index].orchestrator_auto_dispatch_blocked = true;
    }
    append_to_central_target(&fixture, "retired-boot-target", &retired);
    for index in 0..16 {
        append_to_central(&fixture, &format!("healthy-boot-{index}"));
    }
    let mut cursors = (0, 0);
    loop {
        cursors = fixture
            .central
            .run_mailbox_relay_wake_batch(cursors.0, cursors.1)
            .await;
        if cursors == (0, 0) {
            break;
        }
    }
    // Model the lost process-owned queue after a prior handoff. The companion
    // restart control above proves this state using an actual runtime handoff
    // and PersistedState::into_inner; here the extra rows exercise pagination.
    fixture
        .central
        .mailbox_store
        .connection()
        .unwrap()
        .execute(
            "UPDATE mailbox_relay_notifications SET handed_off=1,queued=1",
            [],
        )
        .unwrap();
    {
        let mut inner = fixture.central.inner.lock().unwrap();
        for session in [&retired, &fixture.native] {
            let index = inner.find_session_index(session).unwrap();
            inner.sessions[index].queued_prompts.clear();
            sync_pending_prompts(&mut inner.sessions[index]);
        }
        let index = inner.find_session_index(&retired).unwrap();
        inner.sessions[index].hidden = true;
    }
    let mut boot = MailboxRelayBootRecovery::default();
    fixture
        .central
        .reconcile_mailbox_relay_notifications_after_boot(&mut boot)
        .await
        .unwrap();
    let unreconciled = || {
        fixture
            .central
            .mailbox_store
            .connection()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM mailbox_relay_notifications WHERE handed_off=1",
                [],
                |row| row.get::<_, u64>(0),
            )
            .unwrap()
    };
    assert_eq!(
        unreconciled(),
        10,
        "only eight boundaries are attempted in a page; seven healthy rows succeed despite the first retired target"
    );
    assert_eq!(boot.retry.len(), 1);
    assert!(!boot.scan_complete);
    fixture
        .central
        .reconcile_mailbox_relay_notifications_after_boot(&mut boot)
        .await
        .unwrap();
    assert_eq!(unreconciled(), 2);
    fixture
        .central
        .reconcile_mailbox_relay_notifications_after_boot(&mut boot)
        .await
        .unwrap();
    assert_eq!(unreconciled(), 1);
    assert!(boot.scan_complete);
    assert!(!boot.complete());
    let successful: i64 = fixture
        .central
        .mailbox_store
        .connection()
        .unwrap()
        .query_row(
            "SELECT rowid FROM mailbox_relay_notifications WHERE handed_off=0 LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    fixture
        .central
        .mailbox_store
        .connection()
        .unwrap()
        .execute(
            "UPDATE mailbox_relay_notifications SET handed_off=1,queued=1 WHERE rowid=?1",
            [successful],
        )
        .unwrap();
    fixture
        .central
        .reconcile_mailbox_relay_notifications_after_boot(&mut boot)
        .await
        .unwrap();
    assert_eq!(
        unreconciled(),
        2,
        "retrying the failed target must not reset a successful row after a new handoff"
    );
    assert_eq!(boot.retry.len(), 1);
    append_to_central(&fixture, "ordinary-work-after-boot-error");
    let mut worker = MailboxRelayWakeWorker {
        boot,
        ..Default::default()
    };
    worker.tick(&fixture.central, true).await;
    worker.shutdown().await;
    let delivered: u64 = fixture
        .central
        .mailbox_store
        .connection()
        .unwrap()
        .query_row(
            "SELECT count(*) FROM mailbox_relay_wakes WHERE delivered=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        delivered, 18,
        "an unresolved boot boundary does not suppress ordinary wake delivery"
    );
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_boot_scan_does_not_reset_notifications_created_after_its_snapshot() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    for index in 0..8 {
        append_to_central(&fixture, &format!("initial-boot-boundary-{index}"));
    }
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let mut boot = MailboxRelayBootRecovery::default();
    fixture
        .central
        .reconcile_mailbox_relay_notifications_after_boot(&mut boot)
        .await
        .unwrap();
    assert!(!boot.scan_complete);
    let (_, later) = append_to_central(&fixture, "new-boundary-after-boot-snapshot");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    fixture.central.mailbox_store.connection().unwrap().execute(
        "UPDATE mailbox_relay_notifications SET handed_off=1,queued=1 WHERE through_sequence=?1", [later.sequence]).unwrap();
    fixture
        .central
        .reconcile_mailbox_relay_notifications_after_boot(&mut boot)
        .await
        .unwrap();
    assert!(boot.complete());
    let later_handoff: bool = fixture
        .central
        .mailbox_store
        .connection()
        .unwrap()
        .query_row(
            "SELECT handed_off FROM mailbox_relay_notifications WHERE through_sequence=?1",
            [later.sequence],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        later_handoff,
        "a fresh process-owned handoff is outside the boot snapshot"
    );
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_supervisor_kick_drains_more_than_one_bounded_page() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    for index in 0..17 {
        append_to_central(&fixture, &format!("kick-page-{index}"));
    }
    let app = fixture.central.clone();
    let (stop, mut stop_rx) = tokio::sync::watch::channel(false);
    let (_host, mut host_rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(async move {
        let mut drain = FederationExecutorWakeDrain::default();
        let mut activation = FederationExecutorActivationWorker::default();
        let mut native = FederationMailboxWakeWorker::default();
        let mut relay = MailboxRelayWakeWorker {
            boot: MailboxRelayBootRecovery {
                scan_complete: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let stopped = federation_wait_supervisor_tick(
            &app,
            &mut drain,
            &mut activation,
            &mut native,
            &mut relay,
            &mut stop_rx,
            &mut host_rx,
            Duration::from_secs(3600),
        )
        .await;
        relay.shutdown().await;
        native.shutdown().await;
        activation.shutdown(&app).await;
        drain.shutdown().await;
        stopped
    });
    fixture.central.federation_executor_wake_signal.kick();
    let completed = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        loop {
            let delivered: u64 = fixture
                .central
                .mailbox_store
                .connection()
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM mailbox_relay_wakes WHERE delivered=1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            if delivered == 17 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    stop.send_replace(true);
    assert!(task.await.unwrap());
    assert!(
        completed.is_ok(),
        "kick must drain all pages without waiting for the reconcile timer"
    );
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_revocation_before_promotion_and_final_handoff_holds_only_the_wake() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for (final_handoff, disable) in [(false, false), (true, false), (false, true), (true, true)] {
        let fixture = Fixture::new(false).await;
        let _handles = central_handle(&fixture);
        let (append, receipt) = append_to_central(&fixture, "revoke-before-runtime");
        fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
        let input = claude_input(&fixture);
        let dispatch = final_handoff.then(|| {
            fixture
                .central
                .dispatch_next_queued_turn(&fixture.native, false)
                .unwrap()
                .unwrap()
        });
        let peer = fixture.channel.current_authority().unwrap();
        if disable {
            fixture
                .central
                .inner
                .lock()
                .unwrap()
                .preferences
                .federation
                .enabled = false;
        } else {
            fixture
                .central
                .transition_federation_peer(&peer, FederationMembershipState::Revoked)
                .unwrap();
        }
        if let Some(dispatch) = dispatch {
            assert!(!matches!(
                deliver_turn_dispatch(&fixture.central, dispatch),
                TurnDispatchDeliveryOutcome::Delivered
            ));
        } else {
            assert!(
                fixture
                    .central
                    .dispatch_next_queued_turn(&fixture.native, false)
                    .expect("authority loss is a nonfatal scheduling hold")
                    .is_none()
            );
        }
        assert!(
            matches!(
                input.try_recv(),
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected)
            ),
            "revocation winning before handoff cannot enqueue a runtime prompt"
        );
        let queued = fixture.central.inner.lock().unwrap().sessions[0]
            .queued_prompts
            .len();
        let retained = fixture
            .central
            .mailbox_store
            .mailbox_relay_notification(&append.target, &receipt.mailbox_id, receipt.sequence)
            .unwrap()
            .unwrap();
        assert!(
            !retained.acknowledged && !retained.handed_off,
            "a refused provider handoff cannot acknowledge or settle the obligation"
        );
        assert!(
            queued == 1 || !retained.queued,
            "a rejected carrier must leave either the exact queue wake or its durable pending recovery boundary"
        );
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_missing_notification_metadata_cannot_fail_open_at_drain() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    append_to_central(&fixture, "missing-notification");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let input = claude_input(&fixture);
    fixture
        .central
        .mailbox_store
        .connection()
        .unwrap()
        .execute("DELETE FROM mailbox_relay_notifications", [])
        .unwrap();
    assert!(
        fixture
            .central
            .dispatch_next_queued_turn(&fixture.native, false)
            .expect("missing relay metadata is a nonfatal scheduling hold")
            .is_none()
    );
    assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)));
    let queued = fixture.central.inner.lock().unwrap().sessions[0]
        .queued_prompts
        .len();
    assert_eq!(queued, 1);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_disabled_domain_without_relay_history_keeps_its_refusal() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    fixture
        .central
        .inner
        .lock()
        .unwrap()
        .preferences
        .federation
        .enabled = false;
    let error = list_mailboxes_with_availability(&fixture.native, &fixture.central)
        .await
        .err()
        .expect("an unavailable domain alone is not an empty successful list");
    assert_eq!(error.status, StatusCode::CONFLICT);
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_offline_home_local_history_and_remote_list_availability_are_independent()
 {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let (append, receipt) = append_to_central(&fixture, "offline-history");
    fixture
        .central
        .federation_peer_handles
        .lock()
        .unwrap()
        .clear();
    let summaries = fixture
        .central
        .list_native_mailbox_relays(&fixture.native)
        .await
        .unwrap();
    assert_eq!(summaries.mailboxes.len(), 1);
    assert!(summaries.unavailable_relay_homes.is_empty());
    let peer = fixture.channel.current_authority().unwrap();
    fixture
        .central
        .transition_federation_peer(&peer, FederationMembershipState::Revoked)
        .unwrap();
    fixture
        .central
        .inner
        .lock()
        .unwrap()
        .preferences
        .federation
        .enabled = false;
    let disabled_listing = list_mailboxes_with_availability(&fixture.native, &fixture.central)
        .await
        .unwrap();
    assert_eq!(
        disabled_listing.mailboxes.len(),
        1,
        "disabling dispatch does not hide a root's committed home-local relay history"
    );
    assert!(disabled_listing.unavailable_relay_homes.is_empty());
    let unavailable_domain = serde_json::to_value(
        disabled_listing
            .unavailable_central_domain
            .as_ref()
            .expect("the separate central-domain refusal remains explicit"),
    )
    .unwrap();
    assert_eq!(unavailable_domain["status"], 409);
    assert_eq!(unavailable_domain["availability"], "unavailable_unverified");
    assert!(unavailable_domain.get("unreadCount").is_none());
    assert!(unavailable_domain.get("mailboxes").is_none());
    let partial = list_mailboxes_api(
        AxumPath(fixture.native.clone()),
        State(fixture.central.clone()),
        Query(HashMap::new()),
    )
    .await
    .unwrap();
    assert_eq!(
        partial.headers()["x-termal-unavailable-central-domain"],
        "1"
    );
    assert!(partial.headers().contains_key("link"));
    let page: MailboxReadResponse = fixture
        .central
        .call_native_mailbox_home(
            &append.target,
            &append.conversation,
            MailboxRelayOperation::Read {
                actor: append.target.clone(),
                conversation: append.conversation.clone(),
                mailbox_id: receipt.mailbox_id.clone(),
                after_sequence: None,
                limit: 20,
                issue_receipt: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        page.messages.len(),
        1,
        "revocation does not erase local ownership"
    );
    let acknowledged: MailboxSummary = fixture
        .central
        .call_native_mailbox_home(
            &append.target,
            &append.conversation,
            MailboxRelayOperation::Acknowledge {
                actor: append.target.clone(),
                conversation: append.conversation.clone(),
                mailbox_id: receipt.mailbox_id.clone(),
                receipt: page.receipt.unwrap(),
            },
        )
        .await
        .unwrap();
    assert_eq!(acknowledged.unread_count, 0);
    fixture
        .satellite
        .mailbox_store
        .remember_mailbox_relay(
            &append.conversation,
            &receipt.mailbox_id,
            &["First".into(), "Second".into()],
            || Ok(()),
        )
        .unwrap();
    let raw = fixture
        .satellite
        .mailbox_store
        .append(&MailboxAppendInput {
            sender_session_id: "old-local-peer".into(),
            sender_name: "Old".into(),
            target_session_id: fixture.native.clone(),
            target_name: "Root".into(),
            body: "local history".into(),
            idempotency_key: "local-before-offline".into(),
            topic: None,
            state_stamp: None,
        })
        .unwrap()
        .receipt
        .clone();
    let listed = list_mailboxes_with_availability(&fixture.native, &fixture.satellite)
        .await
        .unwrap();
    assert!(
        listed
            .mailboxes
            .iter()
            .any(|mailbox| mailbox.id == raw.mailbox_id)
    );
    assert_eq!(listed.unavailable_relay_homes.len(), 1);
    let unavailable = serde_json::to_value(&listed.unavailable_relay_homes[0]).unwrap();
    assert_eq!(unavailable["availability"], "unavailable_unverified");
    assert!(unavailable.get("unreadCount").is_none());
    assert!(unavailable.get("messages").is_none());
    let response = list_mailboxes_api(
        AxumPath(fixture.native.clone()),
        State(fixture.satellite.clone()),
        Query(HashMap::new()),
    )
    .await
    .unwrap();
    assert_eq!(response.headers()["x-termal-unavailable-relay-homes"], "1");
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let legacy: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(legacy.is_array());
    assert!(
        legacy
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == raw.mailbox_id)
    );
    let response = list_mailboxes_api(
        AxumPath(fixture.native.clone()),
        State(fixture.satellite.clone()),
        Query(HashMap::from([(
            "includeRelayAvailability".into(),
            "true".into(),
        )])),
    )
    .await
    .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let detailed: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        detailed["unavailableRelayHomes"].as_array().unwrap().len(),
        1
    );
    fixture.finish().await;
}
