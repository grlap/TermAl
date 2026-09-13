// Finite read-process ownership tests shared by Work and review verification.
// Fixtures own their pipes/processes; no live agent or tracker is involved.
use super::*;

#[cfg(unix)]
fn observe_fixture_exit_without_reaping(pid: u32) {
    let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    loop {
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if result == 0 {
            break;
        }
        assert_eq!(
            io::Error::last_os_error().kind(),
            io::ErrorKind::Interrupted
        );
    }
}

#[cfg(unix)]
#[test]
fn bounded_read_old_constructor_reaps_an_already_exited_fixture() {
    // Harmless negative control: never signal this PID after the old wrapper
    // releases it. WNOWAIT deterministically forces the pre-constructor exit.
    let child = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
    observe_fixture_exit_without_reaping(child.id());
    let process = Arc::new(SharedChild::new(child).unwrap());
    assert_eq!(
        read_child_has_exited(&process).unwrap_err().raw_os_error(),
        Some(libc::ECHILD)
    );
    assert!(process.wait().unwrap().success());
}

#[cfg(unix)]
#[test]
fn bounded_read_retains_early_exit_until_group_cleanup_and_collects_after_deadline() {
    let mut command = Command::new("sh");
    command.args(["-c", "sleep 60 & printf 'early\\n'; exit 0"]);
    let output = run_bounded_read_process_with_setup(
        &mut command,
        std::time::Instant::now(),
        4096,
        true,
        |process| {
            observe_fixture_exit_without_reaping(process.id());
            // Still a waitable child: the identity is retained across repeated
            // observations until the transport terminates the owned group.
            assert!(read_child_has_exited(process)?);
            assert!(read_child_has_exited(process)?);
            Ok(())
        },
    )
    .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"early\n");
    assert!(output.stderr.is_empty());
}

#[test]
fn bounded_read_diagnostics_are_drained_and_truncated_without_losing_failure_status() {
    let diagnostics = bounded_read_pipe(io::Cursor::new(vec![b'x'; 100_000]), 4096, true)
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap();
    assert_eq!(diagnostics, vec![b'x'; 4096]);
    let overflow = bounded_read_pipe(io::Cursor::new(vec![b'x'; 100_000]), 4096, false)
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .unwrap_err();
    assert!(overflow.to_string().contains("output limit"));
}

#[cfg(unix)]
#[test]
fn bounded_read_reclaims_descendant_pipes_after_the_launcher_exits() {
    // The background sleep inherits both pipes. EOF before its 60-second
    // lifetime proves the owned group was terminated, not merely the shell.
    let mut command = Command::new("sh");
    command.args(["-c", "sleep 60 & printf 'launched\\n'; exit 0"]);
    let output = run_bounded_read_process(
        &mut command,
        std::time::Instant::now() + Duration::from_secs(5),
        4096,
        true,
    )
    .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"launched\n");
    assert!(output.stderr.is_empty());
}

#[cfg(unix)]
#[test]
fn bounded_read_deadline_kills_the_waiting_launcher_and_its_descendants() {
    let mut command = Command::new("sh");
    command.args(["-c", "sleep 60 & wait"]);
    let error = run_bounded_read_process(
        &mut command,
        std::time::Instant::now() + Duration::from_millis(200),
        4096,
        true,
    )
    .unwrap_err();
    assert!(error.to_string().contains("deadline"), "{error:#}");
}

#[cfg(windows)]
fn windows_bounded_fixture(marker: &FsPath, wait: bool) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "tests::bounded_read_process::bounded_read_windows_fixture_process",
            "--nocapture",
        ])
        .env("TERMAL_BOUNDED_FIXTURE_STAGE", "launcher")
        .env("TERMAL_BOUNDED_FIXTURE_MARKER", marker)
        .env("TERMAL_BOUNDED_FIXTURE_WAIT", if wait { "1" } else { "0" });
    command
}

#[cfg(windows)]
#[test]
fn bounded_read_windows_fixture_process() {
    let Ok(stage) = std::env::var("TERMAL_BOUNDED_FIXTURE_STAGE") else {
        return;
    };
    let marker = PathBuf::from(std::env::var_os("TERMAL_BOUNDED_FIXTURE_MARKER").unwrap());
    if stage == "descendant" {
        use std::io::Write;
        std::io::stdout().write_all(b"descendant-ready\n").unwrap();
        std::io::stdout().flush().unwrap();
        let pending_marker = marker.with_extension("ready.tmp");
        fs::write(&pending_marker, std::process::id().to_string()).unwrap();
        fs::rename(pending_marker, &marker).unwrap();
        std::thread::sleep(Duration::from_secs(60));
        return;
    }
    assert_eq!(stage, "launcher");
    let mut descendant = windows_bounded_fixture(&marker, false)
        .env("TERMAL_BOUNDED_FIXTURE_STAGE", "descendant")
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !marker.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "descendant readiness deadline"
        );
        assert!(
            descendant.try_wait().unwrap().is_none(),
            "descendant exited before readiness"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    if std::env::var("TERMAL_BOUNDED_FIXTURE_WAIT").unwrap() == "1" {
        descendant.wait().unwrap();
    }
    // Exiting without waiting deliberately leaves inherited pipes with the
    // descendant. Only the outer owned Job Object may clean them up.
}

#[cfg(windows)]
fn assert_windows_fixture_descendant_exited(marker: &FsPath) {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, GetLastError, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    let pid: u32 = fs::read_to_string(marker)
        .expect("descendant ran before cleanup")
        .parse()
        .unwrap();
    // Observe only; never signal a PID which could have been recycled.
    unsafe {
        let handle = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            assert_eq!(GetLastError(), ERROR_INVALID_PARAMETER);
        } else {
            let result = WaitForSingleObject(handle, 1000);
            CloseHandle(handle);
            assert_eq!(result, WAIT_OBJECT_0, "descendant survived job cleanup");
        }
    }
}

#[cfg(windows)]
#[test]
fn bounded_read_windows_job_reclaims_descendant_pipes_after_launcher_exit() {
    let marker = test_temp_dir().join(format!("bounded-ready-{}.txt", Uuid::new_v4()));
    let output = run_bounded_read_process(
        &mut windows_bounded_fixture(&marker, false),
        std::time::Instant::now() + Duration::from_secs(20),
        4096,
        true,
    )
    .unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("descendant-ready\n"));
    assert!(output.stderr.is_empty());
    assert_windows_fixture_descendant_exited(&marker);
}

#[cfg(windows)]
#[test]
fn bounded_read_windows_deadline_terminates_launcher_and_ready_descendant() {
    let marker = test_temp_dir().join(format!("bounded-deadline-{}.txt", Uuid::new_v4()));
    let error = run_bounded_read_process(
        &mut windows_bounded_fixture(&marker, true),
        std::time::Instant::now() + Duration::from_secs(15),
        4096,
        true,
    )
    .unwrap_err();
    assert!(error.to_string().contains("deadline"), "{error:#}");
    assert_windows_fixture_descendant_exited(&marker);
}
