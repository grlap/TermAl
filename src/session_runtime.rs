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
// Extracted from state.rs into its own `include!()` fragment so state.rs
// can stay focused on the StateInner state model and its impls.

/// Represents Codex thread action context.
struct CodexThreadActionContext {
    approval_policy: CodexApprovalPolicy,
    model: String,
    model_options: Vec<SessionModelOption>,
    name: String,
    project_id: Option<String>,
    reasoning_effort: CodexReasoningEffort,
    sandbox_mode: CodexSandboxMode,
    thread_id: String,
    thread_state: Option<CodexThreadState>,
    workdir: String,
}

/// Defines the session runtime variants.
#[derive(Clone)]
enum SessionRuntime {
    None,
    Claude(ClaudeRuntimeHandle),
    Codex(CodexRuntimeHandle),
    Acp(AcpRuntimeHandle),
}

/// Represents the Claude runtime handle.
#[derive(Clone)]
struct ClaudeRuntimeHandle {
    runtime_id: String,
    input_tx: Sender<ClaudeRuntimeCommand>,
    process: Arc<SharedChild>,
}

impl ClaudeRuntimeHandle {
    fn kill(&self) -> Result<()> {
        kill_child_process(&self.process, "Claude")
    }
}

/// Represents the Codex runtime handle.
#[derive(Clone)]
struct CodexRuntimeHandle {
    runtime_id: String,
    input_tx: Sender<CodexRuntimeCommand>,
    process: Arc<SharedChild>,
    shared_session: Option<SharedCodexSessionHandle>,
}

impl CodexRuntimeHandle {
    fn kill(&self) -> Result<()> {
        kill_child_process(&self.process, "Codex")
    }
}

/// Represents shared Codex runtime.
#[derive(Clone)]
struct SharedCodexRuntime {
    runtime_id: String,
    input_tx: Sender<CodexRuntimeCommand>,
    process: Arc<SharedChild>,
    sessions: SharedCodexSessionMap,
    thread_sessions: SharedCodexThreadMap,
}

impl SharedCodexRuntime {
    /// Sends a graceful shutdown notification to the app-server, waits briefly
    /// for the process to exit, then escalates to a hard kill if necessary.
    fn kill(&self) -> Result<()> {
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
struct SharedCodexSessionHandle {
    runtime: SharedCodexRuntime,
    session_id: String,
}

impl SharedCodexSessionHandle {
    /// Releases this session's slot in the shared Codex runtime
    /// (`runtime.sessions`) and its `thread_id` → session mapping
    /// (`runtime.thread_sessions`) if one is bound. The underlying
    /// runtime subprocess keeps running for other sessions.
    fn detach(&self) {
        let removed_thread_id = {
            let mut sessions = self
                .runtime
                .sessions
                .lock()
                .expect("shared Codex session mutex poisoned");
            sessions
                .remove(&self.session_id)
                .and_then(|state| state.thread_id)
        };

        if let Some(thread_id) = removed_thread_id {
            self.runtime
                .thread_sessions
                .lock()
                .expect("shared Codex thread mutex poisoned")
                .remove(&thread_id);
        }
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
enum AcpAgent {
    Cursor,
    Gemini,
}

impl AcpAgent {
    fn agent(self) -> Agent {
        match self {
            Self::Cursor => Agent::Cursor,
            Self::Gemini => Agent::Gemini,
        }
    }

    /// Builds the `std::process::Command` used to spawn this ACP
    /// agent. Cursor is invoked as `cursor-agent acp`; Gemini is
    /// invoked as `gemini --acp [--approval-mode ...]`. The binary
    /// must be on `PATH` — absent binary returns a friendly error
    /// rather than a cryptic "No such file" from the OS.
    fn command(self, launch_options: AcpLaunchOptions) -> Result<Command> {
        match self {
            Self::Cursor => {
                let exe = find_command_on_path("cursor-agent")
                    .ok_or_else(|| anyhow!("`cursor-agent` was not found on PATH"))?;
                let mut command = Command::new(exe);
                command.arg("acp");
                Ok(command)
            }
            Self::Gemini => {
                let exe = find_command_on_path("gemini")
                    .ok_or_else(|| anyhow!("`gemini` was not found on PATH"))?;
                let mut command = Command::new(exe);
                command.arg("--acp");
                if let Some(approval_mode) = launch_options.gemini_approval_mode {
                    command.args(["--approval-mode", approval_mode.as_cli_value()]);
                }
                Ok(command)
            }
        }
    }

    fn label(self) -> &'static str {
        self.agent().name()
    }
}

/// Represents the ACP runtime handle.
#[derive(Clone)]
struct AcpRuntimeHandle {
    agent: AcpAgent,
    runtime_id: String,
    input_tx: Sender<AcpRuntimeCommand>,
    process: Arc<SharedChild>,
}

impl AcpRuntimeHandle {
    fn kill(&self) -> Result<()> {
        kill_child_process(&self.process, self.agent.label())
    }
}

/// Defines the killable runtime variants.
#[derive(Clone)]
enum KillableRuntime {
    Claude(ClaudeRuntimeHandle),
    Codex(CodexRuntimeHandle),
    Acp(AcpRuntimeHandle),
}

impl KillableRuntime {
    /// Stops failure is best effort.
    fn stop_failure_is_best_effort(&self) -> bool {
        matches!(self, Self::Codex(handle) if handle.shared_session.is_some())
    }
}

/// Shuts down removed runtime.
fn shutdown_removed_runtime(runtime: KillableRuntime, context: &str) -> Result<()> {
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

/// Defines the deferred stop callback variants.
#[derive(Clone, Debug, PartialEq, Eq)]
enum DeferredStopCallback {
    /// `fail_turn_if_runtime_matches` was called.
    TurnFailed(String),
    /// `mark_turn_error_if_runtime_matches` was called.
    TurnError(String),
    /// `finish_turn_ok_if_runtime_matches` was called.
    TurnCompleted,
    /// `handle_runtime_exit_if_matches` was called.
    RuntimeExited(Option<String>),
}

/// Defines the runtime token variants.
#[derive(Clone)]
enum RuntimeToken {
    Claude(String),
    Codex(String),
    Acp(String),
}

impl SessionRuntime {
    /// Returns a `RuntimeToken` identifying the current runtime, or
    /// `None` when `SessionRuntime::None`. Used by the
    /// `_if_runtime_matches` guard wrappers in `turn_lifecycle.rs`
    /// to drop stray events from torn-down runtimes — see that file
    /// for the staleness pattern.
    fn runtime_token(&self) -> Option<RuntimeToken> {
        match self {
            Self::Claude(handle) => Some(RuntimeToken::Claude(handle.runtime_id.clone())),
            Self::Codex(handle) => Some(RuntimeToken::Codex(handle.runtime_id.clone())),
            Self::Acp(handle) => Some(RuntimeToken::Acp(handle.runtime_id.clone())),
            Self::None => None,
        }
    }

    /// Returns whether runtime token.
    fn matches_runtime_token(&self, token: &RuntimeToken) -> bool {
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
fn set_test_kill_child_process_failure(label: Option<&str>, process: Option<&Arc<SharedChild>>) {
    *forced_kill_child_process_failure()
        .lock()
        .expect("forced kill-child-process failure mutex poisoned") = match (label, process) {
        (Some(label), Some(process)) => Some(ForcedKillChildProcessFailure {
            label: label.to_owned(),
            process_ptr: Arc::as_ptr(process) as usize,
        }),
        _ => None,
    };
}

/// Kills child process.
fn kill_child_process(process: &Arc<SharedChild>, label: &str) -> Result<()> {
    #[cfg(test)]
    {
        let forced_failure = forced_kill_child_process_failure()
            .lock()
            .expect("forced kill-child-process failure mutex poisoned")
            .clone();
        if let Some(forced_failure) = forced_failure {
            if forced_failure.label == label
                && forced_failure.process_ptr == Arc::as_ptr(process) as usize
            {
                return Err(anyhow!("forced {label} kill failure"));
            }
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

fn shared_child_has_exited(process: &Arc<SharedChild>, label: &str) -> Result<bool> {
    match process.try_wait() {
        Ok(Some(_)) => Ok(true),
        Ok(None) => Ok(false),
        Err(err) => Err(anyhow!("failed checking {label} process status: {err}")),
    }
}

/// Handles wait for shared child exit timeout.
fn wait_for_shared_child_exit_timeout(
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
