// Exact-message relay observations through the real home and authenticated
// target RPC. Extends relay_review.rs; does not fabricate delivery or ACKs.
use super::super::production_shape::ProxySend;
use super::*;

fn append_to_satellite(fixture: &Fixture, key: &str) -> (MailboxRelayAppend, MailboxAppendReceipt) {
    // Only the explicit fake-runtime step below may dispatch a provider turn.
    fixture
        .satellite
        .inner
        .lock()
        .unwrap()
        .session_mut(&fixture.native)
        .unwrap()
        .orchestrator_auto_dispatch_blocked = true;
    let sender = fixture
        .central
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap();
    let target = fixture
        .satellite
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap();
    let append = MailboxRelayAppend {
        test_request: None,
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
        message: "body remains at the real home".into(),
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

async fn exact_home_read(fixture: &Fixture, receipt: &MailboxAppendReceipt) -> MailboxMessage {
    fixture
        .central
        .read_native_mailbox_relay_message(
            &fixture.native,
            MailboxRelayLocator {
                version: MAILBOX_RELAY_VERSION,
                home: fixture.central.native_mailbox_relay_home().unwrap(),
                id: receipt.message_id.clone(),
            },
        )
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_exact_status_distinguishes_home_admission_target_queue_and_provider_handoff() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    // Admission revalidation needs the target's authenticated route home too.
    let proxy = ProxySend::new(&fixture).await;
    let (_, receipt) = append_to_satellite(&fixture, "exact-progress-phases");
    let immutable_receipt = serde_json::to_value(&receipt).unwrap();
    let before = exact_home_read(&fixture, &receipt).await;
    let progress = before
        .current_status
        .as_ref()
        .unwrap()
        .relay_progress
        .as_ref()
        .unwrap();
    assert_eq!(progress.home_queue_admission_confirmed, Some(false));
    assert!(
        matches!(
            progress.target,
            MailboxRelayTargetProgress::Unavailable { .. }
        ),
        "a home append alone has no target notification observation"
    );
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let queued = exact_home_read(&fixture, &receipt).await;
    let status = queued.current_status.as_ref().unwrap();
    assert!(!status.recipient_acknowledged);
    let progress = status.relay_progress.as_ref().unwrap();
    assert_eq!(progress.home_queue_admission_confirmed, Some(true));
    assert!(
        status
            .summary
            .contains("The home recorded target queue admission.")
    );
    assert!(status.summary.contains("no provider handoff recorded"));
    assert!(!status.summary.contains("No wake confirmed"));
    match &progress.target {
        MailboxRelayTargetProgress::Observed {
            queue_admission_recorded,
            provider_handoff_recorded,
            currently_queued,
            current_session_holds,
            recovery,
            ..
        } => {
            assert_eq!(*queue_admission_recorded, Some(true));
            assert_eq!(*provider_handoff_recorded, Some(false));
            assert!(*currently_queued);
            assert!(current_session_holds.contains(&MailboxRelayTargetHold::Paused));
            assert!(recovery.contains("Do not resend"));
        }
        _ => panic!("the connected target reports its actual paused queue"),
    }
    let input = claude_input_for(&fixture.satellite, &fixture.native);
    let dispatch = fixture
        .satellite
        .dispatch_next_queued_turn(&fixture.native, false)
        .unwrap()
        .unwrap();
    deliver_turn_dispatch(&fixture.satellite, dispatch).unwrap();
    assert!(matches!(
        recv_within_guard(&input, "target provider handoff"),
        Ok(ClaudeRuntimeCommand::Prompt(_))
    ));
    let handed_off = exact_home_read(&fixture, &receipt).await;
    match &handed_off
        .current_status
        .as_ref()
        .unwrap()
        .relay_progress
        .as_ref()
        .unwrap()
        .target
    {
        MailboxRelayTargetProgress::Observed {
            provider_handoff_recorded,
            currently_queued,
            current_session_holds,
            ..
        } => {
            assert_eq!(*provider_handoff_recorded, Some(true));
            assert!(!currently_queued);
            assert!(current_session_holds.contains(&MailboxRelayTargetHold::ActiveTurn));
        }
        _ => panic!("target handoff must be separately observed"),
    }
    assert!(
        !handed_off
            .current_status
            .as_ref()
            .unwrap()
            .recipient_acknowledged,
        "a provider handoff is not a recipient protocol ACK"
    );
    assert_eq!(
        before.notification_state, handed_off.notification_state,
        "the stored conservative home disposition is not rewritten as provider delivery"
    );
    assert_eq!(serde_json::to_value(&receipt).unwrap(), immutable_receipt);
    assert_eq!(
        fixture
            .central
            .mailbox_store
            .connection()
            .unwrap()
            .query_row("SELECT count(*) FROM mailbox_read_pages", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        0,
        "exact status reads issue no page receipts"
    );
    proxy.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_exact_status_keeps_home_history_when_target_progress_is_unavailable() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let proxy = ProxySend::new(&fixture).await;
    let (_, receipt) = append_to_satellite(&fixture, "target-unavailable");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    proxy.finish().await;
    let read = exact_home_read(&fixture, &receipt).await;
    assert_eq!(read.body, "body remains at the real home");
    let status = read.current_status.as_ref().unwrap();
    assert!(!status.recipient_acknowledged);
    let progress = status.relay_progress.as_ref().unwrap();
    assert_eq!(progress.home_queue_admission_confirmed, Some(true));
    match &progress.target {
        MailboxRelayTargetProgress::Unavailable {
            reason, recovery, ..
        } => {
            assert!(!reason.is_empty());
            assert!(recovery.contains("Do not resend"));
        }
        _ => panic!("a lost target link cannot reuse an earlier observation"),
    }
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_exact_status_separates_target_admission_from_an_unconfirmed_home_wake() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    // Deliberately omit the reverse handle: target admission succeeds, but
    // dispatch revalidation cannot resolve its current authority back home.
    let _handles = central_handle(&fixture);
    let (append, receipt) = append_to_satellite(&fixture, "one-way-progress");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;

    let read = exact_home_read(&fixture, &receipt).await;
    let status = read.current_status.as_ref().unwrap();
    assert!(!status.recipient_acknowledged);
    let progress = status.relay_progress.as_ref().unwrap();
    assert_eq!(progress.home_queue_admission_confirmed, Some(false));
    match &progress.target {
        MailboxRelayTargetProgress::Observed {
            queue_admission_recorded,
            provider_handoff_recorded,
            currently_queued,
            ..
        } => {
            assert_eq!(*queue_admission_recorded, Some(true));
            assert_eq!(*provider_handoff_recorded, Some(false));
            assert!(*currently_queued);
        }
        _ => panic!("the forward link still observes actual target admission"),
    }
    let retained = fixture
        .satellite
        .mailbox_store
        .mailbox_relay_notification(&append.target, &receipt.mailbox_id, receipt.sequence)
        .unwrap()
        .unwrap();
    let target_error = fixture
        .satellite
        .mailbox_relay_dispatch_authority(&retained.wake, retained.peer, None)
        .err()
        .expect("the target has no authenticated reverse route home");
    assert_eq!(target_error.status, StatusCode::CONFLICT);
    assert_eq!(
        target_error.message,
        "native relay home unavailable; obligation held"
    );

    let (refusals, _) = fixture
        .central
        .mailbox_store
        .relay_home_wake_refusals
        .snapshot();
    let refusal = refusals
        .iter()
        .find(|refusal| refusal.message_id == receipt.message_id)
        .expect("the home retains this exact unconfirmed wake");
    // Authenticated RPC intentionally hides the target's private error body;
    // the home records the bounded public refusal, not the private cause.
    assert_eq!(refusal.status, StatusCode::INTERNAL_SERVER_ERROR.as_u16());
    let public_error = federation_mailbox_unknown_outcome(
        StatusCode::INTERNAL_SERVER_ERROR,
        "native mailbox relay response unavailable",
    );
    assert_eq!(
        refusal.reason,
        mailbox_relay_bounded_reason(&public_error.message)
    );
    assert!(!refusal.reason.contains(&target_error.message));
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_progress_rpc_refuses_wrong_conversation_and_replaced_target() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let _handles = central_handle(&fixture);
    let (append, receipt) = append_to_satellite(&fixture, "progress-identity");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let operation = |target: MailboxRelayPrincipal| MailboxRelayOperation::InspectWake {
        actor: append.sender.clone(),
        conversation: append.conversation.clone(),
        target,
        mailbox_id: receipt.mailbox_id.clone(),
        sequence: receipt.sequence,
    };
    let mut impostor = append.target.clone();
    impostor.native_creation_id = Uuid::new_v4().to_string();
    assert!(
        request(&fixture.rpc, &fixture.channel, operation(impostor))
            .await
            .is_err()
    );
    {
        let mut inner = fixture.satellite.inner.lock().unwrap();
        inner
            .session_mut(&fixture.native)
            .unwrap()
            .federation_native_creation_id = Uuid::new_v4().to_string();
    }
    assert!(
        request(
            &fixture.rpc,
            &fixture.channel,
            operation(append.target.clone())
        )
        .await
        .is_err()
    );
    let read = exact_home_read(&fixture, &receipt).await;
    assert!(matches!(
        read.current_status.unwrap().relay_progress.unwrap().target,
        MailboxRelayTargetProgress::Unavailable { .. }
    ));
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_exact_status_rechecks_its_caller_after_the_target_observation() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let _handles = central_handle(&fixture);
    let (_, receipt) = append_to_satellite(&fixture, "caller-replaced-after-home-read");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;

    let replaced = Arc::new(AtomicBool::new(false));
    let witness = replaced.clone();
    let caller_app = fixture.central.clone();
    let caller_id = fixture.native.clone();
    let checked = fixture.satellite_channel.clone();
    let router = federation_router(
        fixture.satellite_channel.clone(),
        Arc::new(move || checked.current_authority().map(|_| ())),
    );
    let router = router.layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let caller_app = caller_app.clone();
            let caller_id = caller_id.clone();
            let replaced = replaced.clone();
            async move {
                let (parts, body) = request.into_parts();
                let bytes = axum::body::to_bytes(body, FEDERATION_MAILBOX_REQUEST_BYTES)
                    .await
                    .unwrap();
                let inspect = parts.uri.path() == MAILBOX_RELAY_RPC_ROUTE
                    && serde_json::from_slice::<MailboxRelayRequest>(&bytes).is_ok_and(|request| {
                        matches!(request.operation, MailboxRelayOperation::InspectWake { .. })
                    });
                let response = next
                    .run(axum::extract::Request::from_parts(
                        parts,
                        axum::body::Body::from(bytes),
                    ))
                    .await;
                if inspect
                    && response.status().is_success()
                    && !replaced.swap(true, Ordering::SeqCst)
                {
                    // The genuine target answered after the home read. Replace
                    // only the local caller before that response is returned.
                    caller_app
                        .inner
                        .lock()
                        .unwrap()
                        .session_mut(&caller_id)
                        .unwrap()
                        .federation_native_creation_id = Uuid::new_v4().to_string();
                }
                response
            }
        },
    ));
    let mut server = DomainServer::start(router).await;
    let checked = fixture.channel.clone();
    let rpc = FederationRpcClient::new(
        server.port,
        fixture.channel.clone(),
        Arc::new(move || checked.current_authority().map(|_| ())),
    )
    .unwrap();
    let peer = fixture.channel.current_authority().unwrap();
    fixture
        .central
        .federation_peer_handles
        .lock()
        .unwrap()
        .insert(
            peer.peer.host_id,
            Arc::new(FederationPeerHandle {
                channel: fixture.channel.clone(),
                rpc,
            }),
        );

    let error = fixture
        .central
        .read_native_mailbox_relay_message(
            &fixture.native,
            MailboxRelayLocator {
                version: MAILBOX_RELAY_VERSION,
                home: fixture.central.native_mailbox_relay_home().unwrap(),
                id: receipt.message_id,
            },
        )
        .await
        .err()
        .expect("a replaced caller cannot receive the earlier home's body");
    assert!(
        witness.load(Ordering::SeqCst),
        "the real target observation reached the response boundary"
    );
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("caller was replaced"));
    server.finish().await;
    fixture.finish().await;
}
