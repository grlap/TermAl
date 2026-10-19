// Owns one disposable satellite process for O1's two-process fake-link proof.
// Does not launch SSH or user agents. New fixture follows federation_loopback.rs
// but carries real M1 host identities and the production O1 route context.
use super::*;

pub(super) struct Satellite {
    child: Arc<SharedChild>,
    stdin: Option<std::process::ChildStdin>,
    lines: mpsc::Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
    waiter: Option<std::thread::JoinHandle<()>>,
    exit: mpsc::Receiver<std::io::Result<std::process::ExitStatus>>,
}

#[derive(Deserialize, Serialize)]
pub(super) struct Ready {
    pub(super) identity: FederationHostIdentity,
    pub(super) port: u16,
}

#[derive(Deserialize, Serialize)]
pub(super) struct Setup {
    pub(super) central: FederationHostIdentity,
    pub(super) central_port: u16,
}

impl Satellite {
    pub(super) fn start() -> (Self, Ready) {
        let test = format!(
            "{}::federation_onboarding_satellite_child",
            module_path!().split_once("::").unwrap().1
        );
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", &test, "--nocapture"])
            .env("TERMAL_TEST_O1_CHILD", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let child = Arc::new(host_command::spawn_shared(&mut command).unwrap());
        let stdin = child.take_stdin().unwrap();
        let stdout = child.take_stdout().unwrap();
        let (line_tx, lines) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if line_tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let (exit_tx, exit) = mpsc::channel();
        let waiting = child.clone();
        let waiter = std::thread::spawn(move || {
            let _ = exit_tx.send(waiting.wait());
        });
        let satellite = Self {
            child,
            stdin: Some(stdin),
            lines,
            reader: Some(reader),
            waiter: Some(waiter),
            exit,
        };
        let ready = serde_json::from_str(&satellite.receive("O1_READY ")).unwrap();
        (satellite, ready)
    }

    fn receive(&self, prefix: &str) -> String {
        let deadline = Instant::now() + phase_sync::DEADLOCK_GUARD;
        loop {
            let line = self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            if let Some((_, value)) = line.split_once(prefix) {
                return value.to_owned();
            }
        }
    }

    pub(super) fn configure(&mut self, setup: Setup) {
        writeln!(
            self.stdin.as_mut().unwrap(),
            "{}",
            serde_json::to_string(&setup).unwrap()
        )
        .unwrap();
        self.stdin.as_mut().unwrap().flush().unwrap();
        assert_eq!(self.receive("O1_RESULT "), "configured");
    }

    pub(super) fn finish(mut self) {
        writeln!(self.stdin.as_mut().unwrap(), "stop").unwrap();
        self.stdin.take();
        assert_eq!(self.receive("O1_RESULT "), "stopped");
        assert!(
            self.exit
                .recv_timeout(phase_sync::DEADLOCK_GUARD)
                .unwrap()
                .unwrap()
                .success()
        );
        self.waiter.take().unwrap().join().unwrap();
        self.reader.take().unwrap().join().unwrap();
    }
}

impl Drop for Satellite {
    fn drop(&mut self) {
        self.stdin.take();
        if self.waiter.is_some() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(waiter) = self.waiter.take() {
            let _ = waiter.join();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_satellite_child() {
    if std::env::var("TERMAL_TEST_O1_CHILD").as_deref() != Ok("1") {
        return;
    }
    let app = federation_registry::domain_state();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let identity = app.inner.lock().unwrap().federation.local.clone().unwrap();
    println!(
        "O1_READY {}",
        serde_json::to_string(&Ready { identity, port }).unwrap()
    );
    let setup = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
        serde_json::from_str::<Setup>(&line).unwrap()
    })
    .await
    .unwrap();
    app.configure_federation(FederationSettings {
        enabled: true,
        role: FederationRole::Satellite,
        central: Some(setup.central.clone()),
    })
    .unwrap();
    let mut record = federation_registry::peer(setup.central_port);
    record.identity = setup.central.clone();
    record.profile = federation_onboarding_fixture::profile();
    record.platform = std::env::consts::OS.to_owned();
    record.arch = std::env::consts::ARCH.to_owned();
    record.endpoint.satellite_federation_port = port;
    record.endpoint.satellite_reverse_forward_port = setup.central_port;
    app.enroll_federation_peer(record).unwrap();
    let channel = FederationChannel::for_app(
        app.federation_peer_snapshot(&setup.central.host_id)
            .unwrap(),
        FederationPairCapability::new("a".repeat(64)).unwrap(),
        app.clone(),
    );
    let checked = channel.clone();
    let check: FederationAuthorityCheck = Arc::new(move || checked.current_authority().map(|_| ()));
    let rpc = FederationRpcClient::new(setup.central_port, channel.clone(), check.clone()).unwrap();
    let router = federation_router(channel.clone(), check).layer(axum::Extension(
        FederationOnboardingHost {
            app,
            rpc,
            io: federation_onboarding_fixture::io(),
        },
    ));
    let (stop, rx) = tokio::sync::watch::channel(false);
    let server_channel = channel.clone();
    let server = tokio::spawn(async move {
        serve_federation_control_listener(listener, router, rx, server_channel)
            .await
            .unwrap();
    });
    println!("O1_RESULT configured");
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
    })
    .await
    .unwrap();
    channel.invalidate();
    stop.send_replace(true);
    server.await.unwrap();
    println!("O1_RESULT stopped");
}
