// Shared finite read-command transport, introduced for host freeze and Work
// reads. Owns child/group and bounded pipe observation, not caller authority,
// command construction or response parsing. Extracted from review_freeze_process.rs.
#[cfg(windows)]
type BoundedReadCommand = windows_launch::LaunchSpec;
#[cfg(not(windows))]
type BoundedReadCommand = Command;

// This is constructed at the read caller, rather than copying opaque Command
// state. The adapter keeps its separate transport during this migration.
#[cfg(not(windows))]
fn bounded_read_command(binary: &FsPath) -> BoundedReadCommand {
    engram_command(binary)
}

fn run_bounded_read_command(
    command: &mut BoundedReadCommand,
    deadline: std::time::Instant,
    output_limit: usize,
    own_tree: bool,
) -> Result<std::process::Output> {
    #[cfg(not(windows))]
    {
        run_bounded_read_process(command, deadline, output_limit, own_tree)
    }
    #[cfg(windows)]
    {
        let _ = own_tree;
        run_bounded_read_windows_with_setup(command, deadline, output_limit, |_| Ok(()))
    }
}

// Fixed host-owned Git batch input only; no caller-controlled executable,
// script, redirection, or scratch file. Pipe writers run beside pipe readers
// and are released by the same deadline/tree cleanup as the ordinary read.
fn run_bounded_read_input(
    command: &mut BoundedReadCommand,
    deadline: std::time::Instant,
    output_limit: usize,
    input: Vec<u8>,
) -> Result<std::process::Output> {
    #[cfg(windows)]
    {
        command.piped_stdin();
        run_bounded_read_windows_with_setup(command, deadline, output_limit, move |process| {
            let mut stdin = process.take_stdin().context("batch stdin missing")?;
            std::thread::Builder::new()
                .name("review-batch-input".to_owned())
                .spawn(move || {
                    let _ = stdin.write_all(&input);
                })
                .context("batch input thread unavailable")?;
            Ok(())
        })
    }
    #[cfg(not(windows))]
    {
        run_bounded_read_process_input(
            command,
            deadline,
            output_limit,
            false,
            |_| Ok(()),
            Some(input),
        )
    }
}

// Same private setup boundary as the legacy transport: tests can pin every
// fixture identity and force leader exit before the supervisor observes it.
#[cfg(windows)]
fn run_bounded_read_windows_with_setup(
    command: &mut BoundedReadCommand,
    deadline: std::time::Instant,
    output_limit: usize,
    before_observe: impl FnOnce(&Arc<windows_launch::WindowsChild>) -> Result<()>,
) -> Result<std::process::Output> {
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    if deadline <= std::time::Instant::now() {
        bail!("bounded read deadline exceeded before launch");
    }
    let launch = windows_launch::prepare(command)?;
    let process = launch.process();
    let result = (|| {
        let stdout = process
            .take_stdout()
            .context("bounded read process stdout missing")?;
        let stderr = process
            .take_stderr()
            .context("bounded read process stderr missing")?;
        let out = bounded_read_pipe(stdout, output_limit, false);
        let err = bounded_read_pipe(stderr, 64 * 1024, true);
        launch.resume_after_attach(&process)?;
        before_observe(&process)?;
        let status = process
            .wait_timeout(deadline.saturating_duration_since(std::time::Instant::now()))?
            .context("bounded read deadline exceeded")?;
        launch.cleanup_after_shell_exit(&process, "bounded read")?;
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
        if let Err(error) = launch.kill(&process, "bounded read") {
            eprintln!("bounded read> cleanup: {error:#}");
        }
        let _ = process.wait();
    }
    result
}

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

#[cfg(any(not(windows), test))]
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
#[cfg(any(not(windows), test))]
fn run_bounded_read_process_with_setup(
    command: &mut Command,
    deadline: std::time::Instant,
    output_limit: usize,
    own_tree: bool,
    before_observe: impl FnOnce(&Arc<SharedChild>) -> Result<()>,
) -> Result<std::process::Output> {
    run_bounded_read_process_input(
        command,
        deadline,
        output_limit,
        own_tree,
        before_observe,
        None,
    )
}

#[cfg(any(not(windows), test))]
fn run_bounded_read_process_input(
    command: &mut Command,
    deadline: std::time::Instant,
    output_limit: usize,
    own_tree: bool,
    before_observe: impl FnOnce(&Arc<SharedChild>) -> Result<()>,
    input: Option<Vec<u8>>,
) -> Result<std::process::Output> {
    #[cfg(windows)]
    eprintln!(
        "windows launch> unavailable: legacy Command has opaque launch settings; using legacy job ownership"
    );
    // Unix Git children stay in the checker process group, so terminating
    // the outer checker also terminates an in-flight Git read. Windows jobs
    // compose and retain descendant ownership even across nested jobs.
    let own_tree = own_tree || cfg!(windows);
    if own_tree {
        configure_terminal_process_tree(command);
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};
        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
    }
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // new(Child) calls try_wait and can reap an already-exited leader. spawn
    // preserves the PID even if exit happens before it returns to this worker.
    let process = Arc::new(host_command::spawn_shared(command)?);
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
    #[cfg(unix)]
    let mut group_cleanup_started = false;
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
        if let Some(input) = input {
            let mut stdin = process.take_stdin().context("batch stdin missing")?;
            std::thread::Builder::new()
                .name("review-batch-input".to_owned())
                .spawn(move || {
                    let _ = stdin.write_all(&input);
                })
                .context("batch input thread unavailable")?;
        }
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
        let status = if let Some(tree) = &tree {
            #[cfg(unix)]
            {
                group_cleanup_started = true;
            }
            finish_read_process_tree(tree, &process, &mut reaped)?
        } else {
            let status = process.wait()?;
            reaped = true;
            status
        };
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
                #[cfg(unix)]
                if group_cleanup_started {
                    // The unresolved contract retained this exact owner. Do
                    // not reset its budget or start a second wait/signal.
                    Ok(())
                } else {
                    finish_read_process_tree(tree, &process, &mut reaped).map(|_| ())
                }
                #[cfg(not(unix))]
                finish_read_process_tree(tree, &process, &mut reaped).map(|_| ())
            }
        } else {
            process.kill().map_err(anyhow::Error::from)
        };
        if let Err(err) = cleanup {
            eprintln!("bounded read> cleanup: {err:#}");
            #[cfg(unix)]
            if tree.is_some() {
                return result.map_err(|error| error.context(format!("{err:#}")));
            }
        }
        #[cfg(unix)]
        if tree.is_none() {
            let _ = process.wait();
        }
        // Unix-owned trees were either reaped by the contract or retained
        // unresolved. A blocking final wait would defeat its finite budget.
        #[cfg(not(unix))]
        let _ = process.wait();
    }
    result
}

#[cfg(any(not(windows), test))]
fn finish_read_process_tree(
    tree: &TerminalProcessTree,
    process: &Arc<SharedChild>,
    reaped: &mut bool,
) -> Result<std::process::ExitStatus> {
    #[cfg(unix)]
    {
        let _ = tree;
        // Do not use helpers that spawn a wait thread: this worker alone must
        // keep the leader unreaped through group termination.
        let reap_state = if *reaped {
            BoundedReadReapState::Reaped
        } else {
            BoundedReadReapState::Unreaped
        };
        let admission = terminal_process_group_id(process.id(), "bounded read").and_then(|pgid| {
            anyhow::ensure!(pgid > 0, "bounded read process group must be positive");
            Ok(pgid)
        });
        let pgid = admit_bounded_read_group(process.id(), admission, |detail| {
            retain_unresolved_bounded_read(process.clone(), reap_state, detail);
        })?;
        finish_bounded_read_group(&mut UnixBoundedReadGroup {
            process,
            reaped,
            reap_state,
            pgid,
            probe_deadline: std::time::Instant::now() + Duration::from_millis(250),
        })
    }
    #[cfg(not(unix))]
    {
        tree.cleanup_after_shell_exit(process, "bounded read")?;
        let status = process.wait()?;
        *reaped = true;
        Ok(status)
    }
}

#[cfg(any(not(windows), test))]
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
