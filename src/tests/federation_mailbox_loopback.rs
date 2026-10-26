// Owns the native two-process mailbox protocol and central-outage matrix.
// Does not claim real SSH, production activation or M3/M4 crash recovery. New
// integration proof using real domain HTTP and isolated stores on both hosts.
use super::federation_mailbox_process::*;
use super::*;

struct DiscoveryHandleCleanup(AppState);
impl Drop for DiscoveryHandleCleanup {
    fn drop(&mut self) { self.0.federation_peer_handles.lock().unwrap().clear(); }
}

async fn api(
    client: &reqwest::Client,
    port: u16,
    session: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let url = format!("http://127.0.0.1:{port}/api/sessions/{session}/{path}");
    let response = match body {
        Some(body) => client.post(url).json(&body).send().await,
        None => client.get(url).send().await,
    }
    .unwrap();
    let status = response.status();
    let value = response.json::<Value>().await.unwrap();
    (status, value)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_process_all_operations_use_c_and_outage_never_creates_satellite_mailbox_state() {
    let central = federation_mailboxes::central_store();
    let (local, participant) = federation_mailboxes::local_participant(&central, "Central native");
    let (mut satellite, native) = tokio::task::spawn_blocking(Satellite::spawn).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut record = federation_registry::peer(port);
    record.identity = native.host.clone();
    record.display_name = "Satellite".to_owned();
    record.endpoint.satellite_federation_port = native.control_port;
    record.endpoint.satellite_reverse_forward_port = port;
    central.enroll_federation_peer(record.clone()).unwrap();
    let peer = central
        .federation_peer_snapshot(&record.identity.host_id)
        .unwrap();
    let channel = FederationChannel::for_app(
        peer.clone(),
        FederationPairCapability::new("a".repeat(64)).unwrap(),
        central.clone(),
    );
    let _handles = DiscoveryHandleCleanup(central.clone());
    let rpc_check = channel.clone();
    central.federation_peer_handles.lock().unwrap().insert(native.host.host_id.clone(),
        Arc::new(FederationPeerHandle {
            channel: channel.clone(),
            rpc: FederationRpcClient::new(native.control_port, channel.clone(),
                Arc::new(move || rpc_check.current_authority().map(|_| ()))).unwrap(),
        }));
    let check = channel.clone();
    let router = federation_router(
        channel.clone(),
        Arc::new(move || check.current_authority().map(|_| ())),
    );
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { federation_wait_stop(&mut stopped).await })
            .await
            .unwrap();
    });
    let config = Configuration {
        central: peer.local.clone(),
        record,
        port,
    };
    let (next, satellite_port) = tokio::task::spawn_blocking(move || {
        let port = satellite.configure(&config);
        (satellite, port)
    })
    .await
    .unwrap();
    satellite = next;
    // C initiates the real control proof to the second process before either
    // side verifies membership or attaches an application placement binding.
    let mut control: FederationControlIo = Box::new(
        tokio::net::TcpStream::connect(("127.0.0.1", native.control_port))
            .await
            .unwrap(),
    );
    let (context, guard) = tokio::time::timeout(
        phase_sync::DEADLOCK_GUARD,
        federation_attest_central(&channel, &mut control),
    )
    .await
    .unwrap()
    .unwrap();
    let (control_stop, control_rx) = tokio::sync::watch::channel(false);
    let (ready, ready_rx) = tokio::sync::oneshot::channel();
    let control_task = tokio::spawn(federation_control_central(
        channel.clone(),
        control,
        context,
        guard,
        control_rx,
        ready,
    ));
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, ready_rx)
        .await
        .unwrap()
        .unwrap();
    let connected = central
        .federation_peer_snapshot(&peer.peer.host_id)
        .unwrap();
    central
        .verify_federation_peer(&connected, federation_registry::checks())
        .unwrap();
    let verified = central
        .federation_peer_snapshot(&peer.peer.host_id)
        .unwrap();
    central
        .transition_federation_peer(&verified, FederationMembershipState::Active)
        .unwrap();
    let peer = central
        .federation_peer_snapshot(&peer.peer.host_id)
        .unwrap();
    let placement = central
        .reserve_federation_placement(
            &peer,
            "remote-session".to_owned(),
            native.session.clone(),
            0,
            0,
        )
        .unwrap();
    let binding = FederationMailboxBinding {
        central: peer.local.clone(),
        identity: placement.identity,
        placement_generation: placement.placement_generation,
    };
    let child_binding = binding.clone();
    let (next, bound) = tokio::task::spawn_blocking(move || {
        let result = satellite.command(json!({"command":"bind", "binding":child_binding}));
        (satellite, result)
    })
    .await
    .unwrap();
    satellite = next;
    assert_eq!(bound, json!({"bound":true}));
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let (status, protocol) = api(
        &client,
        satellite_port,
        &native.session,
        "mailboxes/protocol",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "protocol response: {protocol}");
    assert_eq!(
        protocol["central"],
        serde_json::to_value(&peer.local).unwrap()
    );
    let (status, directory) = api(
        &client,
        satellite_port,
        &native.session,
        "mailboxes/participants",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "directory response: {directory}");
    assert_eq!(directory.as_array().unwrap().len(), 2);
    let (status, created) = api(
        &client,
        satellite_port,
        &native.session,
        "mailboxes/create",
        Some(json!({"targetSessionId":participant.session_id})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create response: {created}");
    let mailbox = created["id"].as_str().unwrap().to_owned();
    // More than the old transport-wide 64 KiB response ceiling, and a body
    // whose escaped JSON is much larger than the handshake's 4 KiB ceiling.
    let outbound = "ż".repeat(40 * 1024);
    let send = json!({"targetSessionId":participant.session_id,"message":outbound,"idempotencyKey":"from-satellite"});
    let (status, remote_send) = api(
        &client,
        satellite_port,
        &native.session,
        "mailboxes/send",
        Some(send.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(remote_send["mailboxId"], mailbox);
    let (status, duplicate) = api(
        &client,
        satellite_port,
        &native.session,
        "mailboxes/send",
        Some(send),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(duplicate["messageId"], remote_send["messageId"]);
    assert_eq!(duplicate["duplicate"], true);
    let reply: MailboxAppendReceipt = central
        .federation_mailbox_operation(
            &local,
            FederationMailboxOperation::Send {
                target: binding.participant_key(),
                message: "\u{1}".repeat(100 * 1024),
                key: "central-reply".to_owned(),
                topic: None,
                state_stamp: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(reply.mailbox_id, mailbox);
    assert_eq!(reply.notification_disposition, "durableButNotWoken");
    assert_eq!(
        reply.notification_cause.as_deref(),
        Some(FEDERATION_REMOTE_WAKE_CAUSE)
    );
    assert_eq!(
        reply.sender_processed_through, 0,
        "sending cannot consume an unread satellite message"
    );
    let (status, list) = api(&client, satellite_port, &native.session, "mailboxes", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    let read_path = format!("mailboxes/{mailbox}/read");
    let (status, page) = api(
        &client,
        satellite_port,
        &native.session,
        &read_path,
        Some(json!({"afterSequence":0,"limit":50,"issueReceipt":true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["messages"].as_array().unwrap().len(), 2);
    assert_eq!(page["readerSessionId"], binding.participant_key());
    let (status, exact) = api(
        &client,
        satellite_port,
        &native.session,
        &format!("mailbox-messages/{}", reply.message_id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(exact["notificationCause"], "remote wake pending; awaiting matching durable receiver receipt");
    let ack_path = format!("mailboxes/{mailbox}/acknowledge");
    let (status, acked) = api(
        &client,
        satellite_port,
        &native.session,
        &ack_path,
        Some(json!({"receipt":page["receipt"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(acked["unreadCount"], 0);
    let (status, _) = api(
        &client,
        satellite_port,
        &native.session,
        &ack_path,
        Some(json!({"expectedProcessedThrough":0,"processedThrough":2})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (next, inspection) = tokio::task::spawn_blocking(move || {
        let value = satellite.command(json!({"command":"inspect"}));
        (satellite, value)
    })
    .await
    .unwrap();
    satellite = next;
    assert_eq!(
        inspection,
        json!({"directory":0,"mailboxes":0,"messages":0}),
        "satellite keeps routing fields, no mailbox replica"
    );
    assert_eq!(
        central
            .mailbox_store
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM mailbox_messages", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        2
    );
    stop.send_replace(true);
    server.await.unwrap();
    let (status, listing) = api(
        &client,
        satellite_port,
        &native.session,
        "mailboxes?includeRelayAvailability=true",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listing["mailboxes"], json!([]));
    assert_eq!(listing["unavailableCentralDomain"]["status"], 503);
    assert!(listing["unavailableCentralDomain"]["error"].is_string());
    for (path, body) in [
        (read_path, Some(json!({"issueReceipt":true}))),
        (format!("mailbox-messages/{}", reply.message_id), None),
        (ack_path, Some(json!({"receipt":page["receipt"]}))),
        (
            "mailboxes/create".to_owned(),
            Some(json!({"targetSessionId":participant.session_id})),
        ),
        (
            "mailboxes/send".to_owned(),
            Some(
                json!({"targetSessionId":participant.session_id,"message":"not queued locally","idempotencyKey":"offline"}),
            ),
        ),
    ] {
        assert_eq!(
            api(&client, satellite_port, &native.session, &path, body)
                .await
                .0,
            StatusCode::SERVICE_UNAVAILABLE,
            "{path}"
        );
    }
    let (next, inspection) = tokio::task::spawn_blocking(move || {
        let value = satellite.command(json!({"command":"inspect"}));
        (satellite, value)
    })
    .await
    .unwrap();
    satellite = next;
    assert_eq!(
        inspection,
        json!({"directory":0,"mailboxes":0,"messages":0})
    );
    let (next, recreated) = tokio::task::spawn_blocking(move || {
        let value = satellite.command(json!({"command":"recreate"}));
        (satellite, value)
    })
    .await
    .unwrap();
    satellite = next;
    assert_eq!(
        recreated,
        json!({"status":409,"bound":false}),
        "old central install cannot bind a recreated native session"
    );
    control_stop.send_replace(true);
    // The outage phase may already have fenced this exact context.
    let _ = control_task.await.unwrap();
    tokio::task::spawn_blocking(move || satellite.finish())
        .await
        .unwrap();
}
