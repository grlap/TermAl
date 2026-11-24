// Keeps a delegation's dedicated Codex app-server and everything it starts
// inside one ending, on Linux.
//
// Owns the keeper: a small mode of the TermAl executable
// (`termal __codex-keeper <control fd> <app-server argv...>`) that TermAl
// starts in place of a delegation's app-server. The keeper is a child
// subreaper, so every process the app-server starts re-parents to the keeper
// when its own parent exits, however it detached (Codex's shell tools call
// `setsid`). The keeper ends that whole tree when the app-server exits by
// itself (a crash, an OOM kill) or when its control pipe closes, which happens
// when TermAl ends the delegation's server and when TermAl itself exits. Also
// owns the TermAl side of the launch: the keeper command and the pipe.
//
// Does not own when a server ends (`owned_codex_server.rs`), nor the JSON-RPC
// wire (`codex.rs`): the app-server inherits the keeper's stdio, so TermAl
// talks to it exactly as before. Windows keeps its kill-on-close job and
// other Unix hosts end only the server. The keeper signals only its own
// unreaped children, whose pids cannot be reused while they stay unreaped.

use super::*;
use std::ffi::OsString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::sync::atomic::AtomicI32;

/// The first argument that selects keeper mode in the TermAl executable.
pub(crate) const CODEX_KEEPER_MODE: &str = "__codex-keeper";

/// Under test the keeper runs as a fixture of the test binary; its control
/// fd and the app-server argv travel in this variable as JSON.
#[cfg(test)]
pub(crate) const TEST_CODEX_KEEPER: &str = "TERMAL_TEST_CODEX_KEEPER";
#[cfg(test)]
const TEST_CODEX_KEEPER_ENTRY: &str = "codex_keeper::codex_keeper_test_entry";

/// The write end of a keeper's control pipe. Holding it keeps the
/// delegation's server alive; closing it, or TermAl's own exit, ends the
/// server and every process it started.
pub(crate) struct CodexKeeperControl(#[allow(dead_code)] OwnedFd);

/// A keeper launch being prepared: the command that starts the keeper, the
/// control pipe's write end, and its read end, which the keeper inherits and
/// which must stay open until the command has been spawned.
pub(crate) struct CodexKeeperLaunch {
    pub(crate) command: Command,
    pub(crate) control: CodexKeeperControl,
    pub(crate) keeper_end: OwnedFd,
}

/// The keeper command that starts the app-server `server` describes. The
/// server's environment and working directory are set on the keeper, which
/// the server inherits; its argv is passed through unchanged.
pub(crate) fn codex_keeper_launch(server: &Command) -> Result<CodexKeeperLaunch> {
    let (keeper_end, control) = control_pipe()?;
    let control_fd = keeper_end.as_raw_fd();
    let mut command = keeper_command(server, control_fd)?;
    for (key, value) in server.get_envs() {
        match value {
            Some(value) => command.env(key, value),
            None => command.env_remove(key),
        };
    }
    if let Some(dir) = server.get_current_dir() {
        command.current_dir(dir);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: only async-signal-safe calls run between fork and exec.
    unsafe {
        command.pre_exec(move || {
            // A session of its own: a terminal's signals reach neither the
            // keeper nor its server.
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            // The one descriptor the keeper inherits beyond its stdio.
            if libc::fcntl(control_fd, libc::F_SETFD, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(CodexKeeperLaunch {
        command,
        control: CodexKeeperControl(control),
        keeper_end,
    })
}

/// `/proc/self/exe` names the running image even after an install replaced
/// the file on disk, so a keeper always starts from TermAl's own build.
#[cfg(not(test))]
fn keeper_command(server: &Command, control_fd: RawFd) -> Result<Command> {
    let mut command = Command::new("/proc/self/exe");
    command
        .arg(CODEX_KEEPER_MODE)
        .arg(control_fd.to_string())
        .arg(server.get_program())
        .args(server.get_args());
    Ok(command)
}

#[cfg(test)]
fn keeper_command(server: &Command, control_fd: RawFd) -> Result<Command> {
    let text = |value: &std::ffi::OsStr| {
        value
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("a test keeper argv must be UTF-8"))
    };
    let mut argv = vec![text(server.get_program())?];
    for arg in server.get_args() {
        argv.push(text(arg)?);
    }
    let mut command = Command::new("/proc/self/exe");
    command
        .args([
            "--exact",
            TEST_CODEX_KEEPER_ENTRY,
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(
            TEST_CODEX_KEEPER,
            json!({ "control_fd": control_fd, "argv": argv }).to_string(),
        );
    Ok(command)
}

/// The keeper's end of the pipe and TermAl's end, both close-on-exec. The
/// keeper's end is moved above the stdio numbers, which the child's own
/// stdio setup would overwrite.
fn control_pipe() -> Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as libc::c_int; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error()).context("failed to create the keeper control pipe");
    }
    // SAFETY: pipe2 returned two descriptors this process now owns.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    if read.as_raw_fd() > 2 {
        return Ok((read, write));
    }
    let moved = unsafe { libc::fcntl(read.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if moved == -1 {
        return Err(io::Error::last_os_error()).context("failed to place the keeper control pipe");
    }
    // SAFETY: F_DUPFD_CLOEXEC returned a new descriptor this process owns.
    Ok((unsafe { OwnedFd::from_raw_fd(moved) }, write))
}

/// Keeper mode, when the TermAl executable was started as a keeper: the
/// exit code to leave with. `None` for every other mode.
pub(crate) fn run_codex_keeper_if_requested() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next()? != CODEX_KEEPER_MODE {
        return None;
    }
    let control_fd = args
        .next()
        .and_then(|fd| fd.to_str()?.parse::<RawFd>().ok());
    let (Some(control_fd), Some(program)) = (control_fd, args.next()) else {
        eprintln!(
            "[termal] Codex keeper: usage: {CODEX_KEEPER_MODE} <control fd> <program> [args...]"
        );
        return Some(2);
    };
    Some(keep(control_fd, program, args.collect()))
}

/// The keeper fixture of the test binary; a no-op in an ordinary test run.
#[cfg(test)]
#[test]
fn codex_keeper_test_entry() {
    let Ok(request) = std::env::var(TEST_CODEX_KEEPER) else {
        return;
    };
    let request: Value = serde_json::from_str(&request).unwrap();
    let control_fd = request["control_fd"].as_i64().unwrap() as RawFd;
    let mut argv = request["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| OsString::from(arg.as_str().unwrap()));
    let program = argv.next().unwrap();
    std::process::exit(keep(control_fd, program, argv.collect()));
}

/// Runs the app-server and keeps its tree; returns the exit code to leave
/// with, or leaves by the signal that ended the server.
fn keep(control_fd: RawFd, program: OsString, args: Vec<OsString>) -> i32 {
    match keep_tree(control_fd, program, args) {
        Ok(status) => leave_like(status),
        Err(error) => {
            eprintln!("[termal] Codex keeper: {error:#}");
            1
        }
    }
}

fn keep_tree(
    control_fd: RawFd,
    program: OsString,
    args: Vec<OsString>,
) -> Result<std::process::ExitStatus> {
    // Before anything starts: orphans of the tree re-parent here, not to init.
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } == -1 {
        return Err(io::Error::last_os_error()).context("failed to become a child subreaper");
    }
    // The drain names children through /proc. Where /proc numbers processes
    // differently from this keeper's own calls, no tree is started that the
    // keeper could not end.
    if !procfs_shares_our_pid_namespace() {
        bail!(
            "/proc is not in this process's PID namespace, so the app-server's tree could not be ended; not starting it"
        );
    }
    let control = take_control(control_fd)?;
    let wake = install_wake()?;
    let keeper = unsafe { libc::getpid() };
    let mut server = std::process::Command::new(&program);
    server.args(&args);
    #[cfg(test)]
    server.env_remove(TEST_CODEX_KEEPER);
    // SAFETY: only async-signal-safe calls run between fork and exec.
    unsafe {
        server.pre_exec(move || {
            // The server dies with the keeper, even a keeper killed outright,
            // so it is never left running without the keeper's subreaper.
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != keeper {
                libc::raise(libc::SIGKILL);
            }
            Ok(())
        });
    }
    let server = server
        .spawn()
        .with_context(|| format!("failed to start {}", program.to_string_lossy()))?;
    let server = server.id() as libc::pid_t;
    // The server holds the wire now; the keeper keeps only its stderr, for
    // its own diagnostics, so stdin and stdout end with the server.
    release_wire()?;
    // The tree is ended however supervision ends, a failed wait included:
    // the drain kills the server too if it is still running.
    let status = supervise(server, control, &wake);
    if let Err(error) = drain() {
        eprintln!("[termal] Codex keeper: the app-server's tree was not fully ended: {error:#}");
    }
    status
}

/// The inherited read end of the control pipe, no longer inherited further.
fn take_control(control_fd: RawFd) -> Result<OwnedFd> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(control_fd, stat.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error()).context("the keeper control pipe is missing");
    }
    // SAFETY: fstat succeeded and filled `stat`.
    let mode = unsafe { stat.assume_init() }.st_mode;
    if mode & libc::S_IFMT != libc::S_IFIFO {
        bail!("descriptor {control_fd} is not the keeper control pipe");
    }
    if unsafe { libc::fcntl(control_fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error())
            .context("failed to keep the control pipe to the keeper");
    }
    // SAFETY: the descriptor was inherited for the keeper alone.
    Ok(unsafe { OwnedFd::from_raw_fd(control_fd) })
}

fn release_wire() -> Result<()> {
    let null = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .context("failed to open /dev/null")?;
    for fd in [libc::STDIN_FILENO, libc::STDOUT_FILENO] {
        if unsafe { libc::dup2(null.as_raw_fd(), fd) } == -1 {
            return Err(io::Error::last_os_error()).context("failed to release the wire");
        }
    }
    Ok(())
}

/// Where the signal handler reports: the write end of the wake pipe.
static WAKE: AtomicI32 = AtomicI32::new(-1);

/// Writes the signal's number to the wake pipe and nothing else.
extern "C" fn on_signal(signal: libc::c_int) {
    // SAFETY: errno is saved and restored around one write(2), which is
    // async-signal-safe; a full pipe drops the byte without blocking.
    unsafe {
        let errno = *libc::__errno_location();
        let fd = WAKE.load(Ordering::Relaxed);
        if fd >= 0 {
            let byte = signal as u8;
            libc::write(fd, (&byte as *const u8).cast(), 1);
        }
        *libc::__errno_location() = errno;
    }
}

/// The signals the keeper acts on, delivered as bytes on a pipe it polls,
/// so that only the keeper's own loop ever reaps or signals a child.
const FORWARDED_SIGNALS: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP];

fn install_wake() -> Result<OwnedFd> {
    let mut fds = [0 as libc::c_int; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error()).context("failed to create the keeper wake pipe");
    }
    // The write end lives as long as the process: a handler may use it at
    // any moment.
    WAKE.store(fds[1], Ordering::Relaxed);
    for signal in std::iter::once(libc::SIGCHLD).chain(FORWARDED_SIGNALS) {
        // SAFETY: a zeroed sigaction with an empty mask and a plain handler.
        let installed = unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            let handler: extern "C" fn(libc::c_int) = on_signal;
            action.sa_sigaction = handler as libc::sighandler_t;
            action.sa_flags = libc::SA_RESTART
                | if signal == libc::SIGCHLD {
                    libc::SA_NOCLDSTOP
                } else {
                    0
                };
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut())
        };
        if installed == -1 {
            return Err(io::Error::last_os_error())
                .context("failed to install a keeper signal handler");
        }
    }
    // SAFETY: pipe2 returned this descriptor, which the keeper now owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fds[0]) })
}

/// Runs until the server has exited, and returns how it exited. A closed
/// control pipe ends the server; a forwarded signal reaches it. The server
/// is signalled only while this loop, its only reaper, has not reaped it.
fn supervise(
    server: libc::pid_t,
    control: OwnedFd,
    wake: &OwnedFd,
) -> Result<std::process::ExitStatus> {
    let mut control = Some(control);
    loop {
        if let Some(status) = reap_exited(server)? {
            return Ok(status);
        }
        let mut fds = [
            libc::pollfd {
                fd: wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                // poll(2) skips a negative descriptor.
                fd: control.as_ref().map_or(-1, AsRawFd::as_raw_fd),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(error).context("keeper poll failed");
        }
        if fds[1].revents != 0 {
            // TermAl never writes: any readiness is its end of the pipe
            // closing, by `end` or by TermAl's own exit.
            control = None;
            unsafe { libc::kill(server, libc::SIGKILL) };
        }
        if fds[0].revents != 0 {
            let mut bytes = [0u8; 64];
            loop {
                let read =
                    unsafe { libc::read(wake.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len()) };
                if read <= 0 {
                    break;
                }
                for &signal in &bytes[..read as usize] {
                    let signal = libc::c_int::from(signal);
                    if FORWARDED_SIGNALS.contains(&signal) {
                        unsafe { libc::kill(server, signal) };
                    }
                }
            }
        }
    }
}

/// Reaps every exited child without waiting; the server's status once it
/// is among them. Orphans of the tree that exit are reaped here too.
fn reap_exited(server: libc::pid_t) -> Result<Option<std::process::ExitStatus>> {
    loop {
        let mut status = 0;
        let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if pid == server {
            return Ok(Some(std::process::ExitStatus::from_raw(status)));
        }
        if pid > 0 {
            continue;
        }
        if pid == 0 {
            return Ok(None);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => continue,
            _ => return Err(error).context("keeper wait failed"),
        }
    }
}

/// Ends every process left in the tree. Each is this keeper's child or
/// becomes one when its parent ends, so the keeper kills its children and
/// reaps one, round after round, until it has no child left at all. It has
/// no deadline and leaves only then: a child stuck in uninterruptible sleep
/// holds the drain in `waitpid` until it exits, after which its own children
/// are the keeper's and are killed in the next round. A round whose children
/// could not be listed still waits for one to exit and lists them again.
fn drain() -> Result<()> {
    if !procfs_shares_our_pid_namespace() {
        bail!("/proc is not in this process's PID namespace, so no child could be named");
    }
    loop {
        let children = own_children().unwrap_or_else(|error| {
            eprintln!(
                "[termal] Codex keeper: this round's children could not be listed: {error:#}"
            );
            Vec::new()
        });
        for pid in children {
            // An unreaped child's pid cannot be reused, exited or not.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        let mut status = 0;
        if unsafe { libc::waitpid(-1, &mut status, 0) } > 0 {
            continue;
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::ECHILD) => return Ok(()),
            _ => return Err(error).context("keeper wait failed"),
        }
    }
}

/// This process's children, read from the children list of each of its
/// threads; where the kernel lacks that list, from every process's parent.
fn own_children() -> Result<Vec<libc::pid_t>> {
    let mut children: Vec<libc::pid_t> = Vec::new();
    for task in fs::read_dir("/proc/self/task").context("failed listing /proc/self/task")? {
        let path = task?.path().join("children");
        match fs::read_to_string(&path) {
            Ok(list) => children.extend(
                list.split_whitespace()
                    .filter_map(|pid| pid.parse::<libc::pid_t>().ok()),
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return children_by_parent(),
            Err(error) => {
                return Err(error).with_context(|| format!("failed reading {}", path.display()));
            }
        }
    }
    Ok(children)
}

fn children_by_parent() -> Result<Vec<libc::pid_t>> {
    let keeper = unsafe { libc::getpid() };
    let mut children = Vec::new();
    for entry in fs::read_dir("/proc").context("failed listing /proc")? {
        let Ok(entry) = entry else { continue };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<libc::pid_t>().ok())
        else {
            continue;
        };
        // A process may exit while the scan runs; one that cannot be read
        // is not proven a child and is left alone.
        let Ok(stat) = fs::read(format!("/proc/{pid}/stat")) else {
            continue;
        };
        if stat_parent(&stat) == Some(keeper) {
            children.push(pid);
        }
    }
    Ok(children)
}

/// The parent pid in a `/proc/<pid>/stat` record, read as bytes after the
/// command name, which may itself hold spaces, ')' or non-UTF-8 bytes.
fn stat_parent(stat: &[u8]) -> Option<libc::pid_t> {
    let close = stat.iter().rposition(|byte| *byte == b')')?;
    let mut fields = stat[close + 1..]
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty());
    fields.next()?;
    std::str::from_utf8(fields.next()?).ok()?.parse().ok()
}

/// Whether this process's procfs numbers processes as its own system calls
/// do. `NStgid` in `/proc/self/status` lists this process's pid in every
/// namespace from the one that mounted this procfs down to its own: exactly
/// one entry, equal to our own pid, means they are the same namespace.
/// Anything else, a kernel without `NStgid` included, fails closed.
pub(crate) fn procfs_shares_our_pid_namespace() -> bool {
    let Ok(status) = fs::read_to_string("/proc/self/status") else {
        return false;
    };
    status
        .lines()
        .find_map(|line| line.strip_prefix("NStgid:"))
        .is_some_and(|ids| {
            let mut ids = ids.split_whitespace();
            ids.next().and_then(|id| id.parse::<u32>().ok()) == Some(std::process::id())
                && ids.next().is_none()
        })
}

/// The exit code that reports `status`: the server's own code, or, for a
/// server ended by a signal, the keeper ends by the same signal, so TermAl
/// sees the server's real end.
fn leave_like(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    let Some(signal) = status.signal() else {
        return 1;
    };
    // SAFETY: restores the default action and raises the signal on the
    // calling thread; for a fatal signal the process ends here.
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        let mut unblock: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut unblock);
        libc::sigaddset(&mut unblock, signal);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &unblock, std::ptr::null_mut());
        libc::raise(signal);
    }
    128 + signal
}
