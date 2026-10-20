// Owns syscall-sequence and native group-cleanup regressions for finite reads.
// Does not exercise live hosts or grant authority. Split from bounded_read_process tests.
use super::*;

struct GroupFixture {
    signal_errno: Option<i32>,
    reap_errno: Option<i32>,
    probes: std::collections::VecDeque<Option<i32>>,
    trace: Vec<&'static str>,
    retries: usize,
}

impl GroupFixture {
    fn new(signal_errno: Option<i32>, probes: &[Option<i32>]) -> Self {
        Self {
            signal_errno,
            reap_errno: None,
            probes: probes.iter().copied().collect(),
            trace: Vec::new(),
            retries: 2,
        }
    }
}

impl BoundedReadGroupIo for GroupFixture {
    type Status = u32;

    fn signal_before_reap(&mut self) -> io::Result<()> {
        assert!(!self.trace.contains(&"reap"), "non-zero signal after reap");
        self.trace.push("signal");
        self.signal_errno
            .map_or(Ok(()), |n| Err(io::Error::from_raw_os_error(n)))
    }

    fn reap(&mut self) -> io::Result<Self::Status> {
        assert_eq!(self.trace.last(), Some(&"signal"));
        self.trace.push("reap");
        self.reap_errno
            .map_or(Ok(23), |n| Err(io::Error::from_raw_os_error(n)))
    }

    fn observe_before_reap(&mut self) -> io::Result<bool> {
        Ok(true)
    }

    fn probe_after_reap(&mut self) -> io::Result<()> {
        assert!(self.trace.contains(&"reap"), "probe before reap");
        self.trace.push("probe");
        self.probes
            .pop_front()
            .expect("unbounded or unexpected probe")
            .map_or(Ok(()), |n| Err(io::Error::from_raw_os_error(n)))
    }

    fn retry_probe(&mut self) -> bool {
        self.trace.push("retry");
        if self.retries == 0 {
            return false;
        }
        self.retries -= 1;
        true
    }

    // Simulated Unix errno values, including when tests run on Windows.
    fn missing_errno(&self) -> i32 {
        3
    }
    fn permission_errno(&self) -> i32 {
        1
    }

    fn process_id(&self) -> u32 {
        4242
    }

    fn retain_unresolved(&mut self, _detail: String) {}
}

#[test]
fn bounded_read_cleanup_zombie_eperm_requires_reap_and_absence() {
    let mut ops = GroupFixture::new(Some(1), &[Some(3)]);
    assert_eq!(finish_bounded_read_group(&mut ops).unwrap(), 23);
    assert_eq!(ops.trace, ["signal", "reap", "probe"]);
}

#[test]
fn bounded_read_cleanup_live_descendants_are_signalled_before_bounded_probes() {
    let mut ops = GroupFixture::new(None, &[None, Some(1), Some(3)]);
    assert_eq!(finish_bounded_read_group(&mut ops).unwrap(), 23);
    assert_eq!(
        ops.trace,
        [
            "signal", "reap", "probe", "retry", "probe", "retry", "probe"
        ]
    );
}

#[test]
fn bounded_read_cleanup_permission_denied_live_member_stays_unresolved() {
    for probe_errno in [None, Some(1)] {
        let mut ops = GroupFixture::new(Some(1), &[probe_errno; 3]);
        let error = finish_bounded_read_group(&mut ops).unwrap_err().to_string();
        assert!(error.contains("unresolved"), "{error}");
        assert!(error.contains("signal_errno=Some(1)"), "{error}");
        assert!(
            error.contains(&format!("probe_errno={probe_errno:?}")),
            "{error}"
        );
        assert_eq!(ops.trace.iter().filter(|&&s| s == "probe").count(), 3);
    }
}

#[test]
fn bounded_read_cleanup_signal_success_does_not_confirm_persistent_group() {
    let mut ops = GroupFixture::new(None, &[None, None, None]);
    let error = finish_bounded_read_group(&mut ops).unwrap_err().to_string();
    assert!(error.contains("unresolved"), "{error}");
    assert!(error.contains("signal_errno=None"), "{error}");
    assert!(error.contains("probe_errno=None"), "{error}");
    assert_eq!(ops.trace.iter().filter(|&&s| s == "probe").count(), 3);
}

#[test]
fn bounded_read_cleanup_missing_signal_still_requires_post_reap_absence() {
    let mut ops = GroupFixture::new(Some(3), &[Some(3)]);
    assert_eq!(finish_bounded_read_group(&mut ops).unwrap(), 23);
    assert_eq!(ops.trace, ["signal", "reap", "probe"]);
}

#[test]
fn bounded_read_cleanup_reap_error_never_probes_or_claims_confirmation() {
    let mut ops = GroupFixture::new(None, &[]);
    ops.reap_errno = Some(10);
    assert!(
        finish_bounded_read_group(&mut ops)
            .unwrap_err()
            .to_string()
            .contains("reap")
    );
    assert_eq!(ops.trace, ["signal", "reap"]);
}

#[test]
fn bounded_read_cleanup_unknown_probe_error_is_unresolved_with_both_errnos() {
    let mut ops = GroupFixture::new(None, &[Some(5)]);
    let error = finish_bounded_read_group(&mut ops).unwrap_err().to_string();
    assert!(error.contains("unresolved"), "{error}");
    assert!(error.contains("signal_errno=None"), "{error}");
    assert!(error.contains("probe_errno=Some(5)"), "{error}");
    assert_eq!(ops.trace, ["signal", "reap", "probe"]);
}

#[test]
fn bounded_read_cleanup_unknown_signal_error_never_confirms_even_if_group_disappears() {
    let mut ops = GroupFixture::new(Some(22), &[Some(3)]);
    let error = finish_bounded_read_group(&mut ops).unwrap_err().to_string();
    assert!(error.contains("unresolved"), "{error}");
    assert!(error.contains("signal_errno=Some(22)"), "{error}");
    assert!(error.contains("probe_errno=Some(3)"), "{error}");
    assert_eq!(ops.trace, ["signal", "reap", "probe"]);
}

#[cfg(target_os = "macos")]
#[test]
fn bounded_read_cleanup_native_zombie_only_group_finishes_successfully() {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "printf 'zombie-only\\n'; exit 23"]);
    let output = run_bounded_read_process_with_setup(
        &mut command,
        std::time::Instant::now() + Duration::from_secs(2),
        4096,
        true,
        |process| {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while !read_child_has_exited(process)? {
                anyhow::ensure!(std::time::Instant::now() < deadline, "fixture did not exit");
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(output.status.code(), Some(23));
    assert_eq!(output.stdout, b"zombie-only\n");
    assert!(output.stderr.is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn bounded_read_cleanup_native_live_group_control_is_killed_and_reaped() {
    use std::os::unix::process::ExitStatusExt;
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", "printf 'ready\\n'; read line"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    configure_terminal_process_tree(&mut command);
    let process = Arc::new(host_command::spawn_shared(&mut command).unwrap());
    let tree = TerminalProcessTree::attach(&process).unwrap();
    let stdout = process.take_stdout().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut reader = io::BufReader::new(stdout);
        let mut line = String::new();
        let result = io::BufRead::read_line(&mut reader, &mut line).map(|_| line);
        let _ = tx.send(result);
    });
    let ready = rx.recv_timeout(Duration::from_secs(2));
    // Always clean up before making readiness assertions.
    let mut reaped = false;
    let result = finish_read_process_tree(&tree, &process, &mut reaped);
    if !reaped {
        let _ = process.kill();
        let _ = process.wait();
    }
    reader.join().unwrap();
    assert_eq!(ready.unwrap().unwrap(), "ready\n");
    assert_eq!(result.unwrap().signal(), Some(libc::SIGKILL));
    assert!(reaped);
}
