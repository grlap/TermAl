// Exact relay-send recovery after a real home commit with its HTTP reply lost.
// Reopens actual source SQLite and renews mutual proof; not a process-kill test.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_relay_lost_home_reply_reopens_source_pin_and_replays_exactly_once_on_fresh_link() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    let sender = fixture
        .satellite
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap();
    let target = fixture
        .central
        .native_mailbox_relay_descriptor(&fixture.native)
        .unwrap();
    let home = fixture.central.native_mailbox_relay_home().unwrap();
    let conversation = MailboxRelayConversation::new(
        home.clone(),
        sender.principal.clone(),
        target.principal.clone(),
    )
    .unwrap();
    // The home has not appended yet, so do not fabricate its association/id.
    // An unconfirmed sender pin is sufficient to recover the actual first send.
    let append = MailboxRelayAppend {
        requested_target: target.principal.participant_key(),
        conversation: conversation.clone(),
        sender: sender.principal.clone(),
        target: target.principal.clone(),
        sender_name: sender.name,
        target_name: target.name,
        message: "body whose home reply will be lost".into(),
        idempotency_key: "lost-home-reply".into(),
        topic: None,
        state_stamp: None,
    };
    let peer = fixture.satellite_channel.current_authority().unwrap();
    let old_home_lease = fixture.channel.current_lease().unwrap();
    let lease = fixture.satellite_channel.current_lease().unwrap();
    let authority = MailboxRelayAuthority {
        app: fixture.satellite.clone(),
        lease,
        native: vec![sender.principal.clone()],
        home: None,
    };
    fixture
        .satellite
        .mailbox_store
        .pin_mailbox_relay(&append, &peer, || authority.lock())
        .unwrap();
    let lose = Arc::new(AtomicBool::new(true));
    let dropped = lose.clone();
    let channel = fixture.channel.clone();
    let checked = channel.clone();
    let router = federation_router(
        channel,
        Arc::new(move || checked.current_authority().map(|_| ())),
    );
    let router = router.layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let dropped = dropped.clone();
            async move {
                let (parts, body) = request.into_parts();
                let bytes = axum::body::to_bytes(body, FEDERATION_MAILBOX_REQUEST_BYTES)
                    .await
                    .unwrap();
                let send = parts.uri.path() == MAILBOX_RELAY_RPC_ROUTE
                    && serde_json::from_slice::<MailboxRelayRequest>(&bytes).is_ok_and(|request| {
                        matches!(request.operation, MailboxRelayOperation::Send { .. })
                    });
                let response = next
                    .run(axum::extract::Request::from_parts(
                        parts,
                        axum::body::Body::from(bytes),
                    ))
                    .await;
                if send && response.status().is_success() && dropped.swap(false, Ordering::SeqCst) {
                    // Only remove the real authenticated response after execution.
                    // No fake success or fence is injected into the application.
                    StatusCode::BAD_GATEWAY.into_response()
                } else {
                    response
                }
            }
        },
    ));
    let mut server = DomainServer::start(router).await;
    let channel = fixture.satellite_channel.clone();
    let checked = channel.clone();
    let rpc = FederationRpcClient::new(
        server.port,
        channel,
        Arc::new(move || checked.current_authority().map(|_| ())),
    )
    .unwrap();
    let _handles = RelayHandleGuard(vec![fixture.satellite.clone()]);
    fixture
        .satellite
        .federation_peer_handles
        .lock()
        .unwrap()
        .insert(
            peer.peer.host_id.clone(),
            Arc::new(FederationPeerHandle {
                channel: fixture.satellite_channel.clone(),
                rpc,
            }),
        );
    let request = SendMailboxMessageRequest {
        target_session_id: append.requested_target.clone(),
        message: append.message.clone(),
        idempotency_key: append.idempotency_key.clone(),
        topic: None,
        state_stamp: None,
        class: Some("routine".into()),
    };
    let error = fixture
        .satellite
        .send_native_mailbox_relay(&fixture.native, &request)
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(error.message.contains(FEDERATION_MAILBOX_UNKNOWN_OUTCOME));
    assert!(!lose.load(Ordering::SeqCst));
    let original = fixture
        .central
        .mailbox_store
        .read_idempotent_message(
            &sender.principal.participant_key(),
            &request.idempotency_key,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        original.body, request.message,
        "the lost response followed a real commit"
    );
    assert_eq!(
        fixture
            .satellite
            .mailbox_store
            .connection()
            .unwrap()
            .query_row("SELECT count(*) FROM mailbox_messages", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
    fixture.reconnect().await;
    let current_peer = fixture.satellite_channel.current_authority().unwrap();
    assert!(same_mailbox_relay_enrollment(&peer, &current_peer));
    assert!(
        !peer.same_transport_as(&current_peer),
        "a real fresh incarnation advances channel accounting"
    );
    assert!(
        fixture
            .satellite
            .mailbox_store
            .pin_mailbox_relay(&append, &current_peer, || authority.lock())
            .is_err(),
        "historical-pin compatibility never authorizes the old in-flight source lease"
    );
    assert!(
        fixture
            .central
            .execute_mailbox_relay(
                MailboxRelayOperation::Send {
                    append: append.clone()
                },
                &old_home_lease,
                &sender.principal.host
            )
            .is_err(),
        "the old home-side operation cut loses after reconnect too"
    );
    let recovered = AppState {
        mailbox_store: Arc::new(MailboxStore::open(&fixture.satellite.mailbox_store.path).unwrap()),
        ..fixture.satellite.clone()
    };
    let receipt = recovered
        .send_native_mailbox_relay(&fixture.native, &request)
        .await
        .unwrap()
        .unwrap();
    let actual = MailboxRelayLocator::decode(&receipt.message_id, true)
        .unwrap()
        .unwrap();
    assert_eq!(actual.id, original.message_id);
    assert!(receipt.duplicate);
    let receipt_again = recovered
        .send_native_mailbox_relay(&fixture.native, &request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt_again.message_id, receipt.message_id);
    assert_eq!(
        fixture
            .central
            .mailbox_store
            .connection()
            .unwrap()
            .query_row("SELECT count(*) FROM mailbox_messages", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        1
    );
    let enrolled =
        { fixture.satellite.inner.lock().unwrap().federation.peers[&peer.peer.host_id].clone() };
    let mut changed = enrolled.clone();
    changed.endpoint.ssh_port += 1;
    fixture.satellite.enroll_federation_peer(changed).unwrap();
    fixture.satellite.enroll_federation_peer(enrolled).unwrap();
    let changed_back = fixture
        .satellite
        .federation_peer_snapshot(&peer.peer.host_id)
        .unwrap();
    assert_eq!(changed_back.endpoint, peer.endpoint);
    assert!(changed_back.enrollment_generation > peer.enrollment_generation);
    assert!(
        !same_mailbox_relay_enrollment(&peer, &changed_back),
        "changing material away and back cannot erase the enrollment fence"
    );
    assert!(
        recovered
            .mailbox_store
            .pin_mailbox_relay(&append, &changed_back, || Ok(()))
            .is_err()
    );
    server.finish().await;
    fixture.finish().await;
}
