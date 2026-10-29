// Owns isolated child-pipe regressions for the production control bridge.
// Does not start SSH, use live peers, or change production deadlines.
use super::*;
use tokio::io::AsyncReadExt;

const CHILD_MODE: &str = "TERMAL_TEST_FEDERATION_STDIO";

#[test]
fn federation_stdio_child() {
    let Ok(mode) = std::env::var(CHILD_MODE) else {
        return;
    };
    println!("STDIO_READY");
    std::io::stdout().flush().unwrap();
    let mut preface = vec![0; FEDERATION_CONTROL_PREFACE.len()];
    std::io::stdin().read_exact(&mut preface).unwrap();
    assert_eq!(preface, FEDERATION_CONTROL_PREFACE);
    if mode == "eof" {
        // Do not append the test harness's terminal text to the binary pipe.
        std::process::exit(0);
    }
    let failure = match mode.as_str() {
        "dns" => Some("ssh: Could not resolve hostname secret.example: failure"),
        "auth" => Some("secret-user@secret.example: Permission denied (publickey)."),
        "key" => Some("Host key verification failed."),
        "forward" => Some("Error: remote port forwarding failed for listen port 47872"),
        "connect" => Some("ssh: connect to host secret.example port 22: Connection refused"),
        "unknown" => Some("opaque error with SENSITIVE_PAIR_SECRET_AND_CONTROL_BYTES"),
        _ => None,
    };
    if let Some(failure) = failure {
        eprintln!("{failure}");
        std::process::exit(255);
    }
    if mode == "partial" {
        std::io::stdout().write_all(&[0]).unwrap();
        std::io::stdout().flush().unwrap();
    }
    std::io::copy(&mut std::io::stdin(), &mut std::io::sink()).unwrap();
    std::process::exit(0);
}

async fn child(mode: &str) -> FederationSshProcess {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "tests::federation_stdio_bridge::federation_stdio_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_MODE, mode)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let mut process = FederationSshProcess::from_child(command.spawn().unwrap()).unwrap();
    let mut output = process.output.take().unwrap();
    let mut banner = tokio::task::spawn_blocking(move || -> std::io::Result<_> {
        // Single-threaded libtest may put its test-name prefix on this line.
        // Read no bytes past the marker: the original pipe belongs to the bridge.
        let mut line = Vec::new();
        for _ in 0..1024 {
            let mut byte = [0];
            output.read_exact(&mut byte)?;
            if byte[0] == b'\n' {
                if line.ends_with(b"STDIO_READY") {
                    return Ok(output);
                }
                line.clear();
            } else if byte[0] != b'\r' {
                line.push(byte[0]);
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "fixture startup banner too long",
        ))
    });
    let ready = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, &mut banner).await;
    let timed_out = ready.is_err();
    if let Ok(Ok(Ok(output))) = ready {
        process.output = Some(output);
        return process;
    }
    // Retain the process while awaiting readiness, so even a silent/malformed
    // banner is killed and reaped before reporting failure. Kill releases read.
    tokio::time::timeout(
        phase_sync::DEADLOCK_GUARD,
        tokio::task::spawn_blocking(move || process.shutdown()),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    if timed_out {
        let _ = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, banner)
            .await
            .unwrap();
    }
    panic!("fixture readiness marker missing or unreadable");
}

fn channel() -> (AppState, FederationChannel) {
    let app = federation_registry::central();
    let peer = federation_registry::peer(47104);
    let id = peer.identity.host_id.clone();
    app.enroll_federation_peer(peer).unwrap();
    let snapshot = app.federation_peer_snapshot(&id).unwrap();
    let mut channel = FederationChannel::for_app(
        snapshot,
        FederationPairCapability::new("a".repeat(64)).unwrap(),
        app.clone(),
    );
    channel.control.clock = FederationControlClock::scripted_io();
    (app, channel)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_stdio_child_eof_is_transport_loss_before_startup_deadline() {
    let (app, channel) = channel();
    let mut child = child("eof").await;
    let mut stream = child.take_control_stream().unwrap();
    assert!(
        child.take_control_stream().is_err(),
        "only one stream per owned child"
    );
    // The scripted startup clock never advances. A missing EOF would leave
    // this pending; only the outer deadlock guard may expire in the broken case.
    let result = tokio::time::timeout(
        phase_sync::DEADLOCK_GUARD,
        federation_attest_central(&channel, &mut stream),
    )
    .await;
    drop(stream);
    tokio::task::spawn_blocking(move || child.shutdown())
        .await
        .unwrap()
        .unwrap();
    let observed =
        app.inner.lock().unwrap().federation_attestation[&channel.snapshot.peer.host_id].project();
    assert_eq!(
        result.unwrap().err().unwrap().status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(observed["lastReason"], "linkDownBeforeHello");
    assert_eq!(observed["consecutiveFailures"], 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_stdio_live_silent_and_partial_children_keep_startup_deadline() {
    for mode in ["silent", "partial"] {
        let (app, channel) = channel();
        let mut child = child(mode).await;
        let (observed, byte_read) = tokio::sync::oneshot::channel();
        let mut stream: FederationControlIo = Box::new(ReadObserved {
            stream: child.take_control_stream().unwrap(),
            observed: Some(observed),
        });
        let result = {
            let proof = federation_attest_central(&channel, &mut stream);
            tokio::pin!(proof);
            if mode == "partial" {
                tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
                    tokio::select! {
                        biased;
                        _ = &mut proof => panic!("partial hello must remain pending"),
                        result = byte_read => result.unwrap(),
                    }
                })
                .await
                .unwrap();
            } else {
                tokio::select! {
                    biased;
                    _ = &mut proof => panic!("a live child has neither EOF nor complete hello"),
                    _ = tokio::task::yield_now() => {},
                }
            }
            channel
                .control
                .clock
                .advance(FEDERATION_LINK_STARTUP_TIMEOUT);
            tokio::time::timeout(phase_sync::DEADLOCK_GUARD, proof)
                .await
                .unwrap()
        };
        assert!(
            !child.child_exited().unwrap(),
            "timeout must not be confused with child termination"
        );
        drop(stream);
        tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
            while !child.child_exited().unwrap() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::task::spawn_blocking(move || child.shutdown())
            .await
            .unwrap()
            .unwrap();
        let observed = app.inner.lock().unwrap().federation_attestation
            [&channel.snapshot.peer.host_id]
            .project();
        assert_eq!(
            result.err().unwrap().status,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(observed["lastReason"], "startupExpired");
    }
}

// Observe delivery through the async reader, not the child's earlier write.
struct ReadObserved {
    stream: FederationControlIo,
    observed: Option<tokio::sync::oneshot::Sender<()>>,
}
impl tokio::io::AsyncRead for ReadObserved {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let result = tokio::io::AsyncRead::poll_read(std::pin::Pin::new(&mut self.stream), cx, buf);
        if matches!(&result, std::task::Poll::Ready(Ok(()))) && buf.filled().len() > before {
            if let Some(observed) = self.observed.take() {
                let _ = observed.send(());
            }
        }
        result
    }
}
impl tokio::io::AsyncWrite for ReadObserved {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        tokio::io::AsyncWrite::poll_write(std::pin::Pin::new(&mut self.stream), cx, bytes)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        tokio::io::AsyncWrite::poll_flush(std::pin::Pin::new(&mut self.stream), cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        tokio::io::AsyncWrite::poll_shutdown(std::pin::Pin::new(&mut self.stream), cx)
    }
}

struct Ended<T>(T, tokio::sync::mpsc::UnboundedSender<()>);
impl<T: std::io::Write> std::io::Write for Ended<T> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}
impl<T: std::io::Read> std::io::Read for Ended<T> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(bytes)
    }
}
impl<T> Drop for Ended<T> {
    fn drop(&mut self) {
        let _ = self.1.send(());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_stdio_local_close_releases_both_pipe_threads_and_owned_child() {
    let mut child = child("silent").await;
    let (ended, mut ends) = tokio::sync::mpsc::unbounded_channel();
    let mut stream = federation_stdio_control(
        Ended(child.input.take().unwrap(), ended.clone()),
        Ended(child.output.take().unwrap(), ended),
    )
    .unwrap();
    tokio::io::AsyncWriteExt::write_all(&mut stream, FEDERATION_CONTROL_PREFACE)
        .await
        .unwrap();
    drop(stream);
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
        ends.recv().await.unwrap();
        ends.recv().await.unwrap();
    })
    .await
    .unwrap();
    tokio::task::spawn_blocking(move || child.shutdown())
        .await
        .unwrap()
        .unwrap();
}

struct BrokenRead;
impl std::io::Read for BrokenRead {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::BrokenPipe.into())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_stdio_reader_error_also_closes_async_input() {
    let mut stream = federation_stdio_control(std::io::sink(), BrokenRead).unwrap();
    let count = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, stream.read(&mut [0]))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_stdio_cleanup_reaps_live_child_despite_poisoned_diagnostics() {
    let mut child = child("silent").await;
    let diagnostic = child.diagnostic.clone();
    let alive = !child.child_exited().unwrap();
    // Poison only the optional diagnostic owner, never the authority state.
    let poison = diagnostic.clone();
    assert!(std::panic::catch_unwind(move || {
        let _held = poison.lock().unwrap();
        panic!("injected diagnostic writer failure");
    })
    .is_err());
    tokio::time::timeout(
        phase_sync::DEADLOCK_GUARD,
        tokio::task::spawn_blocking(move || child.shutdown()),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    let value = diagnostic
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .project();
    assert!(alive);
    assert_eq!(value["cleanupRequested"], true);
    assert_eq!(value["exited"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_stdio_child_exit_has_only_bounded_classified_diagnostics() {
    for (mode, class) in [
        ("dns", "nameResolution"),
        ("auth", "authentication"),
        ("key", "hostKey"),
        ("forward", "forward"),
        ("connect", "connect"),
        ("unknown", "unknown"),
    ] {
        let mut child = child(mode).await;
        let diagnostic = child.diagnostic.clone();
        let mut stream = child.take_control_stream().unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut stream, FEDERATION_CONTROL_PREFACE)
            .await
            .unwrap();
        let count = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, stream.read(&mut [0]))
            .await
            .unwrap()
            .unwrap();
        drop(stream);
        tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
            while !child.child_exited().unwrap() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::task::spawn_blocking(move || child.shutdown())
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(phase_sync::DEADLOCK_GUARD, async {
            while !diagnostic.lock().unwrap().stderr_complete {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let value = diagnostic.lock().unwrap().project();
        assert_eq!(count, 0);
        assert_eq!(value["exitCode"], 255);
        assert_eq!(value["exited"], true);
        assert_eq!(value["class"], class);
        let text = value.to_string();
        assert!(!text.contains("secret") && !text.contains("SENSITIVE") && !text.contains("47872"));
        assert!(text.len() < 256);
    }
}

#[test]
fn federation_ssh_diagnostics_drain_beyond_limit_without_retaining_or_classifying_suffix() {
    let mut bytes = vec![b'x'; FEDERATION_SSH_DIAGNOSTIC_BYTES];
    bytes.extend_from_slice(b"Permission denied SENSITIVE_PAIR_SECRET_AND_CONTROL_BYTES");
    let diagnostics = Arc::new(Mutex::new(FederationSshDiagnostic::default()));
    let mut cursor = std::io::Cursor::new(bytes);
    federation_read_ssh_diagnostics(&mut cursor, &diagnostics);
    let value = diagnostics.lock().unwrap().project();
    assert_eq!(
        cursor.position(),
        cursor.get_ref().len() as u64,
        "drain the entire pipe"
    );
    assert_eq!(
        value["class"], "unknown",
        "only the bounded prefix is evidence"
    );
    assert_eq!(value["truncated"], true);
    assert_eq!(value["stderrComplete"], true);
    assert!(!value.to_string().contains("SENSITIVE"));
}
