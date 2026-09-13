// Shared finite read-command transport, introduced for host freeze and Work
// reads. Owns child/group and bounded pipe observation, not caller authority,
// command construction or response parsing. Extracted from review_freeze_process.rs.
fn bounded_read_pipe(
    mut reader: impl std::io::Read + Send + 'static,
    limit: usize,
    truncate: bool,
) -> mpsc::Receiver<io::Result<Vec<u8>>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut bytes = Vec::new();
        let result = (&mut reader)
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .and_then(|_| {
                if bytes.len() > limit {
                    if truncate {
                        bytes.truncate(limit);
                        // Keep draining diagnostics so a chatty child cannot
                        // deadlock on a full pipe. Only stdout is hashed.
                        io::copy(&mut reader, &mut io::sink()).map(|_| bytes)
                    } else {
                        Err(io::Error::other("bounded read output limit exceeded"))
                    }
                } else {
                    Ok(bytes)
                }
            });
        let _ = tx.send(result);
    });
    rx
}

fn run_bounded_read_process(
    command: &mut Command,
    deadline: std::time::Instant,
    output_limit: usize,
    own_tree: bool,
) -> Result<std::process::Output> {
    run_bounded_read_process_with_setup(command, deadline, output_limit, own_tree, |_| Ok(()))
}

// The setup boundary lets ownership tests force leader exit before observation.
// Production never installs another waiter or exposes this hook to callers.
fn run_bounded_read_process_with_setup(
    command: &mut Command,
    deadline: std::time::Instant,
    output_limit: usize,
    own_tree: bool,
    before_observe: impl FnOnce(&Arc<SharedChild>) -> Result<()>,
) -> Result<std::process::Output> {
    // Unix Git children stay in the checker process group, so terminating
    // the outer checker also terminates an in-flight Git read. Windows jobs
    // compose and retain descendant ownership even across nested jobs.
    let own_tree = own_tree || cfg!(windows);
    if own_tree {
        configure_terminal_process_tree(command);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};
        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // new(Child) calls try_wait and can reap an already-exited leader. spawn
    // preserves the PID even if exit happens before it returns to this worker.
    let process = Arc::new(SharedChild::spawn(command)?);
    let tree = match if own_tree {
        TerminalProcessTree::attach(&process).map(Some)
    } else {
        Ok(None)
    } {
        Ok(tree) => tree,
        Err(err) => {
            let _ = process.kill();
            let _ = process.wait();
            return Err(err);
        }
    };
    let mut reaped = false;
    let result = (|| {
        let stdout = process
            .take_stdout()
            .context("bounded read process stdout missing")?;
        let stderr = process
            .take_stderr()
            .context("bounded read process stderr missing")?;
        let out = bounded_read_pipe(stdout, output_limit, false);
        let err = bounded_read_pipe(stderr, 64 * 1024, true);
        if let Some(tree) = &tree {
            tree.resume_after_attach(&process)?;
        }
        before_observe(&process)?;
        loop {
            if read_child_has_exited(&process)? {
                break;
            }
            if std::time::Instant::now() >= deadline {
                bail!("bounded read deadline exceeded");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // Unix observes exit with WNOWAIT: no waiter has reaped the leader,
        // so its process-group identity cannot be reused before termination.
        if let Some(tree) = &tree {
            terminate_read_process_tree(tree, &process)?;
        }
        let status = process.wait()?;
        reaped = true;
        // A completed process may be observed just past its deadline. Give
        // both reader threads one shared, bounded EOF/drain grace, not a zero
        // timeout racing their sends and not an unbounded receive.
        let pipe_deadline = std::time::Instant::now() + Duration::from_secs(1);
        let stdout =
            out.recv_timeout(pipe_deadline.saturating_duration_since(std::time::Instant::now()))??;
        let stderr =
            err.recv_timeout(pipe_deadline.saturating_duration_since(std::time::Instant::now()))??;
        Ok(std::process::Output {
            status,
            stdout,
            stderr,
        })
    })();
    if result.is_err() {
        let cleanup = if let Some(tree) = &tree {
            if reaped {
                tree.cleanup_after_shell_exit(&process, "bounded read")
            } else {
                terminate_read_process_tree(tree, &process)
            }
        } else {
            process.kill().map_err(anyhow::Error::from)
        };
        if let Err(err) = cleanup {
            eprintln!("bounded read> cleanup: {err:#}");
        }
        let _ = process.wait();
    }
    result
}

fn terminate_read_process_tree(
    tree: &TerminalProcessTree,
    process: &Arc<SharedChild>,
) -> Result<()> {
    #[cfg(unix)]
    {
        let _ = tree;
        // Do not use helpers that spawn a wait thread: this worker alone must
        // keep the leader unreaped through group termination.
        terminate_terminal_process_group(
            terminal_process_group_id(process.id(), "bounded read")?,
            "bounded read",
        )
    }
    #[cfg(not(unix))]
    {
        tree.cleanup_after_shell_exit(process, "bounded read")
    }
}

fn read_child_has_exited(process: &Arc<SharedChild>) -> io::Result<bool> {
    #[cfg(unix)]
    {
        // This worker is the sole wait owner. waitid only observes the child;
        // SharedChild::wait remains responsible for the actual reap/status.
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                process.id() as libc::id_t,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                return Ok(false);
            }
            return Err(error);
        }
        Ok(unsafe { info.assume_init().si_pid() } != 0)
    }
    #[cfg(not(unix))]
    {
        // Windows Job Object ownership survives a reaped direct process.
        process.try_wait().map(|status| status.is_some())
    }
}
