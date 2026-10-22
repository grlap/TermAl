// Owns the loopback fixture and fake owned SSH for production worker tests.
// Does not access OS credentials or launch SSH. Split from federation_runtime.rs.

use super::federation_link::allowed;
use super::*;
use std::sync::atomic::AtomicUsize;

enum Event {
    Credentials,
    Started,
    Reaped,
    Response(String, StatusCode),
    Attempt(StatusCode),
}

struct FakeSsh {
    control: Option<FederationControlIo>,
    stop: tokio::sync::watch::Sender<bool>,
    exited: mpsc::Receiver<()>,
    events: tokio::sync::mpsc::UnboundedSender<Event>,
    cleanup_unknown: Arc<AtomicBool>,
}

impl FederationSshChild for FakeSsh {
    fn take_control_stream(&mut self) -> Result<FederationControlIo, ApiError> {
        self.control.take().ok_or_else(|| {
            ApiError::from_status(StatusCode::FORBIDDEN, "unproven federation transport")
        })
    }
    fn child_exited(&mut self) -> Result<bool, ApiError> {
        Ok(false)
    }

    fn shutdown(self: Box<Self>) -> Result<(), ApiError> {
        self.stop.send_replace(true);
        self.exited
            .recv_timeout(phase_sync::DEADLOCK_GUARD)
            .unwrap();
        let _ = self.events.send(Event::Reaped);
        if self.cleanup_unknown.load(Ordering::SeqCst) {
            return Err(ApiError::internal("fake cleanup unresolved")
                .with_kind(ApiErrorKind::FederationSshCleanupUnknown));
        }
        Ok(())
    }
}

impl Drop for FakeSsh {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

struct Fixture {
    app: AppState,
    id: String,
    satellite: FederationChannel,
    io: FederationRuntimeIo,
    events: tokio::sync::mpsc::UnboundedReceiver<Event>,
    hold: Arc<AtomicBool>,
    fingerprint: Arc<AtomicUsize>,
    fail_health: Arc<AtomicBool>,
    cleanup_unknown: Arc<AtomicBool>,
    reserved_forward_listener: Arc<Mutex<Option<std::net::TcpListener>>>,
    satellite_stop: tokio::sync::watch::Sender<bool>,
    satellite_task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Fixture {
    async fn new() -> Self {
        let app = federation_registry::central();
        let central_port = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let satellite_port = listener.local_addr().unwrap().port();
        let mut record = federation_registry::peer(central_port.local_addr().unwrap().port());
        record.endpoint.satellite_federation_port = satellite_port;
        let id = record.identity.host_id.clone();
        app.enroll_federation_peer(record).unwrap();
        let basis = app.federation_peer_snapshot(&id).unwrap();
        let mut reverse = basis.clone();
        reverse.local = basis.peer.clone();
        reverse.peer = basis.local.clone();
        reverse.local_role = FederationRole::Satellite;
        let satellite = FederationChannel::new(
            reverse,
            FederationPairCapability::new("a".repeat(64)).unwrap(),
        );
        let router = federation_router(satellite.clone(), allowed());
        let (satellite_stop, stop_rx) = tokio::sync::watch::channel(false);
        let satellite_task = tokio::spawn(serve_federation_control_listener(
            listener,
            router,
            stop_rx,
            satellite.clone(),
        ));
        let (events_tx, events) = tokio::sync::mpsc::unbounded_channel();
        let attempt_events = events_tx.clone();
        let read_events = events_tx.clone();
        let hold = Arc::new(AtomicBool::new(false));
        let fingerprint = Arc::new(AtomicUsize::new(1));
        let read_fingerprint = fingerprint.clone();
        let fail_health = Arc::new(AtomicBool::new(false));
        let forward_failure = fail_health.clone();
        let cleanup_unknown = Arc::new(AtomicBool::new(false));
        let ssh_cleanup = cleanup_unknown.clone();
        let read_hold = hold.clone();
        let read_app = app.clone();
        let credentials = Arc::new(move |_: &FederationPeerSnapshot| {
            if read_hold.swap(false, Ordering::SeqCst) {
                read_app.inner.lock().unwrap().federation_durable = false;
            }
            let _ = read_events.send(Event::Credentials);
            Ok(FederationTransportCredentials {
                capability: FederationPairCapability::new("a".repeat(64)).unwrap(),
                known_hosts: Some(PathBuf::from("unused-fake-pin")),
                material_fingerprint: [read_fingerprint.load(Ordering::SeqCst) as u8; 32],
            })
        });
        let handle = tokio::runtime::Handle::current();
        let forward_listener = Arc::new(Mutex::new(Some(
            std::net::TcpListener::bind("127.0.0.1:0").unwrap(),
        )));
        let reserved_forward = forward_listener.clone();
        let ssh: FederationSshStarter = Arc::new(move |_, port, _| {
            let _entered = handle.enter();
            let socket = reserved_forward
                .lock()
                .unwrap()
                .take()
                .ok_or_else(federation_transport_unavailable)?;
            assert_eq!(
                socket.local_addr().unwrap().port(),
                port,
                "handoff retains the selected socket"
            );
            socket
                .set_nonblocking(true)
                .map_err(|_| federation_transport_unavailable())?;
            let listener =
                TcpListener::from_std(socket).map_err(|_| federation_transport_unavailable())?;
            let control = std::net::TcpStream::connect(("127.0.0.1", satellite_port))
                .map_err(|_| federation_transport_unavailable())?;
            control.set_nonblocking(true).unwrap();
            let control = tokio::net::TcpStream::from_std(control).unwrap();
            let (stop, mut stop_rx) = tokio::sync::watch::channel(false);
            let (exit_tx, exited) = mpsc::channel();
            let forward_events = events_tx.clone();
            let forward_failure = forward_failure.clone();
            let router = Router::new().fallback(move |request: axum::extract::Request| {
                let events = forward_events.clone();
                let forward_failure = forward_failure.clone();
                async move {
                    let (parts, body) = request.into_parts();
                    let path = parts.uri.path().to_owned();
                    if path.ends_with("health") && forward_failure.swap(false, Ordering::SeqCst) {
                        return StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                    let bytes = axum::body::to_bytes(body, 4096).await.unwrap();
                    let response = reqwest::Client::builder()
                        .no_proxy()
                        .no_gzip()
                        .build()
                        .unwrap()
                        .request(
                            parts.method,
                            format!("http://127.0.0.1:{satellite_port}{path}"),
                        )
                        .headers(parts.headers)
                        .body(bytes)
                        .send()
                        .await
                        .unwrap();
                    let status = response.status();
                    let headers = response.headers().clone();
                    let body = response.bytes().await.unwrap();
                    let _ = events.send(Event::Response(path, status));
                    (status, headers, body).into_response()
                }
            });
            handle.spawn(async move {
                let _ = axum::serve(listener, router)
                    .with_graceful_shutdown(async move { federation_wait_stop(&mut stop_rx).await })
                    .await;
                let _ = exit_tx.send(());
            });
            let _ = events_tx.send(Event::Started);
            Ok(Box::new(FakeSsh {
                control: Some(Box::new(control)),
                stop,
                exited,
                events: events_tx.clone(),
                cleanup_unknown: ssh_cleanup.clone(),
            }))
        });
        drop(central_port);
        Self {
            app,
            id,
            satellite,
            hold,
            fingerprint,
            fail_health,
            cleanup_unknown,
            events,
            reserved_forward_listener: forward_listener.clone(),
            satellite_stop,
            satellite_task,
            io: FederationRuntimeIo {
                prepare_forward: None,
                forward_listener: Some(forward_listener),
                credentials,
                ssh,
                reconcile_interval: Duration::from_millis(20),
                probe_interval: Duration::from_millis(20),
                operation_timeout: Duration::from_secs(5),
                credential_reporting_deadline: Duration::from_secs(5),
                control_interval: Duration::from_secs(15),
                control_timeout: Duration::from_secs(45),
                link_startup_timeout: FEDERATION_LINK_STARTUP_TIMEOUT,
                attest_timeout: FEDERATION_ATTEST_TIMEOUT,
                control_clock: FederationControlClock::default(),
                attempt_finished: Arc::new(move |result| {
                    if let Err(error) = result {
                        let _ = attempt_events.send(Event::Attempt(error.status));
                    }
                }),
                reconciled: Arc::new(|_| {}),
                connection_published: Arc::new(|| {}),
                control_observed: Arc::new(|_, _| {}),
                credential_observed: Arc::new(|_| {}),
            },
        }
    }

    fn reserve_forward_listener(&self) -> u16 {
        let mut reserved = self.reserved_forward_listener.lock().unwrap();
        if reserved.is_none() {
            *reserved = Some(std::net::TcpListener::bind("127.0.0.1:0").unwrap());
        }
        reserved.as_ref().unwrap().local_addr().unwrap().port()
    }

    async fn event(&mut self, matches: impl Fn(&Event) -> bool) -> bool {
        tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
            while let Some(event) = self.events.recv().await {
                if matches(&event) {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false)
    }

    async fn finish(self, runtime: FederationRuntime) {
        runtime.shutdown().await;
        self.satellite_stop.send_replace(true);
        self.satellite_task.await.unwrap().unwrap();
    }
}
