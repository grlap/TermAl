// A real native test remains in progress while its holder closes. This is
// distinct from delaying delivery of a result that has already completed.
use super::*;
use std::io::{Read, Write};

struct NativePhaseGate {
    address: std::net::SocketAddr,
    entered: mpsc::Receiver<()>,
    release: Option<mpsc::Sender<()>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl NativePhaseGate {
    fn new() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (entered, observed) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(DEADLOCK_GUARD)).unwrap();
            let mut kind = [0; 6];
            stream.read_exact(&mut kind).unwrap();
            if &kind != b"native" {
                return;
            } // Drop wakes an unentered accept.
            entered.send(()).unwrap();
            if released.recv_timeout(DEADLOCK_GUARD).is_ok() {
                let _ = stream.write_all(b"go");
            }
        });
        Self {
            address,
            entered: observed,
            release: Some(release),
            worker: Some(worker),
        }
    }

    fn entered(&self) {
        self.entered
            .recv_timeout(DEADLOCK_GUARD)
            .expect("the actual native test reaches its body, not just the launcher registration");
    }

    fn release(mut self) {
        self.release.take().unwrap().send(()).unwrap();
    }
}

impl Drop for NativePhaseGate {
    fn drop(&mut self) {
        // On failure release a connected test, or wake accept if compilation
        // failed before any test connected. No child is left waiting forever.
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if let Ok(mut wake) = std::net::TcpStream::connect(self.address) {
            let _ = wake.write_all(b"cancel");
        }
        if let Some(worker) = self.worker.take() {
            worker.join().expect("native phase gate joins");
        }
    }
}

fn install_native_phase_test(matrix: &Matrix) {
    fs::write(
        matrix.fixture.root.join("src/lib.rs"),
        r#"#[test]
fn checked_content() {
    use std::io::{Read, Write};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let address = std::fs::read_to_string(root.join("target/matrix-phase-address")).unwrap();
    let mut stream = std::net::TcpStream::connect(address.trim()).unwrap();
    stream.write_all(b"native").unwrap();
    let mut release = [0; 2];
    stream.read_exact(&mut release).unwrap();
    assert_eq!(&release, b"go");
    assert!(!std::fs::read_to_string(root.join("README.md")).unwrap().trim().is_empty());
}
"#,
    )
    .unwrap();
    fs::write(matrix.fixture.root.join("launcher-check.test.mjs"), r#"import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { once } from "node:events";
import { createConnection } from "node:net";
import test from "node:test";
test("checked content", async () => {
  const [host, port] = readFileSync(new URL("./target/matrix-phase-address", import.meta.url), "utf8").trim().split(":");
  const stream = createConnection({host, port: Number(port)});
  await once(stream, "connect");
  stream.write("native");
  let message = Buffer.alloc(0);
  for await (const chunk of stream) {
    message = Buffer.concat([message, chunk]);
    if (message.length >= 2) break;
  }
  stream.destroy();
  assert.equal(message.toString(), "go");
  assert.match(readFileSync(new URL("./README.md", import.meta.url), "utf8"), /\S/);
});
"#).unwrap();
    run_git_test_command(
        &matrix.fixture.root,
        &["add", "--", "src/lib.rs", "launcher-check.test.mjs"],
    );
    run_git_test_command(
        &matrix.fixture.root,
        &["commit", "--quiet", "-m", "fixture native phase gate"],
    );
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn first_close_while_native_test_is_running_accepts_its_later_result() {
    let matrix = Matrix::new(Citation::BoundVerification, false);
    install_native_phase_test(&matrix);
    matrix.begin("Run a correctly placed native check, then close while it is still in progress.");
    let mut events =
        matrix
            .provider
            .events(&matrix.fixture, &matrix.holder, "inflight-check", false);
    events.start();
    matrix.fixture.wait_for_check_capture(&matrix.holder);
    let output = std::thread::scope(|scope| {
        // This guard is inside the scope, so unwinding releases the native
        // test before the scope joins its launcher thread.
        let gate = NativePhaseGate::new();
        let target = matrix.fixture.root.join("target");
        fs::create_dir_all(&target).unwrap();
        fs::write(
            target.join("matrix-phase-address"),
            gate.address.to_string(),
        )
        .unwrap();
        let launcher = scope.spawn(|| {
            let output = isolated_launcher(&matrix.fixture.root, &matrix.holder, &matrix.service)
                .output()
                .expect("actual native launcher starts");
            if !output.status.success() {
                eprintln!(
                    "MATRIX_INFLIGHT_NATIVE_FAILURE stdout_bytes={} stderr_bytes={}",
                    output.stdout.len(),
                    output.stderr.len()
                );
            }
            output
        });
        gate.entered();
        let registration = matrix
            .fixture
            .live
            .state
            .inner
            .lock()
            .unwrap()
            .test_run_registrations
            .values()
            .find(|row| row.owning_session_id == matrix.holder)
            .unwrap()
            .clone();
        let running: Value = serde_json::from_slice(
            &fs::read(FsPath::new(&registration.run_directory).join("results.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            running["state"], "running",
            "the native test is genuinely unfinished"
        );
        matrix.fixture.live.state.refresh_test_runs();
        matrix.fixture.live.state.engram_verification_records_tick();
        assert!(
            matrix
                .fixture
                .live
                .transport
                .requests_for("verification_record")
                .is_empty()
        );
        matrix.close();
        matrix.settle();
        // The producer has durably answered the close before the actual test
        // may finish. Feed positions and completed_at stay their real values.
        gate.release();
        launcher.join().unwrap()
    });
    let mut check = matrix.actual_check_output(output);
    assert_eq!(check.exit, 0, "{}", check.output);
    matrix.receive_check(events, &mut check, false);
    eprintln!(
        "MATRIX_INFLIGHT_CLOSING_REQUEST_COUNT {}",
        matrix.fixture.live.transport.requests_for("execution_observe").len()
    );
    matrix.accept(&check, || {});
}
