// Second-review counterexamples. Real disposable stores and authenticated
// loopback calls; the deliberately slow failure is fixture-only, not load proof.
use super::*;

pub(super) fn user_prompt(text: &str) -> SendMessageRequest {
    SendMessageRequest {
        text: text.into(),
        expanded_text: None,
        attachments: Vec::new(),
        source_session_id: None,
        source_mailbox: None,
    }
}

pub(super) fn set_federation_enabled(app: &AppState, enabled: bool) {
    let mut settings = app.inner.lock().unwrap().preferences.federation.clone();
    settings.enabled = enabled;
    app.configure_federation(settings).unwrap();
}

fn append_to_satellite(fixture: &Fixture, key: &str) -> (MailboxRelayAppend, MailboxAppendReceipt) {
    let sender = fixture
        .central
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap();
    let target = fixture
        .satellite
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
        message: "satellite routine body".into(),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_r2_short_page_keeps_progress_after_slow_first_failure() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture=Fixture::with_router(false,|router|router.layer(axum::middleware::from_fn(
        |request:axum::http::Request<axum::body::Body>,next:axum::middleware::Next|async move {
            if request.uri().path()!="/federation/v1/mailbox/relay" { return next.run(request).await; }
            let (parts,body)=request.into_parts();
            let bytes=axum::body::to_bytes(body,128*1024).await.unwrap();
            if serde_json::from_slice::<MailboxRelayRequest>(&bytes).is_ok_and(|request|matches!(request.operation,MailboxRelayOperation::Wake { .. })) {
                tokio::time::sleep(Duration::from_millis(2100)).await;
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            next.run(axum::http::Request::from_parts(parts,axum::body::Body::from(bytes))).await
        }
    ))).await;
    let _handles = central_handle(&fixture);
    append_to_satellite(&fixture, "slow-first");
    let (healthy, receipt) = append_to_central(&fixture, "healthy-second");
    let progress = fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    assert!(
        progress.0 > 0,
        "a short page cut by the budget must retain the first attempted row"
    );
    fixture
        .central
        .run_mailbox_relay_wake_batch(progress.0, progress.1)
        .await;
    assert!(
        fixture
            .central
            .mailbox_store
            .mailbox_relay_notification(&healthy.target, &receipt.mailbox_id, receipt.sequence)
            .unwrap()
            .is_some(),
        "healthy work behind the failing target must be attempted on the next page"
    );
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_r2_ordinary_user_send_survives_a_refused_relay_head() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    for refusal in ["disabled", "revoked", "missing"] {
        let fixture = Fixture::new(false).await;
        let _handles = central_handle(&fixture);
        append_to_central(&fixture, "head-before-ordinary-user");
        fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
        let input = claude_input(&fixture);
        fixture.central.inner.lock().unwrap().sessions[0].orchestrator_auto_dispatch_blocked = true;
        match refusal {
            "disabled" => set_federation_enabled(&fixture.central, false),
            "revoked" => {
                let peer = fixture.channel.current_authority().unwrap();
                fixture
                    .central
                    .transition_federation_peer(&peer, FederationMembershipState::Revoked)
                    .unwrap();
            }
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
        let result = fixture.central.dispatch_turn(
            &fixture.native,
            user_prompt("ordinary work remains available"),
        );
        assert!(
            result.is_ok(),
            "{refusal}: unrelated wake authority must not refuse ordinary admission: {:?}", result.as_ref().err()
        );
        if let Ok(
            DispatchTurnResult::Dispatched(dispatch)
            | DispatchTurnResult::DispatchedAfterQueue(dispatch),
        ) = result
        {
            deliver_turn_dispatch(&fixture.central, dispatch).unwrap();
            match recv_within_guard(&input, "ordinary prompt") {
                Ok(ClaudeRuntimeCommand::Prompt(prompt)) => {
                    assert_eq!(prompt.text, "ordinary work remains available")
                }
                _ => panic!("expected the ordinary user prompt, not another runtime command"),
            }
        }
        fixture.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_r2_authority_return_redrives_retained_head_and_following_user_work() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    append_to_central(&fixture, "redrive-without-new-mail");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    let input = claude_input(&fixture);
    fixture.central.inner.lock().unwrap().sessions[0]
        .session
        .status = SessionStatus::Active;
    assert!(matches!(
        fixture
            .central
            .dispatch_turn(&fixture.native, user_prompt("work behind the relay"))
            .unwrap(),
        DispatchTurnResult::Queued
    ));
    set_federation_enabled(&fixture.central, false);
    assert!(
        fixture
            .central
            .finish_turn_ok_if_runtime_matches(
                &fixture.native,
                &RuntimeToken::Claude("relay-review-authority".into())
            )
            .is_ok()
    );
    set_federation_enabled(&fixture.central, true);
    // Re-enabling policy deliberately clears old verification and returns the
    // peer to Enrolled. It does not itself restore dispatch authority.
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    assert!(matches!(input.try_recv(),Err(mpsc::TryRecvError::Empty)));
    // A settings transition invalidates the old transport basis as well as its
    // proof. Rebuild both ends from current snapshots before mutual attestation.
    // The remote endpoint still owns the original proof until its guard drops;
    // a fresh channel must not replace that live incarnation implicitly.
    drop(fixture.attested.lock().unwrap().take());
    let central_channel = FederationChannel::for_app(
        fixture.central.federation_peer_snapshot(&fixture.channel.snapshot.peer.host_id).unwrap(),
        FederationPairCapability::new("a".repeat(64)).unwrap(),
        fixture.central.clone(),
    );
    let satellite_channel = FederationChannel::for_app(
        fixture.satellite.federation_peer_snapshot(&fixture.satellite_channel.snapshot.peer.host_id).unwrap(),
        FederationPairCapability::new("a".repeat(64)).unwrap(),
        fixture.satellite.clone(),
    );
    central_channel.current_authority().expect("central reconnect basis must be current and disconnected");
    satellite_channel.current_authority().expect("satellite reconnect basis must be current and disconnected");
    let _fresh_proof = federation_control_fixture::attest(&central_channel, &satellite_channel).await;
    let peer=fixture.central.federation_peer_snapshot(&central_channel.snapshot.peer.host_id).unwrap();
    fixture.central.verify_federation_peer(&peer,federation_registry::checks()).unwrap();
    let peer=fixture.central.federation_peer_snapshot(&peer.peer.host_id).unwrap();
    fixture.central.transition_federation_peer(&peer,FederationMembershipState::Active).unwrap();
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    assert!(
        matches!(input.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))),
        "restoring authority must re-drive the retained head without a new send"
    );
    fixture
        .central
        .finish_turn_ok_if_runtime_matches(
            &fixture.native,
            &RuntimeToken::Claude("relay-review-authority".into()),
        )
        .unwrap();
    match recv_within_guard(&input, "following user work") {
        Ok(ClaudeRuntimeCommand::Prompt(prompt)) => {
            assert_eq!(prompt.text, "work behind the relay")
        }
        _ => panic!("expected following user work, not another runtime command"),
    }
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_r2_remote_home_reconnect_redrives_without_new_mail() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _central_handle = central_handle(&fixture);
    let channel = fixture.channel.clone();
    let checked = channel.clone();
    let mut home_server = DomainServer::start(federation_router(
        channel,
        Arc::new(move || checked.current_authority().map(|_| ())),
    )).await;
    let channel = fixture.satellite_channel.clone();
    let checked = channel.clone();
    let rpc = FederationRpcClient::new(
        home_server.port,
        channel.clone(),
        Arc::new(move || checked.current_authority().map(|_| ())),
    ).unwrap();
    let _satellite_handle = RelayHandleGuard(vec![fixture.satellite.clone()]);
    fixture.satellite.federation_peer_handles.lock().unwrap().insert(
        channel.snapshot.peer.host_id.clone(),
        Arc::new(FederationPeerHandle { channel, rpc }),
    );
    fixture.satellite.inner.lock().unwrap().sessions[0].orchestrator_auto_dispatch_blocked = true;
    let (append, receipt) = append_to_satellite(&fixture, "remote-home-reconnect");
    fixture.central.run_mailbox_relay_wake_batch(0, 0).await;
    assert_eq!(fixture.satellite.inner.lock().unwrap().sessions[0].queued_prompts.len(), 1);
    let input = claude_input_for(&fixture.satellite, &fixture.native);
    fixture.satellite.inner.lock().unwrap().sessions[0].session.status = SessionStatus::Active;
    assert!(matches!(fixture.satellite.dispatch_turn(
        &fixture.native, user_prompt("satellite user behind relay")
    ).unwrap(), DispatchTurnResult::Queued));
    // Drop the actual proof owners, not enrollment or a mocked availability bit.
    // The runtime's completion occurs while the remote home has no current lease.
    drop(fixture.attested.lock().unwrap().take());
    assert!(fixture.satellite.finish_turn_ok_if_runtime_matches(
        &fixture.native, &RuntimeToken::Claude("relay-review-authority".into())
    ).is_ok());
    fixture.satellite.run_mailbox_relay_wake_batch(0, 0).await;
    assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)));
    fixture.reconnect().await;
    // This minimal fixture has no retained onboarding report. A fresh proof
    // therefore leaves membership Enrolled until fixture readiness is restored.
    fixture.satellite.run_mailbox_relay_wake_batch(0, 0).await;
    assert!(matches!(input.try_recv(), Err(mpsc::TryRecvError::Empty)));
    for (app, peer_id) in [
        (&fixture.central, &fixture.channel.snapshot.peer.host_id),
        (&fixture.satellite, &fixture.satellite_channel.snapshot.peer.host_id),
    ] {
        let peer = app.federation_peer_snapshot(peer_id).unwrap();
        assert_eq!(peer.membership, FederationMembershipState::Enrolled);
        app.verify_federation_peer(&peer, federation_registry::checks()).unwrap();
        let peer = app.federation_peer_snapshot(peer_id).unwrap();
        app.transition_federation_peer(&peer, FederationMembershipState::Active).unwrap();
    }
    let notification = fixture.satellite.mailbox_store.mailbox_relay_notification(
        &append.target, &receipt.mailbox_id, receipt.sequence
    ).unwrap().unwrap();
    assert!(!notification.handed_off && notification.queued,
        "a refused remote-home carrier must still be retained, not handed off");
    fixture.satellite.mailbox_relay_dispatch_authority(
        &notification.wake, notification.peer.clone(), None
    ).expect("reconnected retained wake must have current authority");
    fixture.satellite.mailbox_relay_peer(&append.target, &append.conversation)
        .expect("reconnected home route must use the current lease");
    fixture.satellite.run_mailbox_relay_wake_batch(0, 0).await;
    assert!(matches!(input.try_recv(), Ok(ClaudeRuntimeCommand::Prompt(_))),
        "a fresh mutual proof must re-drive the existing remote-home carrier");
    fixture.satellite.finish_turn_ok_if_runtime_matches(
        &fixture.native, &RuntimeToken::Claude("relay-review-authority".into())
    ).unwrap();
    match recv_within_guard(&input, "satellite user after reconnect") {
        Ok(ClaudeRuntimeCommand::Prompt(prompt)) => assert_eq!(prompt.text, "satellite user behind relay"),
        _ => panic!("expected following satellite user prompt"),
    }
    home_server.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_review_r2_reserved_review_topic_is_refused_before_public_append() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let _handles = central_handle(&fixture);
    let (append, receipt) = append_to_central(&fixture, "establish-routine-conversation");
    let request=serde_json::from_value::<SendMailboxApiRequest>(json!({"targetSessionId":append.sender.participant_key(),"message":"must remain readable routine mail","idempotencyKey":"reserved-topic-refusal","topic":"delegation-review-result/v1"})).unwrap();
    let result = send_mailbox_message(
        AxumPath(fixture.native.clone()),
        State(fixture.central.clone()),
        Json(request),
    )
    .await;
    assert_eq!(
        result
            .err()
            .expect("reserved internal topic is not routine mail")
            .status,
        StatusCode::BAD_REQUEST
    );
    let latest: u64 = fixture
        .central
        .mailbox_store
        .connection()
        .unwrap()
        .query_row(
            "SELECT coalesce(max(sequence),0) FROM mailbox_messages WHERE mailbox_id=?1",
            [receipt.mailbox_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(latest, 1, "refusal must precede append and wake creation");
    fixture.finish().await;
}
