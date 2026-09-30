use super::*;
use std::io::Read;
use std::time::Instant;

const GUARD: Duration = Duration::from_secs(30);
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
fn fixture() -> PathBuf {
    root().join("scripts/fixtures/windows-containment.cjs")
}
struct Scratch(PathBuf);
fn scratch_root() -> PathBuf {
    // Fixture writes must not become source-mutation evidence for their
    // own carried gate. Keep them repository-local under the watcher's
    // existing target exclusion, without excluding nested .tmp worktrees.
    root().join("target/.tmp/windows-launch-tests")
}
impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}
impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}
impl AsRef<OsStr> for Scratch {
    fn as_ref(&self) -> &OsStr {
        self.0.as_os_str()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if std::thread::panicking() {
            return;
        }
        let repository = std::fs::canonicalize(root()).unwrap();
        let allowed = std::fs::canonicalize(scratch_root()).unwrap();
        let target = std::fs::canonicalize(&self.0).unwrap();
        assert!(allowed.starts_with(&repository) && allowed != repository);
        assert!(target.starts_with(&allowed) && target != allowed);
        std::fs::remove_dir_all(&target).unwrap();
    }
}
fn scratch() -> Scratch {
    let path = scratch_root().join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&path).unwrap();
    Scratch(path)
}

#[test]
fn native_windows_launch_scratch_is_ignored_by_workspace_watcher() {
    let directory = scratch();
    let scratch_path = directory.to_path_buf();
    for relative in [
        "ready.json",
        "node.exe",
        "launcher/scripts/test-launcher.mjs",
    ] {
        let path = directory.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"fixture").unwrap();
        assert!(
            crate::is_ignored_workspace_file_event_path(&path),
            "fixture write would fence its own carried gate: {}",
            path.display()
        );
    }
    assert!(crate::is_ignored_workspace_file_event_path(&directory));
    // A nested worktree under .tmp still needs source-write observation.
    // Keeping fixture writes out of the watcher must not hide that worktree.
    assert!(!crate::is_ignored_workspace_file_event_path(
        &root().join(".tmp/wt-visible/src/main.rs")
    ));
    drop(directory);
    assert!(
        !scratch_path.exists(),
        "owned fixture scratch must be removed"
    );
}

fn node_spec(mode: &str) -> LaunchSpec {
    let mut spec = LaunchSpec::new("node.exe");
    spec.arg(fixture()).arg(mode).current_dir(root());
    spec
}
fn output(spec: &LaunchSpec, force_unavailable: bool) -> (std::process::Output, ContainmentStatus) {
    let launch = prepare_inner(spec, force_unavailable, false).unwrap();
    let process = launch.process();
    let mut stdout = process.take_stdout().unwrap();
    let mut stderr = process.take_stderr().unwrap();
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    launch.resume_after_attach(&process).unwrap();
    let status = process
        .wait_timeout(GUARD)
        .unwrap()
        .expect("fixture completion guard");
    launch
        .cleanup_after_shell_exit(&process, "fixture")
        .unwrap();
    (
        std::process::Output {
            status,
            stdout: out.join().unwrap(),
            stderr: err.join().unwrap(),
        },
        launch.containment().clone(),
    )
}

fn creation(handle: HANDLE) -> u64 {
    let mut created = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    assert_ne!(
        unsafe { GetProcessTimes(handle, &mut created, &mut exit, &mut kernel, &mut user) },
        0
    );
    (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime)
}
struct Identity {
    handle: OwnedHandle,
    pid: u32,
    created: u64,
}
impl Identity {
    fn capture(pid: u32, earliest: u64) -> Self {
        // PID comes only from the cooperating fixture's complete readiness
        // receipt. Retain identity before issuing any lifecycle operation.
        let handle = owned(unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
                0,
                pid,
            )
        })
        .unwrap();
        assert_eq!(unsafe { GetProcessId(handle.as_raw_handle()) }, pid);
        let created = creation(handle.as_raw_handle());
        assert!(created >= earliest);
        assert_eq!(
            unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) },
            WAIT_TIMEOUT
        );
        eprintln!("containment fixture ready> pid={pid} creationTime={created}");
        Self {
            handle,
            pid,
            created,
        }
    }
    fn assert_ended(&self) {
        assert_eq!(
            unsafe { WaitForSingleObject(self.handle.as_raw_handle(), GUARD.as_millis() as u32) },
            WAIT_OBJECT_0,
            "fixture pid {} survived",
            self.pid
        );
        assert_eq!(creation(self.handle.as_raw_handle()), self.created);
        eprintln!(
            "containment fixture ended> pid={} creationTime={}",
            self.pid, self.created
        );
    }
}
impl Drop for Identity {
    fn drop(&mut self) {
        // Failure cleanup is after the verdict and only through retained owned
        // fixture handles. It can never turn a failed observation into a pass.
        if unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 0) } == WAIT_TIMEOUT {
            unsafe {
                TerminateProcess(self.handle.as_raw_handle(), 1);
            }
            unsafe {
                WaitForSingleObject(self.handle.as_raw_handle(), GUARD.as_millis() as u32);
            }
        }
    }
}
fn ready(path: &Path, process: &WindowsChild) -> serde_json::Value {
    let deadline = Instant::now() + GUARD;
    loop {
        match std::fs::read(path) {
            Ok(bytes) => return serde_json::from_slice(&bytes).unwrap(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("readiness receipt: {error}"),
        }
        assert!(
            process.try_wait().unwrap().is_none(),
            "fixture exited before readiness"
        );
        assert!(Instant::now() < deadline, "fixture readiness guard");
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn identities(receipt: &serde_json::Value, earliest: u64) -> Vec<Identity> {
    receipt["pids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pid| Identity::capture(pid.as_u64().unwrap().try_into().unwrap(), earliest))
        .collect()
}

#[test]
fn native_windows_launch_closing_job_ends_ready_node_chain() {
    let directory = scratch();
    let marker = directory.join("ready.json");
    let mut spec = node_spec("root");
    spec.arg(&marker);
    let launch = prepare(&spec).unwrap();
    let process = launch.process();
    assert!(matches!(launch.containment(), ContainmentStatus::Contained));
    launch.resume_after_attach(&process).unwrap();
    let members = identities(
        &ready(&marker, &process),
        creation(process.handle.as_raw_handle()),
    );
    assert_eq!(members.len(), 3);
    launch
        .cleanup_after_shell_exit(&process, "fixture close")
        .unwrap();
    for member in &members {
        member.assert_ended();
    }
}

#[test]
fn native_windows_launch_job_teardown_does_not_require_root_termination() {
    let directory = scratch();
    let marker = directory.join("ready.json");
    let mut spec = node_spec("root");
    spec.arg(&marker);
    let launch = prepare(&spec).unwrap();
    let process = launch.process();
    launch.resume_after_attach(&process).unwrap();
    let members = identities(
        &ready(&marker, &process),
        creation(process.handle.as_raw_handle()),
    );
    assert_eq!(members.len(), 3);
    // Closing an owned kill-on-close job must suffice even when redundant
    // root termination is refused (as Windows does for terminating processes).
    process
        .deny_root_termination
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let teardown = launch.kill(&process, "fixture cancel");
    for member in &members {
        member.assert_ended();
    }
    assert!(
        teardown.is_ok(),
        "job teardown must not depend on a second root kill: {teardown:?}"
    );
}

#[test]
fn native_windows_launch_kill_accepts_root_already_ended_by_job() {
    let directory = scratch();
    let marker = directory.join("ready.json");
    let mut spec = node_spec("root");
    spec.arg(&marker);
    let launch = prepare(&spec).unwrap();
    let process = launch.process();
    launch.resume_after_attach(&process).unwrap();
    let members = identities(
        &ready(&marker, &process),
        creation(process.handle.as_raw_handle()),
    );
    launch
        .cleanup_after_shell_exit(&process, "fixture close")
        .unwrap();
    for member in &members {
        member.assert_ended();
    }
    assert_eq!(
        unsafe { TerminateProcess(process.handle.as_raw_handle(), 1) },
        0
    );
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(ERROR_ACCESS_DENIED as i32)
    );
    assert!(
        process.kill().is_ok(),
        "an already ended retained process is a successful kill"
    );
}

#[test]
fn native_windows_launch_intermediate_exit_does_not_close_root_lease() {
    let directory = scratch();
    let marker = directory.join("ready.json");
    let mut spec = node_spec("root");
    spec.arg(&marker);
    let launch = prepare(&spec).unwrap();
    let process = launch.process();
    launch.resume_after_attach(&process).unwrap();
    let members = identities(
        &ready(&marker, &process),
        creation(process.handle.as_raw_handle()),
    );
    assert_eq!(members.len(), 3);
    // Only this retained fixture member is ended. Kernel job ownership does
    // not treat intermediate exit as an instruction to close the host lease.
    assert_ne!(
        unsafe { TerminateProcess(members[1].handle.as_raw_handle(), 1) },
        0
    );
    members[1].assert_ended();
    assert_eq!(
        unsafe { WaitForSingleObject(members[0].handle.as_raw_handle(), 0) },
        WAIT_TIMEOUT
    );
    assert_eq!(
        unsafe { WaitForSingleObject(members[2].handle.as_raw_handle(), 0) },
        WAIT_TIMEOUT
    );
    launch
        .cleanup_after_shell_exit(&process, "fixture close")
        .unwrap();
    for member in &members {
        member.assert_ended();
    }
}

#[test]
fn native_windows_launch_supervises_real_focused_launcher_and_recovers_run() {
    let directory = scratch();
    let marker = directory.join("ready.json");
    // Real maintained launcher, private Git metadata: deliberately interrupted
    // fixture runs must never enter this worktree's acceptance evidence store.
    let repository = directory.join("repository");
    let clone = crate::Command::new("git")
        .args(["clone", "--shared", "--no-checkout", "--no-hardlinks"])
        .arg(root())
        .arg(&repository)
        .output()
        .unwrap();
    assert!(
        clone.status.success(),
        "{}",
        String::from_utf8_lossy(&clone.stderr)
    );
    std::fs::create_dir_all(repository.join("scripts")).unwrap();
    for filename in [
        "test-launcher.mjs",
        "review-freeze-fingerprint.mjs",
        "test-temp-root.mjs",
    ] {
        std::fs::copy(
            root().join("scripts").join(filename),
            repository.join("scripts").join(filename),
        )
        .unwrap();
    }
    let mut spec = LaunchSpec::new("node.exe");
    spec.args(["scripts/test-launcher.mjs", "focused", "--", "node.exe"])
        .arg(fixture())
        .arg("root")
        .arg(&marker)
        .current_dir(&repository);
    let launch = prepare(&spec).unwrap();
    let process = launch.process();
    assert!(matches!(launch.containment(), ContainmentStatus::Contained));
    let stdout = process.take_stdout().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout).lines() {
            let line = line.unwrap();
            if let Some(path) = line.strip_prefix("RUN ") {
                let _ = tx.send(PathBuf::from(path));
            }
        }
    });
    launch.resume_after_attach(&process).unwrap();
    let run = rx.recv_timeout(GUARD).expect("launcher RUN receipt");
    assert!(
        std::fs::canonicalize(&run)
            .unwrap()
            .starts_with(std::fs::canonicalize(&repository).unwrap())
    );
    let earliest = creation(process.handle.as_raw_handle());
    let mut members = identities(&ready(&marker, &process), earliest);
    members.push(Identity::capture(process.id(), earliest));
    assert_eq!(members.len(), 4);
    // End only the retained launcher root. Its host supervisor then observes
    // exit and closes the job, exactly as terminal and bounded reads do.
    process.kill().unwrap();
    process
        .wait_timeout(GUARD)
        .unwrap()
        .expect("launcher root exit");
    launch
        .cleanup_after_shell_exit(&process, "launcher fixture")
        .unwrap();
    for member in &members {
        member.assert_ended();
    }
    reader.join().unwrap();
    let mut recover = LaunchSpec::new("node.exe");
    recover
        .args(["scripts/test-launcher.mjs", "recover"])
        .arg(&run)
        .current_dir(&repository);
    let (recovered, _) = output(&recover, false);
    assert!(
        recovered.status.success(),
        "recover: {}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    let result: serde_json::Value =
        serde_json::from_slice(&std::fs::read(run.join("results.json")).unwrap()).unwrap();
    assert_eq!(result["state"], "failed");
    assert_eq!(result["interrupted"], true);
    assert_eq!(result["exitCode"], 1);
    eprintln!(
        "containment real launcher proof> run={} recovered interrupted; no stage rerun",
        run.display()
    );
}

#[test]
fn native_windows_launch_matches_command_argv_environment_cwd_and_bytes() {
    let cwd = scratch();
    let args = [
        "",
        "ordinary",
        "two words",
        "a\tb",
        "quote\"inside",
        "space tail\\",
        "a\\\\\"b",
        "żółć 🦀",
    ];
    let mut spec = node_spec("parity");
    spec.args(args)
        .current_dir(&cwd)
        .env("pAtH", std::env::var_os("PATH").unwrap())
        .env("TERMAL_NATIVE_VALUE", "first")
        .env("termal_native_value", "last żółć")
        .env("TERMAL_NATIVE_REMOVED", "set then removed")
        .env_remove("TERMAL_NATIVE_REMOVED")
        .env("TERMAL_NATIVE_UNICODE", "界");
    let mut command = std::process::Command::new("node.exe");
    command
        .arg(fixture())
        .arg("parity")
        .args(args)
        .current_dir(&cwd)
        .env("pAtH", std::env::var_os("PATH").unwrap())
        .env("TERMAL_NATIVE_VALUE", "first")
        .env("termal_native_value", "last żółć")
        .env("TERMAL_NATIVE_REMOVED", "set then removed")
        .env_remove("TERMAL_NATIVE_REMOVED")
        .env("TERMAL_NATIVE_UNICODE", "界");
    let expected = crate::host_command::reference_output(&mut command).unwrap();
    let (actual, status) = output(&spec, false);
    assert!(matches!(status, ContainmentStatus::Contained));
    assert_eq!(actual.status.code(), expected.status.code());
    assert_eq!(actual.stdout, expected.stdout);
    assert_eq!(actual.stderr, expected.stderr);
}

#[test]
fn native_windows_launch_matches_command_executable_resolution() {
    let directory = scratch();
    let executable = resolve_program(&LaunchSpec::new("node.exe")).unwrap();
    let executable = PathBuf::from(OsString::from_wide(&executable[..executable.len() - 1]));
    // Read the installed binary and own a disposable copy inside the repo.
    // Hard-link creation can require rights on a protected source executable.
    std::fs::copy(&executable, directory.join("node.exe")).unwrap();
    for program in [
        OsString::from("node"),
        directory.join("node").into_os_string(),
    ] {
        let mut spec = LaunchSpec::new(&program);
        spec.arg(fixture())
            .arg("parity")
            .env("PATH", &directory)
            .current_dir(&directory);
        let mut command = std::process::Command::new(&program);
        command
            .arg(fixture())
            .arg("parity")
            .env("PATH", &directory)
            .current_dir(&directory);
        let expected = crate::host_command::reference_output(&mut command).unwrap();
        let (actual, _) = output(&spec, false);
        assert_eq!(actual.status.code(), expected.status.code());
        assert_eq!(actual.stdout, expected.stdout);
        assert_eq!(actual.stderr, expected.stderr);
        assert_eq!(
            resolve_program(&spec).unwrap(),
            user_path(&directory.join("node.exe"), false).unwrap()
        );
    }
    let scratch_path = directory.0.clone();
    drop(directory);
    assert!(
        !scratch_path.exists(),
        "copied executable scratch must be removed"
    );
}

#[test]
fn native_windows_bounded_read_closes_job_after_ready_root_exit() {
    let directory = scratch();
    let marker = directory.join("ready.json");
    let mut spec = node_spec("root");
    spec.arg(&marker);
    let mut members = Vec::new();
    let result = crate::run_bounded_read_windows_with_setup(
        &mut spec,
        Instant::now() + GUARD,
        4096,
        |process| {
            members = identities(
                &ready(&marker, process),
                creation(process.handle.as_raw_handle()),
            );
            process.kill()?;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(members.len(), 3);
    assert!(String::from_utf8_lossy(&result.stdout).contains("pids"));
    for member in &members {
        member.assert_ended();
    }
}

#[test]
fn native_windows_launch_rejects_normalized_batch_paths() {
    let directory = scratch();
    for filename in ["shim.cmd", "shim.bat"] {
        std::fs::write(directory.join(filename), "@echo unsafe\r\n").unwrap();
    }
    for filename in ["shim.cmd.", "shim.cmd...", "shim.bat ", "shim.bat. "] {
        for program in [
            OsString::from(filename),
            directory.join(filename).into_os_string(),
        ] {
            let mut spec = LaunchSpec::new(program);
            spec.env("PATH", &directory).arg("& unsafe argument");
            let error = match prepare(&spec) {
                Err(error) => error,
                Ok(_) => panic!("batch spelling was admitted: {filename}"),
            };
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert!(error.to_string().contains("batch shims"), "{error}");
        }
    }
}

#[test]
fn native_windows_launch_reports_unavailable_and_still_runs_command() {
    let (actual, status) = output(&node_spec("parity"), true);
    assert!(
        matches!(status, ContainmentStatus::Unavailable { reason } if reason.contains("forced job setup unavailable"))
    );
    assert_eq!(actual.status.code(), Some(37));
    assert!(!actual.stdout.is_empty());
}

#[test]
fn native_windows_launch_nonmember_lease_keeps_root_termination_available() {
    let directory = scratch();
    let marker = directory.join("ready.json");
    let mut spec = node_spec("hold");
    spec.arg(&marker);
    let launch = prepare_inner(&spec, true, false).unwrap();
    let process = launch.process();
    {
        let mut lease = launch.job.lock().unwrap();
        *lease = Some(create_job().unwrap());
        assert_eq!(
            membership_failure(&mut lease, process.handle.as_raw_handle()).as_deref(),
            Some("root is not a member of the launch job")
        );
        assert!(lease.is_none());
    }
    launch.resume_after_attach(&process).unwrap();
    let members = identities(
        &ready(&marker, &process),
        creation(process.handle.as_raw_handle()),
    );
    launch.kill(&process, "nonmember fixture").unwrap();
    process
        .wait_timeout(GUARD)
        .unwrap()
        .expect("uncontained root ended");
    members[0].assert_ended();
}

#[test]
fn native_windows_launch_forced_atomic_refusal_uses_fresh_job() {
    let mut launch = prepare_inner(&node_spec("parity"), false, true).unwrap();
    let process = launch.process();
    let observer = launch.refused_job_observer.take().unwrap();
    {
        let lease = launch.job.lock().unwrap();
        let fresh = lease.as_ref().unwrap();
        assert_eq!(
            unsafe { CompareObjectHandles(observer.as_raw_handle(), fresh.as_raw_handle()) },
            0
        );
        let mut in_fresh = 0;
        assert_ne!(
            unsafe {
                IsProcessInJob(
                    process.handle.as_raw_handle(),
                    fresh.as_raw_handle(),
                    &mut in_fresh,
                )
            },
            0
        );
        assert_ne!(in_fresh, 0);
        let mut in_refused = 0;
        assert_ne!(
            unsafe {
                IsProcessInJob(
                    process.handle.as_raw_handle(),
                    observer.as_raw_handle(),
                    &mut in_refused,
                )
            },
            0
        );
        assert_eq!(in_refused, 0);
    }
    drop(observer);
    assert!(matches!(launch.containment(), ContainmentStatus::Contained));
    launch.resume_after_attach(&process).unwrap();
    assert_eq!(
        process.wait_timeout(GUARD).unwrap().unwrap().code(),
        Some(37)
    );
    launch
        .cleanup_after_shell_exit(&process, "forced fallback fixture")
        .unwrap();
}

#[test]
fn native_windows_launch_does_not_inherit_unrelated_handle() {
    let security = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    let unrelated = owned(unsafe { CreateEventW(&security, 1, 1, null()) }).unwrap();
    let launch = prepare(&node_spec("parity")).unwrap();
    let process = launch.process();
    let mut duplicate = null_mut();
    let duplicated = unsafe {
        DuplicateHandle(
            process.handle.as_raw_handle(),
            unrelated.as_raw_handle(),
            GetCurrentProcess(),
            &mut duplicate,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if duplicated != 0 {
        // Numeric handle values may collide. Compare the kernel objects, not
        // the number or the successful duplication of some unrelated object.
        let duplicate = owned(duplicate).unwrap();
        assert_eq!(
            unsafe { CompareObjectHandles(unrelated.as_raw_handle(), duplicate.as_raw_handle()) },
            0
        );
    }
    // Drop terminates the never-resumed process using its original handle.
}

#[test]
fn native_windows_launch_serializes_stdio_with_a_concurrent_standard_child() {
    let directory = scratch();
    let marker = directory.join("legacy-ready.json");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let legacy_barrier = barrier.clone();
    let legacy_marker = marker.clone();
    let (attempt_tx, attempt_rx) = std::sync::mpsc::channel();
    let legacy = std::thread::spawn(move || {
        legacy_barrier.wait();
        assert!(crate::host_command::inheritance_is_locked());
        attempt_tx.send(()).unwrap();
        let mut command = crate::Command::new("node.exe");
        command
            .arg(fixture())
            .arg("hold")
            .arg(legacy_marker)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        command.spawn().unwrap()
    });
    let launch = prepare_with_hook(&node_spec("parity"), false, false, |handles| {
        for &handle in handles {
            let mut flags = 0;
            assert_ne!(unsafe { GetHandleInformation(handle, &mut flags) }, 0);
            assert_ne!(flags & HANDLE_FLAG_INHERIT, 0);
        }
        barrier.wait();
        attempt_rx.recv_timeout(GUARD).unwrap();
    })
    .unwrap();
    let mut legacy = legacy.join().unwrap();
    let legacy_identity = Identity::capture(legacy.id(), creation(legacy.as_raw_handle()));
    let process = launch.process();
    ready(&marker, &process);
    let mut stdout = process.take_stdout().unwrap();
    let (eof_tx, eof_rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        eof_tx.send(stdout.read_to_end(&mut bytes)).unwrap();
    });
    launch.resume_after_attach(&process).unwrap();
    process.wait_timeout(GUARD).unwrap().unwrap();
    launch
        .cleanup_after_shell_exit(&process, "overlap fixture")
        .unwrap();
    // EOF must arrive independently of the unrelated, still-running std child.
    assert!(eof_rx.recv_timeout(GUARD).unwrap().unwrap() > 0);
    assert!(legacy.try_wait().unwrap().is_none());
    reader.join().unwrap();
    legacy.kill().unwrap();
    legacy.wait().unwrap();
    legacy_identity.assert_ended();
}

#[test]
fn host_windows_command_capture_releases_the_lock_before_waiting() {
    for reference in [false, true] {
        let directory = scratch();
        let marker = directory.join("capture-ready.json");
        let output_marker = marker.clone();
        let first = std::thread::spawn(move || {
            if reference {
                let mut command = std::process::Command::new("node.exe");
                command.arg(fixture()).arg("hold").arg(output_marker);
                return crate::host_command::reference_output(&mut command).unwrap();
            }
            crate::Command::new("node.exe")
                .arg(fixture())
                .arg("hold")
                .arg(output_marker)
                .output()
                .unwrap()
        });
        let deadline = Instant::now() + GUARD;
        let receipt: serde_json::Value = loop {
            if let Ok(bytes) = std::fs::read(&marker) {
                break serde_json::from_slice(&bytes).unwrap();
            }
            assert!(Instant::now() < deadline, "capture readiness receipt");
            std::thread::sleep(Duration::from_millis(10));
        };
        let identities = identities(&receipt, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        let second = std::thread::spawn(move || {
            let result = crate::Command::new("node.exe")
                .arg(fixture())
                .arg("parity")
                .output();
            let _ = tx.send(result);
        });
        let result = rx
            .recv_timeout(GUARD)
            .expect("spawn blocked behind another child's output")
            .unwrap();
        assert_eq!(result.status.code(), Some(37));
        assert_eq!(
            unsafe { WaitForSingleObject(identities[0].handle.as_raw_handle(), 0) },
            WAIT_TIMEOUT
        );
        assert_ne!(
            unsafe { TerminateProcess(identities[0].handle.as_raw_handle(), 1) },
            0
        );
        identities[0].assert_ended();
        first.join().unwrap();
        second.join().unwrap();
    }
}

#[test]
fn native_windows_launch_wait_distinguishes_final_259_from_still_active() {
    let mut spec = LaunchSpec::new("powershell.exe");
    spec.args(["-NoProfile", "-NonInteractive", "-Command", "exit 259"]);
    let launch = prepare(&spec).unwrap();
    let process = launch.process();
    assert!(process.try_wait().unwrap().is_none());
    launch.resume_after_attach(&process).unwrap();
    assert_eq!(
        process.wait_timeout(GUARD).unwrap().unwrap().code(),
        Some(259)
    );
    assert_eq!(
        process
            .wait_timeout(Duration::ZERO)
            .unwrap()
            .unwrap()
            .code(),
        Some(259)
    );
}

#[test]
fn native_windows_work_spec_matches_adapter_interpreter_and_environment() {
    for binary in ["C:/fixture/engram.exe", "C:/fixture/engram.ps1"] {
        for context in [None, Some("reviewer context".to_owned())] {
            let connection = crate::EngramConnectionConfig {
                binary_path: binary.into(),
                project_file: "C:/fixture/.engram-project".into(),
                home: "C:/fixture/store".into(),
                project_root: "C:/fixture".into(),
                actor_id: "fixture actor".into(),
                session_id: "fixture session".into(),
                actor_context: context,
            };
            let mut expected = crate::engram_command(&connection.binary_path);
            crate::apply_engram_connection_environment(&mut expected, &connection);
            expected.current_dir(&connection.project_root);
            let actual = crate::work_read_launch_command(&connection);
            assert_eq!(actual.get_program(), expected.get_program());
            assert_eq!(
                actual.get_args().collect::<Vec<_>>(),
                expected.get_args().collect::<Vec<_>>()
            );
            assert_eq!(
                actual.get_envs().collect::<Vec<_>>(),
                expected.get_envs().collect::<Vec<_>>()
            );
            assert_eq!(actual.get_current_dir(), expected.get_current_dir());
        }
    }
}

#[test]
fn native_windows_launch_store_powershell_policy_is_explicit() {
    let mut discover = LaunchSpec::new("powershell.exe");
    discover.args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "(Get-AppxPackage -Name Microsoft.PowerShell | Select-Object -First 1).InstallLocation",
    ]);
    let (discovery, _) = output(&discover, false);
    assert!(
        discovery.status.success(),
        "Store package discovery failed: {}",
        String::from_utf8_lossy(&discovery.stderr)
    );
    let location = String::from_utf8(discovery.stdout).unwrap();
    let location = location.trim();
    if location.is_empty() {
        eprintln!(
            "NOT APPLICABLE: Microsoft.PowerShell Store package is not installed for this Windows user"
        );
        return;
    }
    let pwsh = PathBuf::from(location).join("pwsh.exe");
    assert!(pwsh.is_file(), "installed Store package has no pwsh.exe");
    let directory = scratch();
    let marker = directory.join("ready.json");
    let mut spec = LaunchSpec::new(&pwsh);
    spec.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "& $env:TERMAL_NATIVE_NODE $env:TERMAL_NATIVE_FIXTURE root $env:TERMAL_NATIVE_READY",
    ])
    .env(
        "TERMAL_NATIVE_NODE",
        OsString::from_wide(
            &resolve_program(&LaunchSpec::new("node.exe"))
                .unwrap()
                .into_iter()
                .take_while(|unit| *unit != 0)
                .collect::<Vec<_>>(),
        ),
    )
    .env("TERMAL_NATIVE_FIXTURE", fixture())
    .env("TERMAL_NATIVE_READY", &marker)
    .current_dir(root());
    let launch = prepare(&spec).unwrap();
    let process = launch.process();
    assert!(
        matches!(launch.containment(), ContainmentStatus::ContainedWithPackagedIdentityChanged { package_full_name } if package_full_name.starts_with("Microsoft.PowerShell_")),
        "{:?}",
        launch.containment()
    );
    launch.resume_after_attach(&process).unwrap();
    let earliest = creation(process.handle.as_raw_handle());
    let mut members = identities(&ready(&marker, &process), earliest);
    for member in &members {
        assert!(
            package(member.handle.as_raw_handle()).unwrap().is_some(),
            "policy must retain package identity in descendants"
        );
        let mut in_job = 0;
        assert_ne!(
            unsafe {
                IsProcessInJob(
                    member.handle.as_raw_handle(),
                    launch.job.lock().unwrap().as_ref().unwrap().as_raw_handle(),
                    &mut in_job,
                )
            },
            0
        );
        assert_ne!(in_job, 0);
    }
    members.push(Identity::capture(process.id(), earliest));
    assert_eq!(members.len(), 4);
    launch
        .cleanup_after_shell_exit(&process, "Store fixture")
        .unwrap();
    for member in &members {
        member.assert_ended();
    }
}
