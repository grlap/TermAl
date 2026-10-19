// Owns a delayed fake SSH connect with a real production control handshake.
// Does not extend product deadlines or run SSH. Split from federation runtime tests.

use super::*;

struct DelayedChild {
    control: Option<FederationControlIo>,
    stop: tokio::sync::watch::Sender<bool>,
    ended: mpsc::Receiver<Result<(), ApiError>>,
}

impl FederationSshChild for DelayedChild {
    fn take_control_stream(&mut self) -> Result<FederationControlIo, ApiError> {
        self.control
            .take()
            .ok_or_else(federation_transport_unavailable)
    }
    fn child_exited(&mut self) -> Result<bool, ApiError> {
        Ok(false)
    }
    fn shutdown(self: Box<Self>) -> Result<(), ApiError> {
        self.stop.send_replace(true);
        self.ended.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap()
    }
}

impl Drop for DelayedChild {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_startup_can_reach_first_frame_after_five_seconds_before_attestation() {
    let mut fixture = Fixture::new().await;
    let snapshot = fixture.app.federation_peer_snapshot(&fixture.id).unwrap();
    let channel = FederationChannel::for_app(
        snapshot.clone(),
        FederationPairCapability::new("a".repeat(64)).unwrap(),
        fixture.app.clone(),
    );
    let checked = channel.clone();
    let port = fixture.reserve_forward_listener();
    let client = FederationRpcClient::new(
        port,
        channel,
        Arc::new(move || checked.current_authority().map(|_| ())),
    )
    .unwrap();
    let mut io = fixture.io.clone();
    let start = io.ssh.clone();
    let handle = tokio::runtime::Handle::current();
    let (connected, mut connection_events) = tokio::sync::mpsc::unbounded_channel();
    io.ssh = Arc::new(move |snapshot, port, pin| {
        let (central, mut bridge) = tokio::io::duplex(FEDERATION_FRAME_LIMIT);
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let (finished, ended) = mpsc::channel();
        let start = start.clone();
        let snapshot = snapshot.clone();
        let pin = pin.to_path_buf();
        let connected = connected.clone();
        handle.spawn(async move {
            // No satellite TCP connection exists during this startup phase.
            // Its own proof timer starts only after the delayed connection.
            tokio::select! {
                _ = federation_wait_stop(&mut stopped) => { let _=finished.send(Ok(())); return; },
                _ = tokio::time::sleep(Duration::from_secs(6)) => {},
            }
            let started = tokio::task::spawn_blocking(move || start(&snapshot, port, &pin))
                .await
                .unwrap();
            let mut child = match started {
                Ok(child) => child,
                Err(error) => {
                    let _ = finished.send(Err(error));
                    return;
                }
            };
            let mut control = child.take_control_stream().unwrap();
            let _ = connected.send(());
            tokio::select! {
                _ = federation_wait_stop(&mut stopped) => {},
                _ = tokio::io::copy_bidirectional(&mut bridge, &mut control) => {},
            }
            drop(control);
            let result = tokio::task::spawn_blocking(move || child.shutdown())
                .await
                .unwrap();
            let _ = finished.send(result);
        });
        Ok(Box::new(DelayedChild {
            control: Some(Box::new(central)),
            stop,
            ended,
        }))
    });
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let (done, mut completion) = tokio::sync::oneshot::channel();
    let monitor = tokio::spawn(async move {
        let server = tokio::spawn(std::future::pending::<std::io::Result<()>>());
        let result = federation_central_monitor(
            &snapshot,
            &client,
            PathBuf::from("unused-fake-pin"),
            port,
            &mut stopped,
            &mut FederationBackoff::default(),
            &server,
            &io,
            &mut FederationAttemptState::default(),
        )
        .await;
        server.abort();
        let _ = server.await;
        let _ = done.send(
            result
                .as_ref()
                .err()
                .map(|error| format!("{}: {}", error.status, error.message)),
        );
        result
    });
    let established = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        loop {
            tokio::select! {
                ended = &mut completion => return Err(format!("monitor ended before delayed startup: {ended:?}")),
                event = fixture.events.recv() => match event {
                    Some(Event::Response(path,status)) if path.ends_with("health") && status.is_success() => return Ok(()),
                    Some(_) => {},
                    None => return Err("fixture event stream closed".to_owned()),
                }
            }
        }
    }).await;
    stop.send_replace(true);
    let result = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, monitor)
        .await
        .unwrap()
        .unwrap();
    let actually_connected = connection_events.try_recv().is_ok();
    fixture.satellite_stop.send_replace(true);
    fixture.satellite_task.await.unwrap().unwrap();
    assert!(
        matches!(established, Ok(Ok(()))),
        "SSH startup before the first complete frame must retain its separate 15s budget: {established:?}"
    );
    assert!(
        actually_connected,
        "the delayed real control connection must be exercised"
    );
    assert!(
        result.is_ok(),
        "the authenticated delayed link must stop cleanly"
    );
}
