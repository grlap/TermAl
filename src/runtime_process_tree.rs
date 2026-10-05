// Dedicated Windows provider runtimes share this lease with their waiter.
// The shared Codex app-server is deliberately outside this ownership domain.
// Root launch stays suspended through job attachment and all worker setup.
// Cleanup claims one attempt before OS work; no process wait holds a state,
// registry or lease lock. Only job ActiveProcesses==0 confirms completion.
// Failed attempts retain the exact handles. Kill-on-close is a backstop, not
// evidence; an unresolved launch continues to gate later dedicated launches.
#[cfg(windows)]
struct RuntimeProcessTree {
    tree: TerminalProcessTree,
    cleanup: Mutex<RuntimeCleanup>,
    completion: Condvar,
    #[cfg(test)]
    termination_failures: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    cleanup_gate: Mutex<Option<TestStopFenceGate>>,
    #[cfg(test)]
    cleanup_join_receipt: Mutex<Option<Sender<()>>>,
}

#[cfg(windows)]
enum RuntimeCleanupPhase {
    Live,
    InProgress(Uuid),
    Unconfirmed { attempt: Uuid, detail: String },
    Confirmed,
}

#[cfg(windows)]
struct RuntimeCleanup {
    phase: RuntimeCleanupPhase,
}

#[cfg(windows)]
#[derive(Clone)]
struct RetainedDedicatedOwner {
    token: RuntimeToken,
    tree: Arc<RuntimeProcessTree>,
    process: Arc<SharedChild>,
    label: &'static str,
}

impl SessionRecord {
    fn dedicated_predecessor_pending(&self) -> bool {
        #[cfg(windows)]
        return self.retained_dedicated_owners.iter().any(|owner|
            !self.runtime.matches_runtime_token(&owner.token) && !owner.tree.is_confirmed());
        #[cfg(not(windows))]
        false
    }
    // Called while attachment still owns Inner; no worker ordering is assumed.
    fn register_dedicated_runtime(&mut self) {
        #[cfg(windows)]
        if let Some((tree, process, label)) = self.runtime.dedicated_tree_owner() {
            let token = self.runtime.runtime_token().expect("attached dedicated token");
            self.retained_dedicated_owners.retain(|owner| !owner.tree.is_confirmed());
            if !self.retained_dedicated_owners.iter().any(|owner| owner.token == token && Arc::ptr_eq(&owner.tree, &tree)) {
                self.retained_dedicated_owners.push(RetainedDedicatedOwner { token, tree, process, label });
            }
        }
    }
}

#[cfg(windows)]
impl RuntimeProcessTree {
    fn terminate(&self, process: &Arc<SharedChild>, label: &str) -> Result<()> {
        self.terminate_with(label, |_| kill_child_process(process, label))
    }

    fn terminate_with(&self, label: &str, stop_root: impl FnOnce(std::time::Instant) -> Result<()>) -> Result<()> {
        self.terminate_attempt(label, stop_root, true)
    }

    fn terminate_attempt(&self, label: &str, stop_root: impl FnOnce(std::time::Instant) -> Result<()>, retry: bool) -> Result<()> {
        self.terminate_attempt_after_claim(label, stop_root, retry, || {})
    }

    fn terminate_attempt_after_claim(&self, label: &str, stop_root: impl FnOnce(std::time::Instant) -> Result<()>, retry: bool,
        on_claim: impl FnOnce()) -> Result<()> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let attempt = {
            let mut cleanup = self.cleanup.lock().expect("runtime cleanup mutex poisoned");
            if let RuntimeCleanupPhase::InProgress(joined) = cleanup.phase {
                #[cfg(test)]
                if let Some(sender) = self.cleanup_join_receipt.lock().unwrap().take() { let _ = sender.send(()); }
                #[cfg(test)]
                TEST_DEDICATED_CLEANUP_JOIN.with(|sender| { if let Some(sender) = sender.borrow_mut().take() { let _ = sender.send(()); } });
                while matches!(cleanup.phase, RuntimeCleanupPhase::InProgress(current) if current == joined) {
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    if remaining.is_zero() { bail!("{label} process tree cleanup remains in progress"); }
                    cleanup = self.completion.wait_timeout(cleanup, remaining)
                        .expect("runtime cleanup mutex poisoned").0;
                }
                return match &cleanup.phase {
                    RuntimeCleanupPhase::Confirmed => Ok(()),
                    RuntimeCleanupPhase::Unconfirmed { detail, .. } => Err(anyhow!(detail.clone())),
                    _ => Err(anyhow!("{label} process tree cleanup attempt changed")),
                };
            }
            if matches!(cleanup.phase, RuntimeCleanupPhase::Confirmed) { return Ok(()); }
            if !retry && let RuntimeCleanupPhase::Unconfirmed { detail, .. } = &cleanup.phase { return Err(anyhow!(detail.clone())); }
            let attempt = Uuid::new_v4();
            cleanup.phase = RuntimeCleanupPhase::InProgress(attempt);
            attempt
        };
        // Only the first claimant owns a provider-failure checkpoint. Publish
        // that report off-lock before termination wakes competing callbacks.
        on_claim();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        // The claimed attempt retains the exact lease while all OS operations
        // run outside its mutex. Root exit is never the descendant proof.
        let result = (|| {
        #[cfg(test)]
        let gate = self.cleanup_gate.lock().unwrap().take();
        #[cfg(test)]
        if let Some(gate) = gate {
            let _ = gate.claimed_tx.send(());
            let _ = gate.release_rx.recv();
        }
        self.terminate_job(label)?;
        stop_root(deadline)?;
        if std::time::Instant::now() >= deadline { bail!("{label} process tree cleanup deadline elapsed"); }
        while !self.has_exited()? {
            if std::time::Instant::now() >= deadline {
                bail!("{label} process tree did not exit after termination");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
        })();
        let mut cleanup = self.cleanup.lock().expect("runtime cleanup mutex poisoned");
        if matches!(cleanup.phase, RuntimeCleanupPhase::InProgress(current) if current == attempt) {
            cleanup.phase = match &result {
                Ok(()) => RuntimeCleanupPhase::Confirmed,
                Err(error) => RuntimeCleanupPhase::Unconfirmed { attempt,
                    detail: format!("{label} process tree cleanup failed: {error:#}") },
            };
        }
        self.completion.notify_all();
        result
    }

    fn cleanup_failure(&self) -> Option<String> {
        match &self.cleanup.lock().expect("runtime cleanup mutex poisoned").phase {
            RuntimeCleanupPhase::InProgress(_) => Some("process tree cleanup remains in progress".to_owned()),
            RuntimeCleanupPhase::Unconfirmed { attempt, detail } => { let _ = attempt; Some(detail.clone()) },
            _ => None,
        }
    }

    #[cfg(test)]
    fn is_live(&self) -> bool {
        matches!(self.cleanup.lock().expect("runtime cleanup mutex poisoned").phase, RuntimeCleanupPhase::Live)
    }

    fn is_in_progress(&self) -> bool {
        matches!(self.cleanup.lock().expect("runtime cleanup mutex poisoned").phase, RuntimeCleanupPhase::InProgress(_))
    }

    fn is_confirmed(&self) -> bool {
        matches!(self.cleanup.lock().expect("runtime cleanup mutex poisoned").phase, RuntimeCleanupPhase::Confirmed)
    }


    fn terminate_job(&self, label: &str) -> Result<()> {
        #[cfg(test)]
        if self
            .termination_failures
            .try_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |remaining| remaining.checked_sub(1),
            )
            .is_ok()
        {
            bail!("forced {label} job termination failure");
        }
        terminate_terminal_job(&self.tree.job, label)
    }

    fn has_exited(&self) -> Result<bool> {
        use std::mem::size_of;
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::{
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation,
            QueryInformationJobObject,
        };
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        let success = unsafe {
            QueryInformationJobObject(
                self.tree.job.handle.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                &mut info as *mut _ as *mut std::ffi::c_void,
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        };
        if success == 0 {
            return Err(io::Error::last_os_error()).context("failed querying runtime job");
        }
        Ok(info.ActiveProcesses == 0)
    }
}

struct DedicatedRuntimeChild {
    process: Arc<SharedChild>,
    stdin: std::process::ChildStdin,
    stdout: std::process::ChildStdout,
    stderr: std::process::ChildStderr,
    #[cfg(windows)]
    tree: Arc<RuntimeProcessTree>,
    #[cfg(windows)]
    launch_guard: RuntimeLaunchGuard,
}

#[cfg(windows)]
fn root_exit_provider_error(result: &io::Result<std::process::ExitStatus>, label: &str) -> Option<String> {
    match result {
        Ok(status) if status.success() => None,
        Ok(status) => Some(format!("{label} session exited with status {status}")),
        Err(error) => Some(format!("failed waiting for {label} session: {error}")),
    }
}

// Disposable protocol producers use the production provider workers. These
// thread-local seams replace only executable discovery and observe completion.
#[cfg(all(test, windows))]
thread_local! {
    static TEST_DEDICATED_COMMAND: std::cell::RefCell<Option<Command>> = const { std::cell::RefCell::new(None) };
    static TEST_DEDICATED_WAITER: std::cell::RefCell<Option<Sender<()>>> = const { std::cell::RefCell::new(None) };
    static TEST_DEDICATED_EXIT_GATE: std::cell::RefCell<Option<TestStopFenceGate>> = const { std::cell::RefCell::new(None) };
    static TEST_DEDICATED_WRITER: std::cell::RefCell<Option<Sender<()>>> = const { std::cell::RefCell::new(None) };
    static TEST_DEDICATED_ACP_PENDING: std::cell::RefCell<Option<Sender<AcpPendingRequestMap>>> = const { std::cell::RefCell::new(None) };
    static TEST_DEDICATED_CLEANUP_JOIN: std::cell::RefCell<Option<Sender<()>>> = const { std::cell::RefCell::new(None) };
    static TEST_DEDICATED_READER_FRAMES: std::cell::RefCell<Option<Sender<()>>> = const { std::cell::RefCell::new(None) };
}

fn dedicated_provider_command(build: impl FnOnce() -> Result<Command>) -> Result<Command> {
    #[cfg(all(test, windows))]
    if let Some(command) = TEST_DEDICATED_COMMAND.with(|command| command.borrow_mut().take()) {
        return Ok(command);
    }
    build()
}

#[cfg(all(test, windows))]
struct DedicatedWaiterTestCompletion(Option<Sender<()>>, Option<TestStopFenceGate>);

#[cfg(all(test, windows))]
impl DedicatedWaiterTestCompletion {
    fn capture() -> Self {
        Self(TEST_DEDICATED_WAITER.with(|sender| sender.borrow_mut().take()), TEST_DEDICATED_EXIT_GATE.with(|gate| gate.borrow_mut().take()))
    }
    fn capture_writer() -> Self {
        Self(TEST_DEDICATED_WRITER.with(|sender| sender.borrow_mut().take()), None)
    }
    fn wait_before_cleanup(&mut self) {
        if let Some(gate) = self.1.take() {
            let _ = gate.claimed_tx.send(());
            let _ = gate.release_rx.recv();
        }
    }
}

#[cfg(all(test, windows))]
impl Drop for DedicatedWaiterTestCompletion {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() { let _ = sender.send(()); }
    }
}

#[cfg(windows)]
struct RuntimeLaunchGuard {
    root: Option<std::os::windows::io::OwnedHandle>,
    tree: Option<Arc<RuntimeProcessTree>>,
    armed: bool,
}

#[cfg(windows)]
enum FailedRuntimeLaunch {
    Raw(std::process::Child),
    Attached(RuntimeLaunchGuard),
}

#[cfg(windows)]
enum FailedLaunchPhase { Ready, InProgress(Uuid), Unconfirmed(String), Confirmed }

#[cfg(windows)]
struct FailedRuntimeLaunchEntry {
    owner: Mutex<Option<FailedRuntimeLaunch>>,
    phase: Mutex<FailedLaunchPhase>,
}

#[cfg(windows)]
type FailedRuntimeLaunchRegistry = Arc<Mutex<Vec<Arc<FailedRuntimeLaunchEntry>>>>;

#[cfg(windows)]
fn failed_runtime_launches() -> FailedRuntimeLaunchRegistry {
    #[cfg(test)]
    if let Some(owners) = TEST_RUNTIME_LAUNCH_OWNERS.with(|owners| owners.borrow().clone()) {
        return owners;
    }
    static OWNERS: std::sync::OnceLock<FailedRuntimeLaunchRegistry> =
        std::sync::OnceLock::new();
    OWNERS
        .get_or_init(|| Arc::new(Mutex::new(Vec::new())))
        .clone()
}

#[cfg(all(test, windows))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimeLaunchPhase {
    BeforeContainment,
    BeforeShare,
    BeforeResume,
}

#[cfg(all(test, windows))]
type RuntimeLaunchTestHook =
    Box<dyn FnMut(RuntimeLaunchPhase, &std::os::windows::io::OwnedHandle) -> Result<()>>;

#[cfg(all(test, windows))]
thread_local! {
    static TEST_RUNTIME_LAUNCH_HOOK: std::cell::RefCell<Option<RuntimeLaunchTestHook>> = const { std::cell::RefCell::new(None) };
    // Failure fixtures get a separate owner registry, while exercising the
    // same retention/retry code. Other tests' real launches cannot consume it.
    static TEST_RUNTIME_LAUNCH_OWNERS: std::cell::RefCell<Option<FailedRuntimeLaunchRegistry>> = const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, windows))]
fn runtime_launch_test_checkpoint(
    phase: RuntimeLaunchPhase,
    guard: &RuntimeLaunchGuard,
) -> Result<()> {
    TEST_RUNTIME_LAUNCH_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(phase, guard.root.as_ref().expect("launch root handle"))?;
        }
        Ok(())
    })
}

#[cfg(windows)]
impl RuntimeLaunchGuard {
    fn cleanup(&self) -> Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::{TerminateProcess, WaitForSingleObject};
        let root = self.root.as_ref().expect("launch root handle");
        if let Some(tree) = &self.tree {
            return tree.terminate_with("failed runtime launch", |deadline| {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if unsafe { WaitForSingleObject(root.as_raw_handle(), remaining.as_millis() as u32) } != WAIT_OBJECT_0 {
                    bail!("failed runtime launch root cleanup is unconfirmed");
                }
                Ok(())
            });
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        if unsafe {
            WaitForSingleObject(
                self.root
                    .as_ref()
                    .expect("launch root handle")
                    .as_raw_handle(),
                0,
            )
        } != WAIT_OBJECT_0
            && unsafe {
                TerminateProcess(
                    self.root
                        .as_ref()
                        .expect("launch root handle")
                        .as_raw_handle(),
                    1,
                )
            } == 0
        {
            return Err(io::Error::last_os_error()).context("failed terminating suspended runtime");
        }
        if unsafe {
            WaitForSingleObject(
                self.root
                    .as_ref()
                    .expect("launch root handle")
                    .as_raw_handle(),
                deadline.saturating_duration_since(std::time::Instant::now()).as_millis() as u32,
            )
        } != WAIT_OBJECT_0
        {
            bail!("failed runtime launch root cleanup is unconfirmed");
        }
        Ok(())
    }

    fn resume(&mut self, process: &Arc<SharedChild>, tree: &RuntimeProcessTree) -> Result<()> {
        #[cfg(test)]
        runtime_launch_test_checkpoint(RuntimeLaunchPhase::BeforeResume, self)?;
        tree.tree.resume_after_attach(process)?;
        self.armed = false;
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for RuntimeLaunchGuard {
    fn drop(&mut self) {
        if self.armed {
            // Keep the exact process handle and job lease reachable. A later
            // launch retries this owner first, never launches an uncontained
            // substitute or infers cleanup from the root's exit alone.
            retain_failed_runtime_launch(FailedRuntimeLaunch::Attached(Self {
                    root: self.root.take(),
                    tree: self.tree.take(),
                    armed: false,
                }));
        }
    }
}

#[cfg(windows)]
fn retry_failed_runtime_launch_cleanup() -> Result<()> {
    let registry = failed_runtime_launches();
    let entries = registry.lock().expect("launch cleanup mutex poisoned").clone();
    if entries.is_empty() { return Ok(()); }
    for entry in entries { schedule_runtime_launch_cleanup(&registry, &entry); }
    bail!("prior runtime launch cleanup remains pending")
}

#[cfg(windows)]
fn retain_failed_runtime_launch(owner: FailedRuntimeLaunch) {
    let registry = failed_runtime_launches();
    let entry = Arc::new(FailedRuntimeLaunchEntry { owner: Mutex::new(Some(owner)), phase: Mutex::new(FailedLaunchPhase::Ready) });
    registry.lock().expect("launch cleanup mutex poisoned").push(entry.clone());
    schedule_runtime_launch_cleanup(&registry, &entry);
}

#[cfg(windows)]
fn schedule_runtime_launch_cleanup(registry: &FailedRuntimeLaunchRegistry, entry: &Arc<FailedRuntimeLaunchEntry>) {
    let attempt = {
        let mut phase = entry.phase.lock().expect("launch attempt mutex poisoned");
        if matches!(*phase, FailedLaunchPhase::InProgress(_) | FailedLaunchPhase::Confirmed) { return; }
        if let FailedLaunchPhase::Unconfirmed(detail) = &*phase { eprintln!("retrying retained launch cleanup: {detail}"); }
        let attempt = Uuid::new_v4();
        *phase = FailedLaunchPhase::InProgress(attempt);
        attempt
    };
    let worker_entry = entry.clone();
    let worker_registry = registry.clone();
    let spawned = std::thread::Builder::new().name("runtime-launch-cleanup".to_owned()).spawn(move || {
        // The in-flight entry stays visible while this worker owns its handles.
        let mut owner = worker_entry.owner.lock().expect("launch owner mutex poisoned").take().expect("retained launch owner");
        let result: Result<()> = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &mut owner {
            FailedRuntimeLaunch::Raw(child) => (|| {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                if child.try_wait()?.is_none() { child.kill()?; }
                while child.try_wait()?.is_none() {
                    if std::time::Instant::now() >= deadline { bail!("raw runtime launch cleanup remains unconfirmed"); }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(())
            })(),
            FailedRuntimeLaunch::Attached(owner) => owner.cleanup(),
        })).unwrap_or_else(|_| Err(anyhow!("runtime launch cleanup worker panicked; exact owner retained")));
        if result.is_err() { *worker_entry.owner.lock().expect("launch owner mutex poisoned") = Some(owner); }
        let mut phase = worker_entry.phase.lock().expect("launch attempt mutex poisoned");
        if !matches!(*phase, FailedLaunchPhase::InProgress(current) if current == attempt) { return; }
        match result {
            Ok(()) => {
                *phase = FailedLaunchPhase::Confirmed;
                worker_registry.lock().expect("launch cleanup mutex poisoned").retain(|candidate| !Arc::ptr_eq(candidate, &worker_entry));
            }
            Err(error) => { eprintln!("runtime launch cleanup retained: {error:#}"); *phase = FailedLaunchPhase::Unconfirmed(format!("{error:#}")); }
        }
    });
    if let Err(error) = spawned {
        // No worker took the owner. Thread creation failure cannot lose handles.
        let mut phase = entry.phase.lock().expect("launch attempt mutex poisoned");
        if matches!(*phase, FailedLaunchPhase::InProgress(current) if current == attempt) {
            *phase = FailedLaunchPhase::Unconfirmed(format!("failed starting cleanup worker: {error}"));
        }
    }
}

// Attach while suspended, before even extracting the pipes. The raw child
// guard and the kill-on-close job also cover failed SharedChild construction.
fn spawn_dedicated_runtime(command: &mut Command, label: &str) -> Result<DedicatedRuntimeChild> {
    #[cfg(windows)]
    retry_failed_runtime_launch_cleanup()?;
    #[cfg(windows)]
    configure_terminal_process_tree(command);
    let mut child = command
        .spawn()
        .with_context(|| format!("failed starting {label}"))?;
    #[cfg(windows)]
    let mut launch_guard = {
        use std::os::windows::io::AsHandle;
        match child.as_handle().try_clone_to_owned() {
            Ok(root) => RuntimeLaunchGuard {
                root: Some(root),
                tree: None,
                armed: true,
            },
            Err(error) => {
                retain_failed_runtime_launch(FailedRuntimeLaunch::Raw(child));
                return Err(error)
                    .context("failed retaining runtime launch handle; cleanup pending");
            }
        }
    };
    #[cfg(windows)]
    let job = {
        #[cfg(test)]
        runtime_launch_test_checkpoint(RuntimeLaunchPhase::BeforeContainment, &launch_guard)?;
        use std::os::windows::io::AsRawHandle;
        let result = create_terminal_job_object().and_then(|job| {
            let assigned = unsafe {
                windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(
                    job.handle.as_raw_handle(),
                    child.as_raw_handle(),
                )
            };
            if assigned == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        });
        match result {
            Ok(job) => job,
            Err(error) => {
                return Err(error).context("failed containing dedicated runtime");
            }
        }
    };
    #[cfg(windows)]
    let tree = Arc::new(RuntimeProcessTree {
        tree: TerminalProcessTree { job },
        cleanup: Mutex::new(RuntimeCleanup { phase: RuntimeCleanupPhase::Live }),
        #[cfg(test)]
        cleanup_gate: Mutex::new(None),
        #[cfg(test)]
        cleanup_join_receipt: Mutex::new(None),
        completion: Condvar::new(),
        #[cfg(test)]
        termination_failures: std::sync::atomic::AtomicUsize::new(0),
    });
    #[cfg(windows)]
    {
        launch_guard.tree = Some(tree.clone());
    }
    let pipes = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    let (Some(stdin), Some(stdout), Some(stderr)) = pipes else {
        bail!("{label} runtime pipes unavailable");
    };
    #[cfg(all(test, windows))]
    runtime_launch_test_checkpoint(RuntimeLaunchPhase::BeforeShare, &launch_guard)?;
    let process = Arc::new(SharedChild::new(child).context("failed sharing dedicated runtime")?);
    Ok(DedicatedRuntimeChild {
        process,
        stdin,
        stdout,
        stderr,
        #[cfg(windows)]
        tree,
        #[cfg(windows)]
        launch_guard,
    })
}

#[cfg(windows)]
fn dedicated_reset_context(record: &SessionRecord) -> Vec<u8> {
    serde_json::to_vec(&json!({"agent":record.session.agent,
        "cwd":record.session.workdir,"model":record.session.model,
        "claudeApproval":record.session.claude_approval_mode,"claudeEffort":record.session.claude_effort,
        "geminiApproval":record.session.gemini_approval_mode,"cursorMode":record.session.cursor_mode,
        "opencodeModel":record.session.opencode_model,"opencodeEffort":record.session.opencode_effort,
        "opencodeMode":record.session.opencode_mode,"external":record.external_session_id,
        "reset":record.runtime_reset_required,"turn":record.active_turn_generation,
        "dispatch":record.engram.dispatch_generation,
        "head":record.queued_prompts.front().map(|head| &head.pending_prompt.id)})).expect("reset context serializes")
}

impl AppState {
    fn retry_retained_dedicated_cleanup(&self, session_id: &str) -> Result<()> {
        #[cfg(windows)]
        {
            let owners = {
                let inner = self.inner.lock().expect("state mutex poisoned");
                let Some(index) = inner.find_session_index(session_id) else { return Ok(()); };
                let record = &inner.sessions[index];
                record.retained_dedicated_owners.iter().filter(|owner|
                    !record.runtime.matches_runtime_token(&owner.token)).cloned().collect::<Vec<_>>()
            };
            for owner in owners {
                let outcome = owner.tree.terminate(&owner.process, owner.label);
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                if let Some(index) = inner.find_session_index(session_id) {
                    inner.sessions[index].retained_dedicated_owners.retain(|current|
                        !(current.token == owner.token && Arc::ptr_eq(&current.tree, &owner.tree) && current.tree.is_confirmed()));
                }
                outcome?;
            }
        }
        #[cfg(not(windows))]
        let _ = session_id;
        Ok(())
    }

    fn prepare_dedicated_runtime_reset_off_lock(&self, session_id: &str, model_refresh: bool) -> Result<()> {
        self.retry_retained_dedicated_cleanup(session_id)?;
        #[cfg(not(windows))]
        { let _ = (session_id, model_refresh); return Ok(()); }
        #[cfg(windows)]
        {
            let captured = {
                let mut inner = self.inner.lock().expect("state mutex poisoned");
                let Some(index) = inner.find_session_index(session_id) else { return Ok(()); };
                let record = &mut inner.sessions[index];
                let refresh_restarts = model_refresh && matches!(record.session.agent, Agent::Claude | Agent::OpenCode | Agent::Kimi);
                if !(record.runtime_reset_required || refresh_restarts)
                    || matches!(record.session.status, SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping) {
                    return Ok(());
                }
                if record.runtime_stop_in_progress { bail!("session is stopping"); }
                if let Some(detail) = record.runtime.dedicated_cleanup_failure() { bail!("{detail}; Stop must finish cleanup before reset"); }
                let Some((tree, process, label)) = record.runtime.dedicated_tree_owner() else { return Ok(()); };
                let token = record.runtime.runtime_token().expect("dedicated runtime token");
                let context = dedicated_reset_context(record);
                let installed = record.engram_mcp_installed.clone();
                let generation = record.claim_runtime_stop(RuntimeStopOwnerKind::DedicatedReset, token.clone());
                (tree, process, label, token, context, installed, generation)
            };
            let (tree, process, label, token, context, installed, generation) = captured;
            let outcome = tree.terminate(&process, label);
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else { return outcome; };
            let record = &mut inner.sessions[index];
            if !record.runtime.matches_runtime_token(&token)
                || !record.runtime_stop_is_owned_by(RuntimeStopOwnerKind::DedicatedReset, &token, generation) {
                return Err(anyhow!("dedicated reset no longer owns the runtime"));
            }
            let same_context = dedicated_reset_context(record) == context && record.engram_mcp_installed == installed;
            record.clear_runtime_stop();
            if let Err(error) = outcome {
                record.set_auto_dispatch_blocked(true);
                self.commit_locked(&mut inner)?;
                let callbacks = std::mem::take(&mut inner.sessions[index].deferred_stop_callbacks);
                drop(inner);
                self.replay_deferred_runtime_stop_callbacks(session_id, &token, callbacks);
                let _ = self.handle_runtime_exit_if_matches(session_id, &token, None);
                return Err(error);
            }
            if !same_context {
                record.set_auto_dispatch_blocked(true);
                self.commit_locked(&mut inner)?;
                return Err(anyhow!("dedicated reset configuration or admission changed; callbacks retained for exact retry"));
            }
            record.clear_runtime();
            record.clear_runtime_reset();
            clear_all_pending_requests(record);
            self.commit_locked(&mut inner)?;
            inner.sessions[index].deferred_stop_callbacks.clear();
            Ok(())
        }
    }
}
