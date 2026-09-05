//! Event-driven fixture waits. The limit diagnoses deadlocks, not performance.

use super::*;

pub(super) use crate::TEST_PHASE_DEADLOCK_GUARD as DEADLOCK_GUARD;

/// An external work-context fixture stays in flight until its owning test
/// releases it. Dropping the owner releases both current and subsequent reads.
pub(super) struct WorkContextGate {
    listener: std::net::TcpListener,
    peer: Option<std::net::TcpStream>,
    released_path: PathBuf,
}

impl WorkContextGate {
    pub(super) fn new(root: &FsPath) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = listener.local_addr().unwrap();
        let executable = std::env::current_exe().unwrap();
        let filter = format!("{}::work_context_release_child", module_path!());
        let filter = filter.split_once("::").unwrap().1;
        #[cfg(windows)]
        let (path, script) = (
            root.join("work-context-gate.cmd"),
            format!(
                "@echo off\r\nset TERMAL_TEST_WORK_CONTEXT_GATE={endpoint}\r\n\"{}\" --exact {filter} --nocapture\r\n",
                executable.display()
            ),
        );
        #[cfg(not(windows))]
        let (path, script) = (
            root.join("work-context-gate.sh"),
            format!(
                "#!/bin/sh\nexport TERMAL_TEST_WORK_CONTEXT_GATE={endpoint}\nexec '{}' --exact {filter} --nocapture\n",
                executable.to_string_lossy().replace('\'', "'\\''")
            ),
        );
        fs::write(path, script).unwrap();
        Self {
            listener,
            peer: None,
            released_path: root.join("work-context-released"),
        }
    }

    pub(super) fn wait(&mut self) {
        let publication = PollGuard::new();
        let mut peer = loop {
            match self.listener.accept() {
                Ok((peer, _)) => break peer,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    publication.wait("work-context fixture TCP readiness")
                }
                Err(error) => panic!("work-context fixture accept failed: {error}"),
            }
        };
        peer.set_read_timeout(Some(DEADLOCK_GUARD)).unwrap();
        let mut ready = [0];
        peer.read_exact(&mut ready)
            .expect("work-context fixture entered its release gate");
        assert_eq!(ready, [1]);
        self.peer = Some(peer);
    }

    pub(super) fn release(&mut self) {
        fs::write(&self.released_path, "released").unwrap();
        if let Some(peer) = self.peer.take() {
            peer.shutdown(std::net::Shutdown::Both).unwrap();
        }
    }
}

impl Drop for WorkContextGate {
    fn drop(&mut self) {
        let _ = fs::write(&self.released_path, "released");
        if let Some(peer) = self.peer.take() {
            let _ = peer.shutdown(std::net::Shutdown::Both);
        }
    }
}

#[test]
fn work_context_release_child() {
    let Ok(endpoint) = std::env::var("TERMAL_TEST_WORK_CONTEXT_GATE") else {
        return;
    };
    let mut peer = std::net::TcpStream::connect(endpoint).unwrap();
    peer.write_all(&[1]).unwrap();
    // No timer expiry: an explicit release or owner unwind closes the socket.
    let mut release = [0];
    let _ = peer.read(&mut release);
}

/// Fallback for fixture state that has no notification channel. Callers must
/// check a published predicate on every iteration; elapsed time never means
/// success. Prefer `receive` or a state subscription when one is available.
pub(super) struct PollGuard(std::time::Instant);

impl PollGuard {
    pub(super) fn new() -> Self {
        Self(std::time::Instant::now())
    }

    #[track_caller]
    pub(super) fn wait(&self, phase: impl std::fmt::Display) {
        assert!(
            self.0.elapsed() < DEADLOCK_GUARD,
            "fixture phase `{phase}` has not published readiness after {:?}",
            self.0.elapsed()
        );
        // Back off repeated observations, not a delay assumed to complete work.
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A process with no natural timer expiry. The retained input pipe keeps its
/// read blocked; Drop kills/reaps it even when an assertion unwinds the test.
#[must_use = "retain the owner until after every process-lifetime assertion"]
pub(super) struct ParkedProcess {
    pub(super) process: Arc<SharedChild>,
    _input: std::process::ChildStdin,
}

impl ParkedProcess {
    pub(super) fn spawn() -> Self {
        #[cfg(windows)]
        let mut command = {
            let mut command = Command::new("cmd");
            command.args(["/D", "/C", "set /p fixture_blocked="]);
            command
        };
        #[cfg(not(windows))]
        let mut command = {
            let mut command = Command::new("sh");
            command.args(["-c", "read fixture_blocked"]);
            command
        };
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().expect("parked fixture stdin");
        Self {
            process: Arc::new(SharedChild::new(child).unwrap()),
            _input: input,
        }
    }
}

impl Drop for ParkedProcess {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

#[test]
fn parked_process_requires_termination_before_exit_can_be_observed() {
    let owner = ParkedProcess::spawn();
    // Negative observation: omitting cleanup must not publish an exit while
    // the owner retains the input pipe. No timeout can release that pipe.
    assert!(
        wait_for_shared_child_exit_timeout(
            &owner.process,
            Duration::from_millis(10),
            "retained parked fixture without termination",
        )
        .unwrap()
        .is_none()
    );
    owner.process.kill().unwrap();
    process_exit(&owner.process, "explicitly terminated parked fixture");
}

#[test]
fn parked_process_owner_reaps_on_assertion_unwind() {
    let owner = ParkedProcess::spawn();
    let process = owner.process.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _owner = owner;
        panic!("synthetic assertion failure before process termination");
    }));
    assert!(result.is_err());
    assert!(process.try_wait().unwrap().is_some());
}

/// Wait for the OS exit publication, not a latency budget after `kill` returns.
#[track_caller]
pub(super) fn process_exit(process: &Arc<SharedChild>, phase: &str) -> std::process::ExitStatus {
    let child = process.clone();
    let (sender, receiver) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let _ = sender.send(child.wait());
    });
    let status = receive(&receiver, phase).expect("fixture process wait should succeed");
    waiter.join().expect("fixture process waiter should finish");
    status
}

#[track_caller]
pub(super) fn receive<T>(receiver: &mpsc::Receiver<T>, phase: &str) -> T {
    receive_result(receiver, phase).expect("fixture publisher disconnected")
}

/// Preserves channel-disconnect assertions at call sites that match a Result.
#[track_caller]
pub(super) fn receive_result<T>(
    receiver: &mpsc::Receiver<T>,
    phase: &str,
) -> Result<T, mpsc::RecvError> {
    match receive_before_cleanup(receiver, phase) {
        Ok(value) => Ok(value),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(mpsc::RecvError),
        Err(error) => panic!("fixture phase `{phase}` did not complete: {error}"),
    }
}

/// Cleanup-sensitive tests release/join their fixture before asserting this
/// result. Keep timeout as an error here so that cleanup still runs.
#[track_caller]
pub(super) fn receive_before_cleanup<T>(
    receiver: &mpsc::Receiver<T>,
    phase: &str,
) -> Result<T, mpsc::RecvTimeoutError> {
    let started = std::time::Instant::now();
    let result = receiver.recv_timeout(DEADLOCK_GUARD);
    if let Err(error) = &result {
        eprintln!(
            "fixture phase `{phase}` failed after {:?}: {error}",
            started.elapsed()
        );
    }
    result
}

/// Self-exec target for process-tree fixtures. Readiness is acknowledged by the
/// parent test before stdout releases the control fixture's startup handshake.
#[test]
fn parked_control_descendant() {
    let Ok(endpoint) = std::env::var("TERMAL_TEST_DESCENDANT_ENDPOINT") else {
        return;
    };
    let mut stream = std::net::TcpStream::connect(endpoint).expect("connect descendant readiness");
    stream.set_read_timeout(Some(DEADLOCK_GUARD)).unwrap();
    stream.write_all(&std::process::id().to_be_bytes()).unwrap();
    let mut ready = [0];
    stream
        .read_exact(&mut ready)
        .expect("parent should acquire the descendant probe");
    assert_eq!(ready, [1]);
    println!("termal-descendant-ready");
    std::io::stdout().flush().unwrap();
    stream.set_read_timeout(None).unwrap();
    // No natural lifetime: only production tree cleanup (or parent test
    // teardown closing this socket after an assertion failure) releases us.
    let _ = stream.read(&mut ready);
}
