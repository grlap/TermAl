// Owns the two-process loopback proof of federation HTTP in both directions.
// Does not launch real SSH, use live enrollment or open a live store. New tests.

use super::federation_link::{allowed, channel};
use super::*;

const CHILD_ROLE: &str = "TERMAL_TEST_FEDERATION_SATELLITE";

struct SatelliteProcess {
    child: Arc<SharedChild>,
    input: Option<std::process::ChildStdin>,
    lines: mpsc::Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
    exit: mpsc::Receiver<std::io::Result<std::process::ExitStatus>>,
    waiter: Option<std::thread::JoinHandle<()>>,
}

impl SatelliteProcess {
    fn spawn() -> (Self, u16) {
        let module = module_path!().split_once("::").unwrap().1;
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                &format!("{module}::federation_satellite_child"),
                "--nocapture",
            ])
            .env(CHILD_ROLE, "satellite")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let child = Arc::new(host_command::spawn_shared(&mut command).unwrap());
        let input = child.take_stdin().unwrap();
        let stdout = child.take_stdout().unwrap();
        let (line_tx, lines) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if line_tx.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let waiting_child = child.clone();
        let (exit_tx, exit) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let _ = exit_tx.send(waiting_child.wait());
        });
        let process = Self {
            child,
            input: Some(input),
            lines,
            reader: Some(reader),
            exit,
            waiter: Some(waiter),
        };
        let port = process.receive("FDR_READY ").parse().unwrap();
        (process, port)
    }

    fn receive(&self, prefix: &str) -> String {
        let started = Instant::now();
        loop {
            let remaining = phase_sync::DEADLOCK_GUARD
                .checked_sub(started.elapsed())
                .expect("federation child phase deadline");
            let line = self
                .lines
                .recv_timeout(remaining)
                .expect("federation child must report phase");
            // libtest may prefix the first uncaptured line with the test name.
            if let Some((_, value)) = line.split_once(prefix) {
                return value.to_owned();
            }
        }
    }

    fn command(&mut self, command: &str) -> String {
        writeln!(self.input.as_mut().unwrap(), "{command}").unwrap();
        self.input.as_mut().unwrap().flush().unwrap();
        self.receive("FDR_RESULT ")
    }

    fn finish(mut self) {
        assert_eq!(self.command("exit"), "stopped");
        self.input.take();
        let status = self
            .exit
            .recv_timeout(phase_sync::DEADLOCK_GUARD)
            .expect("satellite child exit")
            .unwrap();
        assert!(status.success());
        self.join_threads();
    }

    fn join_threads(&mut self) {
        if let Some(waiter) = self.waiter.take() {
            waiter.join().unwrap();
        }
        if let Some(reader) = self.reader.take() {
            reader.join().unwrap();
        }
    }
}

impl Drop for SatelliteProcess {
    fn drop(&mut self) {
        self.input.take();
        if self.waiter.is_some() {
            // This owner retains the exact SharedChild object, never a PID.
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.join_threads();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_two_process_loopback_bidirectional_and_reconnect() {
    let central = channel(FederationRole::Central, 'a');
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let central_port = listener.local_addr().unwrap().port();
    let (server_stop, mut server_stop_rx) = tokio::sync::watch::channel(false);
    let router = federation_router(central.clone(), allowed());
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { federation_wait_stop(&mut server_stop_rx).await })
            .await
            .unwrap();
    });
    // The parent test binary is the central host; this exact libtest child is
    // the satellite host. Loopback sockets stand in for SSH's two forwards.
    let (mut satellite, satellite_port) = tokio::task::spawn_blocking(SatelliteProcess::spawn)
        .await
        .unwrap();
    let wire = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    for port in [central_port, satellite_port] {
        for path in ["/api/health", "/api/state", "/api/sessions"] {
            assert_eq!(
                wire.get(format!("http://127.0.0.1:{port}{path}"))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::NOT_FOUND
            );
        }
        assert_eq!(
            wire.get(format!("http://127.0.0.1:{port}/federation/v1/health"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            wire.get(format!("http://127.0.0.1:{port}/federation/v1/health"))
                .header(FEDERATION_CAPABILITY_HEADER, "b".repeat(64))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    let rpc = FederationRpcClient::new(satellite_port, central.clone(), allowed()).unwrap();
    let old = rpc.open_link().await.unwrap();
    rpc.health(&old).await.unwrap();
    let command = format!("reverse {central_port}");
    let (next_satellite, result) = tokio::task::spawn_blocking(move || {
        let result = satellite.command(&command);
        (satellite, result)
    })
    .await
    .unwrap();
    satellite = next_satellite;
    assert_eq!(result, "reverse-ok");
    let (next_satellite, result) = tokio::task::spawn_blocking(move || {
        let result = satellite.command("hold");
        (satellite, result)
    })
    .await
    .unwrap();
    satellite = next_satellite;
    assert_eq!(result, "held");
    central.invalidate();
    let current = rpc.open_link().await.unwrap();
    assert_ne!(old.incarnation, current.incarnation);
    assert!(rpc.health(&old).await.is_err());
    rpc.health(&current).await.unwrap();
    let command = format!("stale {central_port}");
    let (mut satellite, result) = tokio::task::spawn_blocking(move || {
        let result = satellite.command(&command);
        (satellite, result)
    })
    .await
    .unwrap();
    assert_eq!(result, "stale-fenced");
    let command = format!("reverse {central_port}");
    let (satellite, result) = tokio::task::spawn_blocking(move || {
        let result = satellite.command(&command);
        (satellite, result)
    })
    .await
    .unwrap();
    assert_eq!(result, "reverse-ok");
    tokio::task::spawn_blocking(move || satellite.finish())
        .await
        .unwrap();
    server_stop.send_replace(true);
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_satellite_child() {
    if std::env::var(CHILD_ROLE).as_deref() != Ok("satellite") {
        return;
    }
    let satellite = channel(FederationRole::Satellite, 'a');
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop, mut stop_rx) = tokio::sync::watch::channel(false);
    let router = federation_router(satellite.clone(), allowed());
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { federation_wait_stop(&mut stop_rx).await })
            .await
            .unwrap();
    });
    println!("FDR_READY {port}");
    let mut held = None;
    loop {
        let command = tokio::task::spawn_blocking(|| {
            let mut command = String::new();
            std::io::stdin().read_line(&mut command).unwrap();
            command
        })
        .await
        .unwrap();
        let parts: Vec<_> = command.split_whitespace().collect();
        let result = match parts.as_slice() {
            ["exit"] | [] => {
                println!("FDR_RESULT stopped");
                break;
            }
            ["hold"] => {
                held = Some(satellite.current_lease().unwrap());
                "held"
            }
            [operation @ ("reverse" | "stale"), port] => {
                let rpc =
                    FederationRpcClient::new(port.parse().unwrap(), satellite.clone(), allowed())
                        .unwrap();
                let lease = if *operation == "stale" {
                    held.clone().unwrap()
                } else {
                    satellite.current_lease().unwrap()
                };
                let result = rpc.health(&lease).await;
                if *operation == "stale" {
                    assert!(result.is_err());
                    "stale-fenced"
                } else {
                    result.unwrap();
                    "reverse-ok"
                }
            }
            _ => panic!("unknown fixture phase"),
        };
        println!("FDR_RESULT {result}");
    }
    satellite.invalidate();
    stop.send_replace(true);
    server.await.unwrap();
}
