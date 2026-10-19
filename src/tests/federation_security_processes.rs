// Owns exact child-process lifetimes for control and adversarial forward tests.
// Does not launch SSH, touch live stores or put keys on diagnostic pipes. Split from federation_loopback.rs.

use super::*;
use fixture::PeerInfo;

const PROCESS_ROLE: &str = "TERMAL_TEST_SECURITY_PROCESS";

struct Process {
    child: Arc<SharedChild>,
    input: Option<std::process::ChildStdin>,
    lines: mpsc::Receiver<String>,
    readers: Vec<std::thread::JoinHandle<()>>,
    exit: mpsc::Receiver<std::io::Result<std::process::ExitStatus>>,
    waiter: Option<std::thread::JoinHandle<()>>,
    key_logged: Arc<AtomicBool>,
}

impl Process {
    fn spawn(
        role: &str,
        port: u16,
        target: u16,
        observed: Option<tokio::sync::mpsc::UnboundedSender<bool>>,
    ) -> Self {
        let module = module_path!().split_once("::").unwrap().1;
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                &format!("{module}::federation_security_process_child"),
                "--nocapture",
            ])
            .env(PROCESS_ROLE, role)
            .env("TERMAL_TEST_SECURITY_PORT", port.to_string())
            .env("TERMAL_TEST_SECURITY_TARGET", target.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = Arc::new(host_command::spawn_shared(&mut command).unwrap());
        let input = child.take_stdin().unwrap();
        let stdout = child.take_stdout().unwrap();
        let stderr = child.take_stderr().unwrap();
        let (line_tx, lines) = mpsc::channel();
        let key_logged = Arc::new(AtomicBool::new(false));
        let stdout_key = key_logged.clone();
        let stderr_key = key_logged.clone();
        let stdout_reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(|line| line.ok()) {
                if line.contains(&"a".repeat(64)) {
                    stdout_key.store(true, Ordering::SeqCst);
                }
                if let Some((_, value)) = line.split_once("SEC_SEEN ") {
                    if let Some(observed) = &observed {
                        let _ = observed.send(value == "key");
                    }
                }
                if line_tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr_reader = std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(|line| line.ok()) {
                if line.contains(&"a".repeat(64)) {
                    stderr_key.store(true, Ordering::SeqCst);
                }
            }
        });
        let waiting = child.clone();
        let (send_exit, exit) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let _ = send_exit.send(waiting.wait());
        });
        Self {
            child,
            input: Some(input),
            lines,
            readers: vec![stdout_reader, stderr_reader],
            exit,
            waiter: Some(waiter),
            key_logged,
        }
    }

    fn receive(&self, prefix: &str) -> String {
        let until = Instant::now() + phase_sync::DEADLOCK_GUARD;
        loop {
            let line = self
                .lines
                .recv_timeout(until.saturating_duration_since(Instant::now()))
                .expect("security child phase deadline");
            if let Some((_, value)) = line.split_once(prefix) {
                return value.to_owned();
            }
        }
    }

    fn finish(mut self) -> bool {
        writeln!(self.input.as_mut().unwrap(), "stop").unwrap();
        self.input.take();
        assert!(
            self.exit
                .recv_timeout(phase_sync::DEADLOCK_GUARD)
                .unwrap()
                .unwrap()
                .success()
        );
        self.waiter.take().unwrap().join().unwrap();
        for reader in self.readers.drain(..) {
            reader.join().unwrap();
        }
        !self.key_logged.load(Ordering::SeqCst)
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.input.take();
        if self.waiter.is_some() {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.waiter.take().unwrap().join().unwrap();
            for reader in self.readers.drain(..) {
                reader.join().unwrap();
            }
        }
    }
}

struct SpyChild(Option<Process>);

impl FederationSshChild for SpyChild {
    fn child_exited(&mut self) -> Result<bool, ApiError> {
        Ok(false)
    }

    fn shutdown(mut self: Box<Self>) -> Result<(), ApiError> {
        assert!(
            self.0.take().unwrap().finish(),
            "diagnostic pipes must not carry the pair key"
        );
        Ok(())
    }
}

async fn adversarial_process(relay: bool) {
    let (observed, mut observations) = tokio::sync::mpsc::unbounded_channel();
    let started = Arc::new(AtomicBool::new(false));
    let child_started = started.clone();
    let prepared = Arc::new(Mutex::new(None::<(u16, Process)>));
    let prepare_process = prepared.clone();
    let prepare = Arc::new(move |snapshot: &FederationPeerSnapshot| {
        let target = if relay {
            snapshot.endpoint.satellite_federation_port
        } else {
            0
        };
        // The child owns its OS-selected socket before publishing the address;
        // no probe/drop/rebind interval or cross-process counter namespace.
        let process = Process::spawn("proxy", 0, target, Some(observed.clone()));
        let port = process.receive("SEC_READY ").parse::<u16>().unwrap();
        *prepare_process.lock().unwrap() = Some((port, process));
        Ok(port)
    });
    let pair = Pair::with_starter(
        Arc::new(move |_, port, _| {
            let (owned_port, process) = prepared.lock().unwrap().take().unwrap();
            assert_eq!(owned_port, port);
            child_started.store(true, Ordering::SeqCst);
            Ok(Box::new(SpyChild(Some(process))))
        }),
        prepare,
    )
    .await;
    let leaked = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        loop {
            let changed = pair.relay.changed.notified();
            if pair.relay.unproven.load(Ordering::SeqCst) {
                break false;
            }
            tokio::select! {
                Some(seen) = observations.recv() => break seen,
                _ = changed => {},
            }
        }
    })
    .await;
    pair.finish().await;
    assert!(
        started.load(Ordering::SeqCst),
        "the unrelated process must occupy the selected forward"
    );
    assert!(
        !leaked.expect("a packet or an explicit owned-stream refusal must be observed"),
        "a squatter or transparent relay process must never receive the pair key"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_security_two_process_squatter_never_receives_pair_key() {
    adversarial_process(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_security_two_process_relay_never_receives_pair_key() {
    adversarial_process(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_security_two_process_control_and_signed_rpc() {
    let (process, info) = tokio::task::spawn_blocking(|| {
        let process = Process::spawn("satellite", 0, 0, None);
        let info: PeerInfo = serde_json::from_str(&process.receive("SEC_READY ")).unwrap();
        (process, info)
    })
    .await
    .unwrap();
    let result = Control::open(&info, 'a', |_| {}, None).await;
    let proof = match result {
        Ok(control) => {
            let mut headers = axum::http::HeaderMap::new();
            let authority = FederationRequestAuthority {
                caller: FederationHostIdentity {
                    host_id: info.central_id.clone(),
                    host_generation: info.epoch,
                },
                central: FederationHostIdentity {
                    host_id: info.central_id.clone(),
                    host_generation: info.epoch,
                },
                enrollment_generation: info.enrollment,
            };
            headers.insert(
                FEDERATION_AUTHORITY_HEADER,
                HeaderValue::from_str(&serde_json::to_string(&authority).unwrap()).unwrap(),
            );
            headers.insert(
                FEDERATION_INCARNATION_HEADER,
                HeaderValue::from_str(&control.session.incarnation).unwrap(),
            );
            let (headers, representation) = sign_request(
                &control.session,
                1,
                "GET",
                "/federation/v1/health",
                headers,
                &[],
            );
            let response = wire(info.port, "GET", "/federation/v1/health", headers, &[]).await;
            response.status == StatusCode::OK
                && response.verified(&control.session, 1, &representation)
        }
        Err(_) => false,
    };
    let no_key_logged = tokio::task::spawn_blocking(move || process.finish())
        .await
        .unwrap();
    assert!(
        proof,
        "actual second process must complete attestation and sign the RPC reply"
    );
    assert!(no_key_logged);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_security_process_child() {
    let Ok(role) = std::env::var(PROCESS_ROLE) else {
        return;
    };
    if role == "satellite" {
        let satellite = Satellite::new().await;
        println!(
            "SEC_READY {}",
            serde_json::to_string(&satellite.info()).unwrap()
        );
        tokio::task::spawn_blocking(|| {
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
        })
        .await
        .unwrap();
        satellite.finish().await;
        return;
    }
    let port: u16 = std::env::var("TERMAL_TEST_SECURITY_PORT")
        .unwrap()
        .parse()
        .unwrap();
    let target: u16 = std::env::var("TERMAL_TEST_SECURITY_TARGET")
        .unwrap()
        .parse()
        .unwrap();
    let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    let owned_port = listener.local_addr().unwrap().port();
    let router = Router::new().fallback(move |request: axum::extract::Request| async move {
        let (parts, body) = request.into_parts();
        let body = axum::body::to_bytes(body, 4096).await.unwrap();
        let key = "a".repeat(64);
        let seen = parts.headers.values().any(|value| {
            value
                .as_bytes()
                .windows(64)
                .any(|bytes| bytes == key.as_bytes())
        }) || body.windows(64).any(|bytes| bytes == key.as_bytes());
        println!("SEC_SEEN {}", if seen { "key" } else { "no-key" });
        if target == 0 {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        let response = reqwest::Client::builder()
            .no_proxy()
            .no_gzip()
            .build()
            .unwrap()
            .request(
                parts.method,
                format!("http://127.0.0.1:{target}{}", parts.uri),
            )
            .headers(parts.headers)
            .body(body)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        (status, headers, response.bytes().await.unwrap()).into_response()
    });
    println!("SEC_READY {owned_port}");
    let stop = async {
        tokio::task::spawn_blocking(|| {
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
        })
        .await
        .unwrap();
    };
    axum::serve(listener, router)
        .with_graceful_shutdown(stop)
        .await
        .unwrap();
}
