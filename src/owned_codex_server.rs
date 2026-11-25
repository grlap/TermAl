// Owns a delegation's dedicated Codex app-server as a process tree: how it is
// launched so that the tree stays addressable (Linux: under a keeper, a child
// subreaper that ends the tree, see `codex_keeper.rs`; other Unix: a session
// of its own; Windows: a kill-on-close job), the registry that lets runtime
// lookups recognise it, the Codex home its launches reuse, and ending the
// whole tree, on a cleanup worker, once the last handle to the server drops
// or a release that could not run ends it explicitly.
//
// Does not own the shared per-profile app-servers, which serve many sessions
// and keep their own lifetime, nor the JSON-RPC wire, which `codex.rs` runs
// for both kinds of server. Ending signals only the process TermAl started;
// on Linux the keeper signals only its own children.

use super::*;

/// Whom an app-server serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CodexServerOwnership {
    /// Every root session of one profile, from its shared slot.
    Shared,
    /// One delegation child; the server's tree ends with the child's runtime.
    Delegation,
}

impl CodexServerOwnership {
    pub(crate) fn of_record(record: &SessionRecord) -> Self {
        if record.session.parent_delegation_id.is_some() {
            Self::Delegation
        } else {
            Self::Shared
        }
    }
}

/// What runtime lookups need of a live delegation-owned app-server.
pub(crate) struct OwnedCodexServerEntry {
    /// The profile whose Codex home the server uses.
    pub(crate) profile: SharedCodexProfile,
    /// When the server last wrote to stdout (busy-versus-wedged checks).
    pub(crate) stdout_activity: SharedCodexStdoutActivityState,
    /// The server itself, so that a failure path can end its tree.
    server: std::sync::Weak<OwnedCodexServer>,
}

/// One state's delegation-owned app-servers.
#[derive(Default)]
pub(crate) struct OwnedCodexServers {
    /// Live servers by runtime id.
    entries: Mutex<HashMap<String, OwnedCodexServerEntry>>,
    /// The Codex home of each profile as last prepared. Every preparation,
    /// for a shared or an owned launch, runs under this lock, so two never
    /// write the same home at once; owned launches reuse a prepared home.
    prepared_homes: Mutex<Vec<(SharedCodexProfile, PathBuf)>>,
    /// The worker that ends trees; started before the first owned launch, so
    /// ending a server never depends on creating a thread.
    cleanup: Mutex<Option<Sender<Arc<SharedChild>>>>,
    /// Owned servers from launch until the process TermAl holds for them (on
    /// Linux the keeper) has exited, as their runtime's waiter observes. A
    /// failure path that finds an id here leaves the ending to its owner and
    /// never kills that process outright, even after `end` has removed the
    /// server from `entries` and the worker has stopped waiting for it.
    unfinished: Mutex<HashSet<String>>,
}

pub(crate) type OwnedCodexServerRegistry = Arc<OwnedCodexServers>;

impl OwnedCodexServers {
    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<String, OwnedCodexServerEntry>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The Codex home of `profile` for a launch. A shared launch seeds it
    /// again, as it always has; an owned launch reuses the last preparation.
    /// Both run under one lock, so preparations of a home never overlap.
    pub(crate) fn prepared_home(
        &self,
        profile: SharedCodexProfile,
        ownership: CodexServerOwnership,
        prepare: impl FnOnce() -> Result<PathBuf>,
    ) -> Result<PathBuf> {
        let mut homes = self
            .prepared_homes
            .lock()
            .expect("Codex home preparation mutex poisoned");
        let prepared = homes.iter().position(|(prepared, _)| *prepared == profile);
        if ownership == CodexServerOwnership::Delegation {
            if let Some(index) = prepared {
                return Ok(homes[index].1.clone());
            }
        }
        let home = prepare()?;
        match prepared {
            Some(index) => homes[index].1 = home.clone(),
            None => homes.push((profile, home.clone())),
        }
        Ok(home)
    }

    pub(crate) fn has_unfinished(&self) -> bool {
        !lock_unfinished(&self.unfinished).is_empty()
    }

    pub(crate) fn invalidate_prepared_homes(&self) {
        self.prepared_homes
            .lock()
            .expect("Codex home preparation mutex poisoned")
            .clear();
    }

    /// Starts the cleanup worker unless it is running. Called before an
    /// owned server is launched, so a launch fails rather than leaving a
    /// server that nothing could end.
    pub(crate) fn ensure_cleanup_worker(&self) -> Result<()> {
        let mut cleanup = self
            .cleanup
            .lock()
            .expect("owned Codex cleanup mutex poisoned");
        if cleanup.is_some() {
            return Ok(());
        }
        let (sender, receiver) = mpsc::channel::<Arc<SharedChild>>();
        std::thread::Builder::new()
            .name("termal-owned-codex-cleanup".to_owned())
            .spawn(move || {
                // Ends when the registry, and with it the sender, is gone.
                for process in receiver {
                    end_owned_codex_tree_reporting(&process);
                }
            })
            .context("failed to start the delegation Codex cleanup worker")?;
        *cleanup = Some(sender);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn live_server_count(&self) -> usize {
        self.entries().len()
    }

    fn queue_cleanup(&self, process: &Arc<SharedChild>) {
        let queued = self
            .cleanup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(|sender| sender.send(process.clone()).is_ok());
        if !queued {
            // The worker is gone (it can only have panicked): end the tree
            // here rather than lose it.
            end_owned_codex_tree_reporting(process);
        }
    }
}

/// The process TermAl holds for the runtime `runtime_id` has exited: an owned
/// server's ending is over, and nothing about it is left to protect.
pub(crate) fn owned_codex_server_exited(state: &AppState, runtime_id: &str) {
    lock_unfinished(&state.owned_codex_servers.unfinished).remove(runtime_id);
}

fn lock_unfinished(
    unfinished: &Mutex<HashSet<String>>,
) -> std::sync::MutexGuard<'_, HashSet<String>> {
    unfinished
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether `runtime_id` is a delegation's own server whose ending has not
/// finished: such a server ends only through its owner.
pub(crate) fn is_unfinished_owned_codex_server(state: &AppState, runtime_id: &str) -> bool {
    lock_unfinished(&state.owned_codex_servers.unfinished).contains(runtime_id)
}

pub(crate) fn owned_codex_server_profile(
    state: &AppState,
    runtime_id: &str,
) -> Option<SharedCodexProfile> {
    state
        .owned_codex_servers
        .entries()
        .get(runtime_id)
        .map(|entry| entry.profile)
}

/// How long the owned server `runtime_id` has been silent on stdout, or
/// `None` when no live owned server has that id.
pub(crate) fn owned_codex_server_stdout_silence(
    state: &AppState,
    runtime_id: &str,
) -> Option<Duration> {
    state
        .owned_codex_servers
        .entries()
        .get(runtime_id)
        .map(|entry| {
            entry
                .stdout_activity
                .lock()
                .expect("shared Codex stdout activity mutex poisoned")
                .elapsed()
        })
}

/// Ends the tree of the owned server `runtime_id` through its own ending,
/// so no failure path kills the process TermAl holds outright. Nothing to
/// do when that ending has already begun.
pub(crate) fn end_owned_codex_server(state: &AppState, runtime_id: &str) {
    let server = state
        .owned_codex_servers
        .entries()
        .get(runtime_id)
        .and_then(|entry| entry.server.upgrade());
    // Ended outside the registry lock, which `end` takes itself.
    if let Some(server) = server {
        server.end();
    }
}

/// What TermAl holds while a delegation's tree should live: on Linux the
/// write end of the keeper's control pipe, nothing on other hosts.
#[cfg(target_os = "linux")]
type OwnedCodexServerControl = crate::codex_keeper::CodexKeeperControl;
#[cfg(not(target_os = "linux"))]
type OwnedCodexServerControl = ();

/// Under test, the states (by default workdir) whose host is treated as
/// having its /proc in another PID namespace.
#[cfg(all(test, target_os = "linux"))]
pub(crate) static TEST_FOREIGN_PROCFS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(Default::default);

/// A delegation server's launch, prepared on its app-server command.
pub(crate) struct OwnedCodexServerLaunch {
    control: OwnedCodexServerControl,
    /// The keeper's end of its control pipe, open until the keeper has
    /// been spawned.
    #[cfg(target_os = "linux")]
    keeper_end: std::os::fd::OwnedFd,
}

impl OwnedCodexServerLaunch {
    /// Gives the app-server's tree an identity of its own before it runs.
    /// Linux: `command` becomes the keeper that starts the app-server and
    /// ends its tree. Other Unix: the server leads a session of its own.
    /// Windows: every app-server already starts suspended into a
    /// kill-on-close job (`spawn_dedicated_runtime`), whose last handle its
    /// waiter drops when the root exits; descendants cannot leave that job.
    pub(crate) fn prepare(state: &AppState, command: &mut Command) -> Result<Self> {
        // Read only under test, and only on Linux.
        let _ = state;
        #[cfg(target_os = "linux")]
        {
            // The keeper would refuse to start a server whose tree it could
            // not end; refused here, the delegation fails at once and says
            // why.
            let procfs_names_our_processes = crate::codex_keeper::procfs_shares_our_pid_namespace();
            #[cfg(test)]
            let procfs_names_our_processes = procfs_names_our_processes
                && !TEST_FOREIGN_PROCFS
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .contains(&state.default_workdir);
            if !procfs_names_our_processes {
                bail!(
                    "this host's /proc is not in TermAl's own PID namespace, so a Codex delegation's process tree could not be ended; its app-server was not started"
                );
            }
            let launch = crate::codex_keeper::codex_keeper_launch(command)?;
            *command = launch.command;
            Ok(Self {
                control: launch.control,
                keeper_end: launch.keeper_end,
            })
        }
        #[cfg(all(unix, not(target_os = "linux")))]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: only an async-signal-safe call runs between fork and exec.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            Ok(Self { control: () })
        }
        #[cfg(windows)]
        {
            let _ = command;
            Ok(Self { control: () })
        }
    }

    /// The launch has started its process: the keeper's end of the pipe
    /// closes here, and what keeps the tree alive goes to its owner.
    fn started(self) -> OwnedCodexServerControl {
        #[cfg(target_os = "linux")]
        drop(self.keeper_end);
        self.control
    }
}

/// Held by every clone of a delegation-owned `SharedCodexRuntime`. The tree
/// ends once: at the last drop, or earlier through `end`.
pub(crate) struct OwnedCodexServer {
    runtime_id: String,
    process: Arc<SharedChild>,
    registry: OwnedCodexServerRegistry,
    control: Mutex<Option<OwnedCodexServerControl>>,
    ended: AtomicBool,
}

impl OwnedCodexServer {
    /// Takes ownership of a just-started server, before anything else about
    /// the runtime can fail: every later failure then ends its tree through
    /// this owner.
    pub(crate) fn register(
        state: &AppState,
        runtime_id: &str,
        process: &Arc<SharedChild>,
        stdout_activity: &SharedCodexStdoutActivityState,
        profile: SharedCodexProfile,
        launch: OwnedCodexServerLaunch,
    ) -> Arc<Self> {
        let server = Arc::new(Self {
            runtime_id: runtime_id.to_owned(),
            process: process.clone(),
            registry: state.owned_codex_servers.clone(),
            control: Mutex::new(Some(launch.started())),
            ended: AtomicBool::new(false),
        });
        lock_unfinished(&state.owned_codex_servers.unfinished).insert(runtime_id.to_owned());
        state.owned_codex_servers.entries().insert(
            runtime_id.to_owned(),
            OwnedCodexServerEntry {
                profile,
                stdout_activity: stdout_activity.clone(),
                server: Arc::downgrade(&server),
            },
        );
        server
    }

    /// Ends the server's tree now, even while handles to it remain: a
    /// release that could not run still holds one, and must not keep the
    /// server and its tools alive. Later archive work goes to the shared
    /// server of the same Codex home. Runs at most once, and only queues the
    /// work, so it is safe under the state lock.
    pub(crate) fn end(&self) {
        if self.ended.swap(true, Ordering::AcqRel) {
            return;
        }
        // Closing what keeps the tree alive: on Linux this starts the
        // keeper's ending of the tree.
        let _ = self
            .control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.registry.entries().remove(&self.runtime_id);
        self.registry.queue_cleanup(&self.process);
    }

    /// Closes the keeper's control pipe as TermAl's own exit would, without
    /// anything else `end` does.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn close_control_as_if_termal_exited(&self) {
        drop(self.control.lock().unwrap().take());
    }
}

impl Drop for OwnedCodexServer {
    fn drop(&mut self) {
        self.end();
    }
}

const OWNED_CODEX_LABEL: &str = "delegation Codex app-server";

/// How long the worker awaits the keeper's ending of the tree before it
/// reports the ending as still in progress. The keeper is never killed.
#[cfg(target_os = "linux")]
const KEEPER_END_REPORT_AFTER: Duration = Duration::from_secs(30);

/// Awaits the keeper's own ending of the tree for at most `wait`. A keeper
/// still running then is reported and left to finish, never killed: a child
/// stuck in uninterruptible sleep holds the keeper in its drain, and only the
/// keeper, a subreaper, can end that child's descendants once it leaves that
/// state. Killing it would let them re-parent to init and outlive the
/// delegation. The runtime's own waiter reaps the keeper whenever it exits.
#[cfg(target_os = "linux")]
pub(crate) fn end_kept_tree_within(process: &Arc<SharedChild>, wait: Duration) -> Result<()> {
    if wait_for_shared_child_exit_timeout(process, wait, OWNED_CODEX_LABEL)?.is_some() {
        return Ok(());
    }
    bail!(
        "its keeper is still ending the tree after {}s and is left to finish it; only the keeper can end what remains",
        wait.as_secs()
    );
}

fn end_owned_codex_tree_reporting(process: &Arc<SharedChild>) {
    if let Err(error) = end_owned_codex_tree(process) {
        eprintln!(
            "[termal] failed to end {OWNED_CODEX_LABEL} {}: {error:#}",
            process.id()
        );
    }
}

/// Ends the server and what this platform can prove is its tree. Linux: the
/// process TermAl holds is the keeper, which ends the server and every
/// descendant once `end` has closed its control pipe; it is awaited, never
/// killed. Windows: ending the root closes its kill-on-close job. Other Unix
/// hosts end only the server itself: no raw pid or process group is
/// signalled.
fn end_owned_codex_tree(process: &Arc<SharedChild>) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        end_kept_tree_within(process, KEEPER_END_REPORT_AFTER)
    }
    #[cfg(not(target_os = "linux"))]
    {
        // A reaped root has already released its tree.
        if process
            .try_wait()
            .with_context(|| format!("failed checking {OWNED_CODEX_LABEL}"))?
            .is_some()
        {
            return Ok(());
        }
        kill_child_process(process, OWNED_CODEX_LABEL)
    }
}
