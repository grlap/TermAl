// Owns monitor behavior under full mailbox admission and relayed uncertainty.
// Does not change production clocks or launch real SSH. Split from federation
// runtime tests to exercise the two production monitor loops at saturation.
use super::*;

fn probe_observer(
    channel: FederationChannel,
    enabled: Arc<AtomicBool>,
    observed: tokio::sync::mpsc::UnboundedSender<()>,
) -> FederationAuthorityCheck {
    Arc::new(move || {
        channel.current_authority()?;
        if enabled.load(Ordering::SeqCst) {
            let _ = observed.send(());
        }
        Ok(())
    })
}

// These tests deliberately use one Tokio thread. Once the observer sends,
// a locally rejected probe and its monitor branch run without an await before
// yielding. Reading the lease after recv therefore observes that branch's
// completed decision, not a timing guess or a sleep-based absence check.
#[tokio::test]
async fn central_monitor_keeps_its_link_when_four_mailbox_slots_are_admitted() {
    let mut fixture = Fixture::new().await;
    let snapshot = fixture.app.federation_peer_snapshot(&fixture.id).unwrap();
    // This monitor regression needs an authenticated transport, not a second
    // M1 persistence lifecycle. Connection durability has its own tests.
    let channel = FederationChannel::new(snapshot.clone(),
        FederationPairCapability::new("a".repeat(64)).unwrap());
    let enabled = Arc::new(AtomicBool::new(false));
    let (observed, mut received) = tokio::sync::mpsc::unbounded_channel();
    let port = fixture.reserve_forward_listener();
    let client = FederationRpcClient::new(port, channel.clone(),
        probe_observer(channel.clone(), enabled.clone(), observed)).unwrap();
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let (finished, mut completion) = tokio::sync::oneshot::channel();
    let io = fixture.io.clone();
    let monitor = tokio::spawn(async move {
        let server = tokio::spawn(std::future::pending::<std::io::Result<()>>());
        let result = federation_central_monitor(&snapshot, &client,
            PathBuf::from("unused-fake-pin"), port, &mut stopped,
            &mut FederationBackoff::default(), &server, &io,
            &mut FederationAttemptState::default()).await;
        server.abort();
        let _ = server.await;
        let _ = finished.send(result.as_ref().err().map(|error|
            format!("{}: {}", error.status, error.message)));
        result
    });
    let mut startup_trace = Vec::new();
    let startup = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        loop {
            tokio::select! {
                result = &mut completion => return Err(format!("monitor ended: {result:?}")),
                event = fixture.events.recv() => match event {
                    Some(Event::Response(path, status)) => {
                        if path.ends_with("health") && status.is_success() { return Ok(()); }
                        if startup_trace.len() < 16 { startup_trace.push(format!("{path}: {status}")); }
                    }
                    Some(Event::Started) => startup_trace.push("SSH started".to_owned()),
                    Some(Event::Reaped) => startup_trace.push("SSH reaped".to_owned()),
                    Some(_) => {}
                    None => return Err("fixture events closed".to_owned()),
                }
            }
        }
    }).await;
    if !matches!(startup, Ok(Ok(()))) {
        stop.send_replace(true);
        let _ = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, monitor).await;
        fixture.satellite_stop.send_replace(true);
        fixture.satellite_task.await.unwrap().unwrap();
        panic!("health bootstrap failed before pressure: {startup:?}; events={startup_trace:?}");
    }
    // Same admission used by four mailbox operations; wait for any preceding
    // successful health call to release its slot, then occupy all four.
    let admitted = channel.bound.permits.clone().acquire_many_owned(4).await.unwrap();
    let lease = channel.current_lease().unwrap();
    enabled.store(true, Ordering::SeqCst);
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, received.recv()).await.unwrap().unwrap();
    let preserved = lease.with_incarnation(|| Ok(())).is_ok();
    stop.send_replace(true);
    drop(admitted);
    let outcome = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, monitor).await.unwrap().unwrap();
    fixture.satellite_stop.send_replace(true);
    fixture.satellite_task.await.unwrap().unwrap();
    assert!(preserved, "local health admission429 retired a healthy central link");
    assert!(outcome.is_ok(), "monitor must stop cleanly after congestion");
}

#[tokio::test]
async fn satellite_monitor_keeps_its_link_when_four_mailbox_slots_are_admitted() {
    let channel = federation_link::channel(FederationRole::Satellite, 'a');
    let lease = channel.activate(Uuid::new_v4().to_string());
    let admitted = channel.bound.permits.clone().acquire_many_owned(4).await.unwrap();
    let (observed, mut received) = tokio::sync::mpsc::unbounded_channel();
    let client = FederationRpcClient::new(48001, channel.clone(),
        probe_observer(channel.clone(), Arc::new(AtomicBool::new(true)), observed)).unwrap();
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let monitor = tokio::spawn(async move {
        let server = tokio::spawn(std::future::pending::<std::io::Result<()>>());
        let io = FederationRuntimeIo { probe_interval: Duration::from_millis(20), ..Default::default() };
        let result = federation_satellite_monitor(&client, &mut stopped,
            &mut FederationBackoff::default(), &server, &io).await;
        server.abort();
        let _ = server.await;
        result
    });
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, received.recv()).await.unwrap().unwrap();
    let preserved = lease.with_incarnation(|| Ok(())).is_ok();
    stop.send_replace(true);
    drop(admitted);
    monitor.await.unwrap().unwrap();
    assert!(preserved, "local health admission429 retired a healthy satellite link");
}

#[test]
fn relayed_central503_keeps_unknown_append_outcome_and_original_key_guidance() {
    let response = federation_mailbox_rpc_error(StatusCode::SERVICE_UNAVAILABLE);
    let raw: anyhow::Error = TermalDelegationApiError {
        status: response.status, message: response.message,
    }.into();
    assert!(termal_delegation_outcome_is_unknown(&raw), "{raw:#}");
    assert!(!error_is_safe_replay_service_unavailable(&raw));
    let error = mailbox_send_bridge_error(raw);
    assert!(error.downcast_ref::<TermalDelegationUnknownOutcome>().is_some());
    assert!(error.to_string().contains("same idempotencyKey"));
    // An ordinary explicit no-commit admission refusal stays known.
    let refused: anyhow::Error = TermalDelegationApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        message: "mailbox writer busy; no mailbox write was committed".to_owned(),
    }.into();
    assert!(!termal_delegation_outcome_is_unknown(&refused));
}

#[tokio::test]
async fn unusable_successful_central_reply_is_an_unknown_mailbox_outcome() {
    let channel = federation_link::channel(FederationRole::Satellite, 'a');
    let lease = channel.activate(Uuid::new_v4().to_string());
    let central = channel.snapshot.central.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let server = tokio::spawn(async move {
        let router = Router::new().route("/federation/v1/mailbox",
            post(|| async { (StatusCode::OK, "unusable receipt") }));
        axum::serve(listener, router).with_graceful_shutdown(async move {
            federation_wait_stop(&mut stopped).await;
        }).await.unwrap();
    });
    let client = FederationRpcClient::new(port, channel, federation_link::allowed()).unwrap();
    let error = client.request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(lease,
        FederationMailboxRequest {
            protocol_version: 1, schema_version: "1".to_owned(), central,
            actor: None, operation: FederationMailboxOperation::Protocol,
        })).await.unwrap_err();
    stop.send_replace(true);
    server.await.unwrap();
    let raw: anyhow::Error = TermalDelegationApiError {
        status: error.status, message: error.message,
    }.into();
    assert!(termal_delegation_outcome_is_unknown(&raw), "{raw:#}");
}
