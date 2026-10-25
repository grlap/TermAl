// Disposable second-process host and real domain HTTP used by entrypoint
// regressions. Loopback replaces SSH; neither this fixture nor its controls
// implement activation, placement or binding routes under test.
use super::federation_entrypoints_control::Control;
pub(super) use super::federation_entrypoints_writer::start_writer;
use super::*;

pub(super) const CHILD_ENV: &str = "TERMAL_TEST_ENTRYPOINT_SATELLITE";

// A process restart closes its connections. Use fresh fake-link connections
// so a rebuilt channel does not retain an old router's keepalive.
pub(super) async fn close_connections(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        axum::http::header::CONNECTION,
        HeaderValue::from_static("close"),
    );
    response
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Native {
    pub host: FederationHostIdentity,
    pub session: String,
    pub creation: String,
    pub port: u16,
}

#[derive(Deserialize, Serialize)]
pub(super) struct Setup {
    pub central: FederationHostIdentity,
    pub central_port: u16,
    pub central_state_path: PathBuf,
}

pub(super) struct Satellite {
    child: Arc<SharedChild>,
    input: Option<std::process::ChildStdin>,
    lines: mpsc::Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
    waiter: Option<std::thread::JoinHandle<()>>,
    exit: mpsc::Receiver<std::io::Result<std::process::ExitStatus>>,
}

impl Satellite {
    pub fn start() -> (Self, Native) {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "tests::federation_entrypoints_child::satellite_child",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let child = Arc::new(host_command::spawn_shared(&mut command).unwrap());
        let input = child.take_stdin().unwrap();
        let output = child.take_stdout().unwrap();
        let (tx, lines) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else {
                    break;
                };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let waiting = child.clone();
        let (exit_tx, exit) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let _ = exit_tx.send(waiting.wait());
        });
        let process = Self {
            child,
            input: Some(input),
            lines,
            reader: Some(reader),
            waiter: Some(waiter),
            exit,
        };
        let native = serde_json::from_str(&process.receive("ENTRY_NATIVE ")).unwrap();
        (process, native)
    }

    fn receive(&self, prefix: &str) -> String {
        let deadline = Instant::now() + phase_sync::DEADLOCK_GUARD;
        loop {
            let line = self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("entrypoint satellite must report its phase");
            if let Some((_, body)) = line.split_once(prefix) {
                return body.to_owned();
            }
        }
    }

    pub fn configure(&mut self, setup: Setup) {
        writeln!(
            self.input.as_mut().unwrap(),
            "{}",
            serde_json::to_string(&setup).unwrap()
        )
        .unwrap();
        self.input.as_mut().unwrap().flush().unwrap();
        assert_eq!(self.receive("ENTRY_CONFIGURED "), "true");
    }

    pub fn command(&mut self, command: Value) -> Value {
        writeln!(self.input.as_mut().unwrap(), "{command}").unwrap();
        self.input.as_mut().unwrap().flush().unwrap();
        serde_json::from_str(&self.receive("ENTRY_RESULT ")).unwrap()
    }
}

impl Drop for Satellite {
    fn drop(&mut self) {
        if let Some(input) = self.input.as_mut() {
            let _ = writeln!(input, "{{\"command\":\"stop\"}}");
            let _ = input.flush();
        }
        self.input.take();
        if self.exit.recv_timeout(phase_sync::DEADLOCK_GUARD).is_err() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(waiter) = self.waiter.take() {
            waiter.join().unwrap();
        }
        if let Some(reader) = self.reader.take() {
            reader.join().unwrap();
        }
    }
}

pub(super) struct Pair {
    pub central: AppState,
    pub native: Native,
    pub child: Satellite,
    pub rpc: FederationRpcClient,
    pub channel: FederationChannel,
    router: Arc<Mutex<Router>>,
    control: Option<Control>,
    stop: tokio::sync::watch::Sender<bool>,
    server: tokio::task::JoinHandle<()>,
    _process_slot: tokio::sync::OwnedSemaphorePermit,
}

impl Pair {
    pub async fn new() -> Self {
        // Bound fixture subprocesses independently of the Rust test runner's
        // parallelism; this does not change any production RPC deadline.
        static PROCESS_SLOT: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
        let process_slot = PROCESS_SLOT
            .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
            .clone()
            .acquire_owned()
            .await
            .unwrap();
        let mut central = federation_mailboxes::central_store();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let central_port = listener.local_addr().unwrap().port();
        let (mut child, native) = tokio::task::spawn_blocking(Satellite::start).await.unwrap();
        let mut record = federation_registry::peer(central_port);
        record.identity = native.host.clone();
        record.platform = std::env::consts::OS.to_owned();
        record.arch = std::env::consts::ARCH.to_owned();
        record.profile = federation_onboarding_fixture::profile();
        record.capacity.max_sessions = 2;
        record.endpoint.satellite_federation_port = native.port;
        record.endpoint.satellite_reverse_forward_port = central_port;
        central.enroll_federation_peer(record).unwrap();
        // Enrollment is fixture setup, not the entrypoint under test. Commit
        // it through the real synchronous test fallback before starting the
        // queued writer, so setup does not race its five-second fence budget.
        // Every operation exercised by the test still uses the queued writer.
        start_writer(&mut central);
        let snapshot = central
            .federation_peer_snapshot(&native.host.host_id)
            .unwrap();
        let setup = Setup {
            central: snapshot.local.clone(),
            central_port,
            central_state_path: central.persistence_path.as_ref().clone(),
        };
        let mut channel = FederationChannel::for_app(
            snapshot,
            FederationPairCapability::new("a".repeat(64)).unwrap(),
            central.clone(),
        );
        // Keep control at the explicitly observed first-heartbeat phase;
        // application refusal tests do not race a wall-clock heartbeat.
        channel.control.clock = FederationControlClock::scripted();
        let checked = channel.clone();
        let check: FederationAuthorityCheck =
            Arc::new(move || checked.current_authority().map(|_| ()));
        let rpc = FederationRpcClient::new(native.port, channel.clone(), check.clone()).unwrap();
        let router = federation_router(channel.clone(), check)
            .layer(axum::Extension(central.clone()))
            .layer(axum::Extension(FederationOnboardingHost {
                app: central.clone(),
                rpc: rpc.clone(),
                io: federation_onboarding_fixture::io(),
            }))
            .layer(axum::middleware::from_fn(close_connections));
        // Keep the endpoint stable across state/channel reloads. Immediate
        // same-port rebinding after closing TCP connections is not portable.
        // Each request still runs the actual production federation Router.
        let router = Arc::new(Mutex::new(router));
        let dispatch = router.clone();
        let ingress = Router::new().fallback(move |request: axum::extract::Request| {
            let current = dispatch.lock().unwrap().clone();
            async move { current.oneshot(request).await.unwrap() }
        });
        let (stop, mut rx) = tokio::sync::watch::channel(false);
        let server = tokio::spawn(async move {
            axum::serve(listener, ingress)
                .with_graceful_shutdown(async move { federation_wait_stop(&mut rx).await })
                .await
                .unwrap();
        });
        child = tokio::task::spawn_blocking(move || {
            child.configure(setup);
            child
        })
        .await
        .unwrap();
        let control = Control::connect(&channel, native.port).await;
        central.federation_peer_handles.lock().unwrap().insert(
            native.host.host_id.clone(),
            Arc::new(FederationPeerHandle {
                channel: channel.clone(),
                rpc: rpc.clone(),
            }),
        );
        let pair = Self {
            central,
            native,
            child,
            rpc,
            channel,
            router,
            control: Some(control),
            stop,
            server,
            _process_slot: process_slot,
        };
        pair.rpc.open_link().await.unwrap();
        let (status, report) = pair.operator("verify", json!({})).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "real O1 fixture must verify: {report}"
        );
        assert!(
            serde_json::from_value::<FederationOnboardingReport>(report)
                .unwrap()
                .termal_complete()
        );
        pair
    }

    pub async fn operator(&self, suffix: &str, body: Value) -> (StatusCode, Value) {
        operator(&self.central, &self.native.host.host_id, suffix, body).await
    }

    pub async fn activate(&self) {
        let (status, body) = self.operator("activate", json!({})).await;
        assert_eq!(status, StatusCode::OK, "activation route: {body}");
    }

    pub async fn reserve(&self, key: &str) -> (StatusCode, Value) {
        self.operator(
            "placements",
            json!({"nativeSessionId":self.native.session,"admissionKey":key}),
        )
        .await
    }

    pub async fn command(&mut self, command: Value) -> Value {
        tokio::task::block_in_place(|| self.child.command(command))
    }

    pub async fn retire_central_channel(&mut self) {
        if let Some(control) = self.control.take() {
            control.finish().await;
        }
        self.channel.invalidate();
        self.central
            .federation_peer_handles
            .lock()
            .unwrap()
            .remove(&self.native.host.host_id);
    }

    pub async fn replace_central_channel(&mut self, reload: bool) {
        self.retire_central_channel().await;
        if reload {
            self.central.shutdown_persist_blocking();
            let restored = load_state(self.central.persistence_path.as_path())
                .unwrap()
                .unwrap();
            *self.central.inner.lock().unwrap() = restored;
            start_writer(&mut self.central);
        }
        let mut channel = FederationChannel::for_app(
            self.peer(),
            FederationPairCapability::new("a".repeat(64)).unwrap(),
            self.central.clone(),
        );
        channel.control.clock = FederationControlClock::scripted();
        let checked = channel.clone();
        let check: FederationAuthorityCheck =
            Arc::new(move || checked.current_authority().map(|_| ()));
        let rpc =
            FederationRpcClient::new(self.native.port, channel.clone(), check.clone()).unwrap();
        let router = federation_router(channel.clone(), check)
            .layer(axum::Extension(self.central.clone()))
            .layer(axum::Extension(FederationOnboardingHost {
                app: self.central.clone(),
                rpc: rpc.clone(),
                io: federation_onboarding_fixture::io(),
            }))
            .layer(axum::middleware::from_fn(close_connections));
        *self.router.lock().unwrap() = router;
        self.channel = channel.clone();
        self.rpc = rpc.clone();
        self.central.federation_peer_handles.lock().unwrap().insert(
            self.native.host.host_id.clone(),
            Arc::new(FederationPeerHandle { channel, rpc }),
        );
        self.control = Some(Control::connect(&self.channel, self.native.port).await);
        self.rpc.open_link().await.unwrap();
    }

    // Fixture setup for domain-route tests only. Activation tests always use
    // the new operator route, while INSTALL tests must reach their own seam.
    pub fn fixture_active(&self) {
        self.central
            .transition_federation_peer(&self.peer(), FederationMembershipState::Active)
            .unwrap();
    }

    pub fn fixture_membership(&self, membership: FederationMembershipState) {
        let id = self.native.host.host_id.clone();
        self.central
            .mutate_federation(|domain, _| {
                let peer = domain.peers.get_mut(&id).unwrap();
                peer.membership = membership;
                if membership == FederationMembershipState::Enrolled
                    || membership == FederationMembershipState::Revoked
                {
                    peer.verification = None;
                }
                Ok(())
            })
            .unwrap();
    }

    pub async fn domain(&self, body: Value, authenticated: bool) -> (StatusCode, Value) {
        if authenticated {
            let lease = self.channel.current_lease().unwrap();
            let command = serde_json::from_value(body).unwrap();
            return match self
                .rpc
                .request(FederationRpcRoute::Binding(lease, command))
                .await
            {
                Ok(body) => (StatusCode::OK, body),
                Err(error) => (error.status, json!({"error":error.message})),
            };
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let request = client
            .post(format!(
                "http://127.0.0.1:{}/federation/v1/mailbox/binding",
                self.native.port
            ))
            .json(&body);
        let response = request.send().await.unwrap();
        let status = response.status();
        let bytes = response.bytes().await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| json!({"nonJson":String::from_utf8_lossy(&bytes)})),
        )
    }

    pub fn peer(&self) -> FederationPeerSnapshot {
        self.central
            .federation_peer_snapshot(&self.native.host.host_id)
            .unwrap()
    }

    pub fn existing_binding(&self, key: &str) -> FederationMailboxBinding {
        let peer = self.peer();
        let placement = self
            .central
            .reserve_federation_placement(&peer, key.to_owned(), self.native.session.clone(), 0, 0)
            .unwrap();
        FederationMailboxBinding {
            central: peer.local,
            identity: placement.identity,
            placement_generation: placement.placement_generation,
        }
    }
}

impl Drop for Pair {
    fn drop(&mut self) {
        drop(self.control.take());
        self.central
            .federation_peer_handles
            .lock()
            .unwrap()
            .remove(&self.native.host.host_id);
        self.channel.invalidate();
        self.stop.send_replace(true);
        self.server.abort();
        self.central.shutdown_persist_blocking();
    }
}

pub(super) async fn operator(
    app: &AppState,
    peer: &str,
    suffix: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app_router(app.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/federation/peers/{peer}/{suffix}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({"nonJson":String::from_utf8_lossy(&bytes)})),
    )
}
