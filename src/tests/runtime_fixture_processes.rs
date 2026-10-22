use super::*;

#[cfg(unix)]
fn observe_child_slot(pid: u32) -> (i32, Option<i32>) {
    // SAFETY: waitid initializes the valid siginfo pointer. WNOWAIT observes
    // without reaping, including if a PID has already been reused by a child
    // belonging to another concurrently running test.
    let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    (result, io::Error::last_os_error().raw_os_error())
}

#[cfg(unix)]
#[test]
fn successful_child_fixture_reaps_before_returning_its_handle() {
    let mut child = test_exit_success_child();
    // A completed fixture must already have released its kernel process slot.
    // Unlike Child::try_wait, this observation cannot hide a missing wait by
    // consulting the handle's cached status.
    let (observed, error) = observe_child_slot(child.id());
    // Keep the regression's failing path from leaking its own child.
    let _ = child.wait();
    assert_eq!(observed, -1, "fixture returned a running or unreaped child");
    assert_eq!(error, Some(libc::ECHILD));
}

#[test]
fn runtime_handle_fixtures_retain_success_without_unreaped_children() {
    let (codex, _codex_commands) = test_codex_runtime_handle("reaped-codex");
    let (claude, _claude_commands) = test_claude_runtime_handle("reaped-claude");
    let (acp, _acp_commands) = test_acp_runtime_handle(AcpAgent::Cursor, "reaped-acp");
    let (_shared, _shared_commands, shared_process) = test_shared_codex_runtime("reaped-shared");
    for process in [&codex.process, &claude.process, &acp.process, &shared_process] {
        #[cfg(unix)]
        {
            let (observed, error) = observe_child_slot(process.id());
            if observed == 0 {
                let _ = process.wait();
            }
            assert_eq!(observed, -1, "runtime fixture retained an unreaped child");
            assert_eq!(error, Some(libc::ECHILD));
        }
        assert!(
            process
                .try_wait()
                .unwrap()
                .expect("cached fixture exit")
                .success()
        );
        assert!(process.wait().unwrap().success());
    }
}
