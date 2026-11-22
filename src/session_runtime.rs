// Session runtime types + process kill utilities.
//
// Covers the per-session runtime handle types that `SessionRecord.runtime`
// can point to (`ClaudeRuntimeHandle`, `CodexRuntimeHandle`, `AcpRuntimeHandle`,
// and the shared `SharedCodexRuntime` + `SharedCodexSessionHandle`), the
// `SessionRuntime` enum that wraps them, the `KillableRuntime` projection
// used by stop/kill paths, the `DeferredStopCallback` + `RuntimeToken`
// types for deferred-callback replay semantics, and the platform-safe
// process-kill helpers (with a test-only forced-failure injection seam).
//
// Also carries the `CodexThreadActionContext` context record used by the
// thread-action dispatch entry points (fork/archive/unarchive/compact/
// rollback) and the `AcpAgent` enum that identifies which ACP agent a
// handle wraps.
//
// Extracted from state.rs into its own module so state.rs
// can stay focused on the StateInner state model and its impls.

use super::*;

/// Represents Codex thread action context.
pub(crate) struct CodexThreadActionContext {
    pub(crate) approval_policy: CodexApprovalPolicy,
    pub(crate) fast_mode: bool,
    pub(crate) model: String,
    pub(crate) model_options: Vec<SessionModelOption>,
    pub(crate) name: String,
    pub(crate) project_id: Option<String>,
    pub(crate) reasoning_effort: CodexReasoningEffort,
    pub(crate) sandbox_mode: CodexSandboxMode,
    /// The app-server that holds this session's thread.
    pub(crate) shared_codex_profile: SharedCodexProfile,
    pub(crate) thread_id: String,
    pub(crate) thread_state: Option<CodexThreadState>,
    pub(crate) workdir: String,
}

/// Defines the session runtime variants.
#[derive(Clone)]
pub(crate) enum SessionRuntime {
    None,
    Claude(ClaudeRuntimeHandle),
    Codex(CodexRuntimeHandle),
    Acp(AcpRuntimeHandle),
}

/// Represents the Claude runtime handle.
#[derive(Clone)]
pub(crate) struct ClaudeRuntimeHandle {
    pub(crate) runtime_id: String,
    pub(crate) input_tx: Sender<ClaudeRuntimeCommand>,
    pub(crate) process: Arc<SharedChild>,
    #[cfg(windows)]
    pub(crate) process_tree: Option<Arc<RuntimeProcessTree>>,
}

impl ClaudeRuntimeHandle {
    pub(crate) fn kill_for_locked_reset(&self) -> Result<()> {
        #[cfg(windows)]
        if let Some(tree) = &self.process_tree {
            if tree.is_confirmed() {
                return Ok(());
            }
            bail!("dedicated runtime reset requires off-lock cleanup preparation");
        }
        self.kill()
    }
    pub(crate) fn kill(&self) -> Result<()> {
        #[cfg(windows)]
        if let Some(tree) = &self.process_tree {
            return tree.terminate(&self.process, "Claude");
        }
        kill_child_process(&self.process, "Claude")
    }
}

/// Represents the Codex runtime handle.
#[derive(Clone)]
pub(crate) struct CodexRuntimeHandle {
    pub(crate) runtime_id: String,
    pub(crate) input_tx: Sender<CodexRuntimeCommand>,
    pub(crate) process: Arc<SharedChild>,
    pub(crate) shared_session: Option<SharedCodexSessionHandle>,
}

impl CodexRuntimeHandle {
    pub(crate) fn kill(&self) -> Result<()> {
        kill_child_process(&self.process, "Codex")
    }
}

/// Last time the shared Codex app-server produced ANY stdout line. Stamped by
/// the stdout reader thread (`src/codex.rs`), read by the response-timeout
/// handlers to tell a busy server from a wedged one: a server that spoke
/// during a request's wait window is alive and merely slow, so timing out that
/// one request must not tear down the runtime under every other session.
pub(crate) type SharedCodexStdoutActivityState = Arc<Mutex<std::time::Instant>>;

/// Represents shared Codex runtime.
#[derive(Clone)]
pub(crate) struct SharedCodexRuntime {
    pub(crate) runtime_id: String,
    pub(crate) input_tx: Sender<CodexRuntimeCommand>,
    pub(crate) process: Arc<SharedChild>,
    pub(crate) sessions: SharedCodexSessionMap,
    pub(crate) thread_sessions: SharedCodexThreadMap,
    pub(crate) stdout_activity: SharedCodexStdoutActivityState,
}

impl SharedCodexRuntime {
    /// Sends a graceful shutdown notification to the app-server, waits briefly
    /// for the process to exit, then escalates to a hard kill if necessary.
    pub(crate) fn kill(&self) -> Result<()> {
        // Attempt graceful shutdown by sending a `shutdown` notification.
        // The send may fail if the writer thread already exited — that is
        // fine, we will fall through to the hard kill.
        let _ = self
            .input_tx
            .send(CodexRuntimeCommand::JsonRpcNotification {
                method: "shutdown".to_owned(),
            });

        // Give the app-server a moment to exit cleanly before escalating.
        if wait_for_shared_child_exit_timeout(
            &self.process,
            Duration::from_secs(3),
            "shared Codex runtime",
        )?
        .is_some()
        {
            return Ok(());
        }

        kill_child_process(&self.process, "shared Codex runtime")
    }
}

/// Represents the shared Codex session handle.
#[derive(Clone)]
pub(crate) struct SharedCodexSessionHandle {
    pub(crate) runtime: SharedCodexRuntime,
    pub(crate) session_id: String,
}

impl SharedCodexSessionHandle {
    fn remove_locked_session(
        sessions: &mut HashMap<String, SharedCodexSessionState>,
        session_id: &str,
    ) -> Option<String> {
        sessions
            .remove(session_id)
            .and_then(|state| state.thread_id)
    }

    fn remove_thread_mapping(&self, removed_thread_id: Option<String>) {
        if let Some(thread_id) = removed_thread_id {
            self.runtime
                .thread_sessions
                .lock()
                .expect("shared Codex thread mutex poisoned")
                .remove(&thread_id);
        }
    }

    /// Releases this session's slot in the shared Codex runtime
    /// (`runtime.sessions`) and its `thread_id` → session mapping
    /// (`runtime.thread_sessions`) if one is bound. The underlying
    /// runtime subprocess keeps running for other sessions.
    pub(crate) fn detach(&self) {
        let removed_thread_id = {
            let mut sessions = self
                .runtime
                .sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            Self::remove_locked_session(&mut sessions, &self.session_id)
        };

        self.remove_thread_mapping(removed_thread_id);
    }

    /// Best-effort nonblocking variant used by natural completion cleanup.
    /// `turn/completed` handling owns `runtime.sessions` while applying the
    /// state transition; blocking here would self-deadlock before parent
    /// delegation result deltas are published.
    pub(crate) fn try_detach(&self) -> bool {
        let removed_thread_id = match self.runtime.sessions.try_lock() {
            Ok(mut sessions) => Self::remove_locked_session(&mut sessions, &self.session_id),
            Err(std::sync::TryLockError::WouldBlock) => return false,
            Err(std::sync::TryLockError::Poisoned(_)) => {
                panic!("shared Codex session mutex poisoned")
            }
        };

        self.remove_thread_mapping(removed_thread_id);
        true
    }

    fn detach_async(&self, context: &str) -> Result<()> {
        let shared_session = self.clone();
        let context = context.to_owned();
        std::thread::Builder::new()
            .name("termal-codex-detach".to_owned())
            .spawn(move || {
                shared_session.detach();
                trace_shared_codex_event(
                    "shared_session_detach_deferred_done",
                    "delegation/runtime_cleanup",
                    Some(&shared_session.session_id),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(context.as_str()),
                );
            })
            .map(|_| ())
            .map_err(|err| anyhow!("failed to spawn shared Codex detach cleanup: {err}"))
    }

    /// Sends an `interrupt` request to the shared Codex runtime for
    /// this session's currently active `(thread_id, turn_id)` pair,
    /// waits up to 10 s for the runtime's acknowledgement, and
    /// returns on success. No-ops when the session has no active
    /// thread or turn (a rare race where the turn finished just
    /// before the user hit stop).
    fn interrupt_turn(&self) -> Result<()> {
        let (thread_id, turn_id) = {
            let sessions = self
                .runtime
                .sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            let Some(state) = sessions.get(&self.session_id) else {
                return Ok(());
            };
            let Some(thread_id) = state.thread_id.clone() else {
                return Ok(());
            };
            let Some(turn_id) = state.turn_id.clone() else {
                return Ok(());
            };
            (thread_id, turn_id)
        };

        let (response_tx, response_rx) = mpsc::channel();
        self.runtime
            .input_tx
            .send(CodexRuntimeCommand::InterruptTurn {
                response_tx,
                thread_id,
                turn_id,
            })
            .map_err(|err| anyhow!("failed to queue Codex turn interrupt: {err}"))?;

        match response_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(detail)) => Err(anyhow!(detail)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(anyhow!("timed out waiting for Codex turn interrupt"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(anyhow!("Codex turn interrupt did not return a result"))
            }
        }
    }

    /// Convenience: interrupts the in-flight turn (if any), then
    /// detaches the session from the shared runtime regardless of
    /// whether the interrupt succeeded. Used on session kill paths
    /// where both steps are wanted atomically.
    fn interrupt_and_detach(&self) -> Result<()> {
        let result = self.interrupt_turn();
        self.detach();
        result
    }
}

/// Defines the ACP agent variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AcpAgent {
    Cursor,
    Gemini,
    OpenCode,
    Kimi,
}

impl AcpAgent {
    pub(crate) fn agent(self) -> Agent {
        match self {
            Self::Cursor => Agent::Cursor,
            Self::Gemini => Agent::Gemini,
            Self::OpenCode => Agent::OpenCode,
            Self::Kimi => Agent::Kimi,
        }
    }

    /// Builds the `std::process::Command` used to spawn this ACP
    /// agent. Cursor uses `cursor-agent acp`; the Gemini harness uses
    /// Antigravity's `agy acp`. Each adapter resolves its native executable
    /// before changing the working directory and reports missing setup.
    pub(crate) fn command(self, launch_options: AcpLaunchOptions) -> Result<Command> {
        match self {
            Self::Kimi => kimi_acp_command(resolve_kimi_executable()),
            Self::Cursor => {
                let exe = find_command_on_path("cursor-agent")
                    .ok_or_else(|| anyhow!("`cursor-agent` was not found on PATH"))?;
                let mut command = Command::new(exe);
                command.arg("acp");
                Ok(command)
            }
            Self::Gemini => gemini_acp_command(
                resolve_gemini_executable(),
                launch_options.gemini_approval_mode,
            ),
            Self::OpenCode => {
                let exe = resolve_opencode_executable()
                    .ok_or_else(|| anyhow!("`opencode` was not found on PATH"))?;
                let mut command = opencode_command(&exe);
                command.arg("acp");
                Ok(command)
            }
        }
    }

    pub(crate) fn label(self) -> &'static str {
        self.agent().name()
    }
}

/// Represents the ACP runtime handle.
#[derive(Clone)]
pub(crate) struct AcpRuntimeHandle {
    pub(crate) agent: AcpAgent,
    pub(crate) runtime_id: String,
    pub(crate) input_tx: Sender<AcpRuntimeCommand>,
    pub(crate) process: Arc<SharedChild>,
    #[cfg(windows)]
    pub(crate) process_tree: Option<Arc<RuntimeProcessTree>>,
    pub(crate) turn_lifecycle: AcpTurnLifecycle,
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct TestAcpRuntimeOverride {
    pub(crate) agent: AcpAgent,
    pub(crate) runtime: AcpRuntimeHandle,
}

impl AcpRuntimeHandle {
    pub(crate) fn kill_for_locked_reset(&self) -> Result<()> {
        #[cfg(windows)]
        if let Some(tree) = &self.process_tree {
            if tree.is_confirmed() {
                return Ok(());
            }
            bail!("dedicated runtime reset requires off-lock cleanup preparation");
        }
        self.kill()
    }
    pub(crate) fn kill(&self) -> Result<()> {
        #[cfg(windows)]
        if let Some(tree) = &self.process_tree {
            return tree.terminate(&self.process, self.agent.label());
        }
        kill_child_process(&self.process, self.agent.label())
    }

    /// Queues cancellation for the active ACP turn, waits for its prompt
    /// response to settle, then terminates the now-detached subprocess.
    ///
    /// Local process termination never proves that the agent-side session is
    /// invalid, so this path always preserves the external session id. The
    /// typed resume classifier owns any later continuity invalidation.
    pub(crate) fn stop(&self) -> Result<()> {
        self.stop_with_grace(ACP_CANCEL_GRACE)
    }

    pub(crate) fn stop_with_grace(&self, grace: Duration) -> Result<()> {
        if let Err(err) = self.input_tx.send(AcpRuntimeCommand::Cancel) {
            eprintln!(
                "session cleanup warning> failed to queue {} ACP cancellation; \
                 falling back to process termination: {err}",
                self.agent.label()
            );
        } else if !wait_for_acp_turn_settle(&self.turn_lifecycle, grace) {
            eprintln!(
                "session cleanup warning> {} ACP prompt did not settle within the \
                 cancellation grace; terminating the process while preserving its \
                 external session id",
                self.agent.label()
            );
        }
        self.kill()?;
        Ok(())
    }
}

/// Defines the killable runtime variants.
#[derive(Clone)]
pub(crate) enum KillableRuntime {
    Claude(ClaudeRuntimeHandle),
    Codex(CodexRuntimeHandle),
    Acp(AcpRuntimeHandle),
}

impl KillableRuntime {
    /// Stops failure is best effort.
    pub(crate) fn stop_failure_is_best_effort(&self) -> bool {
        matches!(self, Self::Codex(handle) if handle.shared_session.is_some())
    }

    /// Confirms whether the owned subprocess has exited. Cleanup callers must
    /// retain a dedicated runtime handle when termination failed and this
    /// probe cannot prove exit, otherwise the stale MCP-bearing process loses
    /// its only retry handle.
    pub(crate) fn process_has_exited(&self) -> Result<bool> {
        #[cfg(windows)]
        match self {
            Self::Claude(handle) if handle.process_tree.is_some() => {
                return handle.process_tree.as_ref().unwrap().has_exited();
            }
            Self::Acp(handle) if handle.process_tree.is_some() => {
                return handle.process_tree.as_ref().unwrap().has_exited();
            }
            _ => {}
        }
        match self {
            Self::Claude(handle) => shared_child_has_exited(&handle.process, "Claude runtime"),
            Self::Codex(handle) => shared_child_has_exited(&handle.process, "Codex runtime"),
            Self::Acp(handle) => shared_child_has_exited(
                &handle.process,
                &format!("{} runtime", handle.agent.label()),
            ),
        }
    }
}

/// Shuts down removed runtime.
pub(crate) fn shutdown_removed_runtime(runtime: KillableRuntime, context: &str) -> Result<()> {
    match runtime {
        KillableRuntime::Codex(handle) => {
            if let Some(shared_session) = &handle.shared_session {
                match shared_session.interrupt_and_detach() {
                    Ok(()) => Ok(()),
                    Err(interrupt_err) => {
                        if shared_child_has_exited(&handle.process, "shared Codex runtime")? {
                            Err(anyhow!(
                                "shared Codex runtime had already exited while removing {context}: {interrupt_err:#}"
                            ))
                        } else {
                            Err(anyhow!(
                                "failed to interrupt shared Codex turn for {context}: {interrupt_err:#}"
                            ))
                        }
                    }
                }
            } else {
                handle
                    .kill()
                    .with_context(|| format!("failed to kill Codex runtime for {context}"))
            }
        }
        KillableRuntime::Claude(handle) => handle
            .kill()
            .with_context(|| format!("failed to kill Claude runtime for {context}")),
        KillableRuntime::Acp(handle) => handle.kill().with_context(|| {
            format!(
                "failed to kill {} runtime for {context}",
                handle.agent.label()
            )
        }),
    }
}

/// Stops a user-visible runtime. OpenCode and Kimi get a bounded graceful ACP
/// cancel so their resumable sessions can settle before local process teardown; the
/// existing Cursor and Gemini stop contract remains immediate termination.
pub(crate) fn shutdown_stopped_runtime(runtime: KillableRuntime, context: &str) -> Result<()> {
    match runtime {
        KillableRuntime::Acp(handle)
            if matches!(handle.agent, AcpAgent::OpenCode | AcpAgent::Kimi) =>
        {
            handle.stop().with_context(|| {
                format!(
                    "failed to stop {} runtime for {context}",
                    handle.agent.label()
                )
            })
        }
        runtime => shutdown_removed_runtime(runtime, context),
    }
}

/// Detaches a naturally terminal delegation child without interrupting an
/// already-finished shared Codex turn.
pub(crate) fn shutdown_terminal_delegation_child_runtime(
    runtime: KillableRuntime,
    context: &str,
) -> Result<()> {
    match runtime {
        KillableRuntime::Codex(handle) => {
            if let Some(shared_session) = &handle.shared_session {
                if !shared_session.try_detach() {
                    trace_shared_codex_event(
                        "shared_session_detach_deferred",
                        "delegation/runtime_cleanup",
                        Some(&shared_session.session_id),
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some("sessions_locked"),
                    );
                    shared_session.detach_async(context)?;
                }
                Ok(())
            } else {
                handle
                    .kill()
                    .with_context(|| format!("failed to kill Codex runtime for {context}"))
            }
        }
        runtime => shutdown_removed_runtime(runtime, context),
    }
}

/// Defines the deferred stop callback variants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DeferredStopCallback {
    /// Delivery is durable, but the Stop owner has not resolved yet.
    TurnHandoff {
        active_turn_generation: u64,
        dispatch: DeferredTurnHandoff,
    },
    /// `fail_turn_if_runtime_matches` was called.
    TurnFailed {
        active_turn_generation: u64,
        message: String,
    },
    /// `mark_turn_error_if_runtime_matches` was called.
    TurnError {
        active_turn_generation: u64,
        message: String,
    },
    /// `finish_turn_ok_if_runtime_matches` was called.
    TurnCompleted { active_turn_generation: u64 },
    /// `handle_runtime_exit_if_matches` was called.
    RuntimeExited {
        active_turn_generation: u64,
        message: Option<String>,
    },
}

impl DeferredStopCallback {
    pub(crate) fn active_turn_generation(&self) -> u64 {
        match self {
            Self::TurnHandoff {
                active_turn_generation,
                ..
            }
            | Self::TurnFailed {
                active_turn_generation,
                ..
            }
            | Self::TurnError {
                active_turn_generation,
                ..
            }
            | Self::TurnCompleted {
                active_turn_generation,
            }
            | Self::RuntimeExited {
                active_turn_generation,
                ..
            } => *active_turn_generation,
        }
    }
}

/// Defines the runtime token variants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RuntimeToken {
    Claude(String),
    Codex(String),
    Acp(String),
}

impl SessionRuntime {
    #[cfg(windows)]
    pub(crate) fn dedicated_tree_owner(
        &self,
    ) -> Option<(Arc<RuntimeProcessTree>, Arc<SharedChild>, &'static str)> {
        match self {
            Self::Claude(handle) => handle
                .process_tree
                .as_ref()
                .map(|tree| (tree.clone(), handle.process.clone(), "Claude")),
            Self::Acp(handle) => handle
                .process_tree
                .as_ref()
                .map(|tree| (tree.clone(), handle.process.clone(), handle.agent.label())),
            _ => None,
        }
    }
    pub(crate) fn dedicated_cleanup_failure(&self) -> Option<String> {
        #[cfg(windows)]
        match self {
            Self::Claude(handle) => {
                return handle
                    .process_tree
                    .as_ref()
                    .and_then(|tree| tree.cleanup_failure());
            }
            Self::Acp(handle) => {
                return handle
                    .process_tree
                    .as_ref()
                    .and_then(|tree| tree.cleanup_failure());
            }
            _ => {}
        }
        None
    }

    /// Returns a `RuntimeToken` identifying the current runtime, or
    /// `None` when `SessionRuntime::None`. Used by the
    /// `_if_runtime_matches` guard wrappers in `turn_lifecycle.rs`
    /// to drop stray events from torn-down runtimes — see that file
    /// for the staleness pattern.
    pub(crate) fn runtime_token(&self) -> Option<RuntimeToken> {
        match self {
            Self::Claude(handle) => Some(RuntimeToken::Claude(handle.runtime_id.clone())),
            Self::Codex(handle) => Some(RuntimeToken::Codex(handle.runtime_id.clone())),
            Self::Acp(handle) => Some(RuntimeToken::Acp(handle.runtime_id.clone())),
            Self::None => None,
        }
    }

    /// Returns whether runtime token.
    pub(crate) fn matches_runtime_token(&self, token: &RuntimeToken) -> bool {
        match (self, token) {
            (Self::Claude(handle), RuntimeToken::Claude(runtime_id)) => {
                handle.runtime_id == *runtime_id
            }
            (Self::Codex(handle), RuntimeToken::Codex(runtime_id)) => {
                handle.runtime_id == *runtime_id
            }
            (Self::Acp(handle), RuntimeToken::Acp(runtime_id)) => handle.runtime_id == *runtime_id,
            _ => false,
        }
    }
}

/// Represents forced kill child process failure.
#[cfg(test)]
#[derive(Clone)]
struct ForcedKillChildProcessFailure {
    label: String,
    process_ptr: usize,
    remaining_failures: Option<usize>,
}

/// Returns the forced kill child process failure.
#[cfg(test)]
fn forced_kill_child_process_failure() -> &'static Mutex<Option<ForcedKillChildProcessFailure>> {
    static FORCED_KILL_CHILD_PROCESS_FAILURE: std::sync::OnceLock<
        Mutex<Option<ForcedKillChildProcessFailure>>,
    > = std::sync::OnceLock::new();
    FORCED_KILL_CHILD_PROCESS_FAILURE.get_or_init(|| Mutex::new(None))
}

/// Sets test kill child process failure.
#[cfg(test)]
pub(crate) fn set_test_kill_child_process_failure(
    label: Option<&str>,
    process: Option<&Arc<SharedChild>>,
) {
    *forced_kill_child_process_failure()
        .lock()
        .expect("forced kill-child-process failure mutex poisoned") = match (label, process) {
        (Some(label), Some(process)) => Some(ForcedKillChildProcessFailure {
            label: label.to_owned(),
            process_ptr: Arc::as_ptr(process) as usize,
            remaining_failures: None,
        }),
        _ => None,
    };
}

#[cfg(test)]
pub(crate) fn set_test_kill_child_process_failure_count(
    label: &str,
    process: &Arc<SharedChild>,
    remaining_failures: usize,
) {
    *forced_kill_child_process_failure()
        .lock()
        .expect("forced kill-child-process failure mutex poisoned") =
        Some(ForcedKillChildProcessFailure {
            label: label.to_owned(),
            process_ptr: Arc::as_ptr(process) as usize,
            remaining_failures: Some(remaining_failures),
        });
}

/// Kills child process.
pub(crate) fn kill_child_process(process: &Arc<SharedChild>, label: &str) -> Result<()> {
    #[cfg(test)]
    {
        let mut forced_failure = forced_kill_child_process_failure()
            .lock()
            .expect("forced kill-child-process failure mutex poisoned");
        let matches_forced_failure = forced_failure.as_ref().is_some_and(|failure| {
            failure.label == label && failure.process_ptr == Arc::as_ptr(process) as usize
        });
        if matches_forced_failure {
            let should_clear = forced_failure
                .as_mut()
                .and_then(|failure| failure.remaining_failures.as_mut())
                .is_some_and(|remaining_failures| {
                    *remaining_failures = remaining_failures.saturating_sub(1);
                    *remaining_failures == 0
                });
            if should_clear {
                *forced_failure = None;
            }
            return Err(anyhow!("forced {label} kill failure"));
        }
    }

    if wait_for_shared_child_exit_timeout(process, Duration::from_millis(50), label)?.is_some() {
        return Ok(());
    }

    match process.kill() {
        Ok(()) => Ok(()),
        Err(err) => {
            if wait_for_shared_child_exit_timeout(process, Duration::from_millis(50), label)?
                .is_some()
            {
                Ok(())
            } else {
                Err(err).with_context(|| format!("failed to terminate {label} process"))
            }
        }
    }
}

pub(crate) fn shared_child_has_exited(process: &Arc<SharedChild>, label: &str) -> Result<bool> {
    match process.try_wait() {
        Ok(Some(_)) => Ok(true),
        Ok(None) => Ok(false),
        Err(err) => Err(anyhow!("failed checking {label} process status: {err}")),
    }
}

/// Handles wait for shared child exit timeout.
pub(crate) fn wait_for_shared_child_exit_timeout(
    process: &Arc<SharedChild>,
    timeout: Duration,
    label: &str,
) -> Result<Option<std::process::ExitStatus>> {
    match process.try_wait() {
        Ok(Some(status)) => return Ok(Some(status)),
        Ok(None) => {}
        Err(err) => return Err(anyhow!("failed waiting for {label} process: {err}")),
    }

    let wait_process = process.clone();
    let (status_tx, status_rx) = mpsc::sync_channel(1);
    // If the timeout elapses, callers either terminate the process immediately or continue with
    // a long-lived shared child. The waiter is detached so we never block the caller thread.
    std::thread::spawn(move || {
        let _ = status_tx.send(wait_process.wait());
    });

    match status_rx.recv_timeout(timeout) {
        Ok(Ok(status)) => Ok(Some(status)),
        Ok(Err(err)) => Err(anyhow!("failed waiting for {label} process: {err}")),
        Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(anyhow!(
            "failed waiting for {label} process: wait thread disconnected"
        )),
    }
}
