// Engram's ordinary host-private work-session protocol and adapter.
//
// Owns: protocol serialization, the `EngramControlTransport` seam and its test
// doubles and deadline-bounded work-service calls.
// Does not own: the persistent per-session `engram control` process transport
// (`engram_control_transport.rs`), TermAl's dispatch/termination policy,
// project settings routes, transcript mutation, or delegation lifecycle. Those
// call this seam only after releasing the global state mutex.

use super::*;

#[cfg(test)]
pub(crate) const ENGRAM_CONTROL_SCHEMA_VERSION: u16 = 1;
/// Ordinary Engram operations get at least one minute. A slow operation is
/// diagnosed after ten seconds without interrupting it.
// Shared with the launcher's budget owner: operation minimum, process delivery.
pub(crate) const ENGRAM_CALL_BUDGET_MS: [u64; 2] =
    include!("../scripts/engram-call-budget-ms.json");
pub(crate) const ENGRAM_MIN_CALL_TIMEOUT: Duration =
    Duration::from_millis(ENGRAM_CALL_BUDGET_MS[0]);
pub(crate) const ENGRAM_CALL_WARNING_AFTER: Duration = Duration::from_secs(10);
pub(crate) const ENGRAM_MAX_CALL_TIMEOUT_MS: u64 = 60_000;
pub(crate) const ENGRAM_DEFAULT_CALL_TIMEOUT_MS: u64 = ENGRAM_MAX_CALL_TIMEOUT_MS;
/// Test process startup allows the call bound plus one second for cleanup
/// ownership to publish its terminal state.
#[cfg(test)]
pub(crate) const ENGRAM_CONTROL_SETTLE_TIMEOUT: Duration =
    Duration::from_millis(ENGRAM_MAX_CALL_TIMEOUT_MS + 1_000);
// Only explicit Full Audit runs `engram doctor --json`; that audit is the
// only consumer of this bound — it is not an ordinary Engram call timeout and
// must not be confused with one. The value comes from measurement, not taste.
// Runs reported on Engram build 304edbce2b18 (2026-09-09): 122.441 s and
// 113.674 s auditing a migrated store's preserved history, against 4.946 s and
// 5.325 s for a small one. Those are exact samples, not a proven upper bound.
// The previous 5-second bound rejected the healthy migrated store outright.
// Five minutes is about two and a half times the largest measured run; being
// wrong high costs a longer wait before failure. Expiry attempts process-tree
// termination; failed cleanup does not block the response.
// THIS BOUND EXPIRES AS EVIDENCE. Engram build 7c773f62c50f (2026-09-17) keeps
// no archive of pre-migration records: a store changes format by a plain JSON
// export and import, and nothing of the old format stays behind. Measured on
// the same store after that conversion (2,288 MiB -> 174 MiB, the preserved
// archive gone): 84.5 s, against 3.9 s for a 70 MiB store. Removing 94% of the
// bytes took only about a quarter off the time, so the audit's cost is
// dominated by verifying CURRENT records and grows with them; the earlier
// guess that a frozen archive drove it was wrong. There is no longer a step at
// each migration. Re-measure when a store's record count has grown severalfold,
// and raise this from a measurement rather than a guess.
#[cfg(not(test))]
pub(crate) const ENGRAM_ENABLEMENT_DOCTOR_TIMEOUT: Duration = Duration::from_secs(5 * 60);
// The Windows fixture launches through powershell.exe, and the full Rust suite
// starts several process-heavy tests concurrently. Fixtures answer immediately,
// so tests keep a much shorter bound: a hung fixture must fail the suite
// quickly instead of holding it for the production window.
#[cfg(test)]
pub(crate) const ENGRAM_ENABLEMENT_DOCTOR_TIMEOUT: Duration = ENGRAM_MIN_CALL_TIMEOUT;
pub(crate) const ENGRAM_WORK_BINDING_COMMAND_TIMEOUT: Duration = ENGRAM_MIN_CALL_TIMEOUT;
pub(crate) const ENGRAM_WORK_BINDING_LOCK_RETRY_DELAY: Duration = Duration::from_millis(250);

// Outer budgets decide whether to admit another call. Once admitted, a call
// retains its minimum operation window even near the end of that budget.
pub(crate) fn engram_operation_timeout(requested: Duration) -> Duration {
    if requested.is_zero() {
        requested
    } else {
        requested.max(ENGRAM_MIN_CALL_TIMEOUT)
    }
}

pub(crate) fn warn_slow_engram_call(
    started: std::time::Instant,
    warned: &mut bool,
    operation: &str,
) {
    let elapsed = started.elapsed();
    if !*warned && elapsed >= ENGRAM_CALL_WARNING_AFTER {
        *warned = true;
        eprintln!(
            "engram warning> {operation} still running after {} ms",
            elapsed.as_millis()
        );
    }
}

pub(crate) fn receive_engram_call<T>(
    receiver: &mpsc::Receiver<T>,
    started: std::time::Instant,
    timeout: Duration,
    operation: &str,
) -> Result<T, mpsc::RecvTimeoutError> {
    let mut warned = false;
    loop {
        warn_slow_engram_call(started, &mut warned, operation);
        let remaining = timeout.saturating_sub(started.elapsed());
        let wait = if warned {
            remaining
        } else {
            remaining.min(ENGRAM_CALL_WARNING_AFTER.saturating_sub(started.elapsed()))
        };
        match receiver.recv_timeout(wait) {
            Err(mpsc::RecvTimeoutError::Timeout) if started.elapsed() < timeout => continue,
            result => return result,
        }
    }
}

pub(crate) const ENGRAM_CONTROL_MAX_FRAME_BYTES: usize = 256 * 1_024;
// Resource policy, not a measured upper bound on reports. Doctor reports
// are independent of control frames; cap each of stdout/stderr at 8 MiB.
pub(crate) const ENGRAM_DOCTOR_MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const ENGRAM_GLOBAL_DISABLE_ENV: &str = "TERMAL_ENGRAM_DISABLED";

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EngramAuthorityStoreKey {
    pub(crate) database_path: PathBuf,
    pub(crate) project_id: String,
}

#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EngramRetiredWorkAuthorityGrant {
    pub(crate) home: String,
    /// Raw project root retained even when `.engram-project` cannot be read.
    /// Retained as historical data; no revocation or ingress route reads it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) project_root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) store_key: Option<EngramAuthorityStoreKey>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) project_id: String,
    pub(crate) grant_hash: String,
    pub(crate) retired_at: String,
    pub(crate) reason: String,
    #[serde(default)]
    pub(crate) revoke_confirmed: bool,
}

impl std::fmt::Debug for EngramRetiredWorkAuthorityGrant {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EngramRetiredWorkAuthorityGrant")
            .field("home", &self.home)
            .field("project_root", &self.project_root)
            .field("store_key", &self.store_key)
            .field("project_id", &self.project_id)
            .field("grant_hash", &"[REDACTED]")
            .field("retired_at", &self.retired_at)
            .field("reason", &self.reason)
            .field("revoke_confirmed", &self.revoke_confirmed)
            .finish()
    }
}

pub(crate) fn default_engram_execution_observability() -> bool {
    true
}

#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EngramProjectSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) acceptance_evaluation: Option<AcceptanceEvaluatorDefaults>,
    #[serde(default)]
    pub(crate) enabled: bool,
    /// Controls new automatic execution observations, never turn admission.
    #[serde(default = "default_engram_execution_observability")]
    pub(crate) execution_observability: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) binary_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) home: Option<String>,
    /// Host-private connection identity; omitted from client snapshots and
    /// MCP composition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) authority_store_key: Option<EngramAuthorityStoreKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) deadline_ms: Option<u64>,
}

impl std::fmt::Debug for EngramProjectSettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EngramProjectSettings")
            .field("acceptance_evaluation", &self.acceptance_evaluation)
            .field("enabled", &self.enabled)
            .field("execution_observability", &self.execution_observability)
            .field("binary_path", &self.binary_path)
            .field("home", &self.home)
            .field("authority_store_key", &self.authority_store_key)
            .field("deadline_ms", &self.deadline_ms)
            .finish()
    }
}

impl Default for EngramProjectSettings {
    fn default() -> Self {
        Self {
            acceptance_evaluation: None,
            enabled: false,
            execution_observability: true,
            binary_path: None,
            home: None,
            authority_store_key: None,
            deadline_ms: None,
        }
    }
}

impl EngramProjectSettings {
    pub(crate) fn is_base_enabled(&self) -> bool {
        self.enabled && !engram_globally_disabled()
    }

    pub(crate) fn call_timeout(&self) -> Duration {
        Duration::from_millis(
            self.deadline_ms
                .unwrap_or(ENGRAM_DEFAULT_CALL_TIMEOUT_MS)
                .max(ENGRAM_MIN_CALL_TIMEOUT.as_millis() as u64),
        )
    }
}

pub(crate) fn engram_globally_disabled() -> bool {
    std::env::var(ENGRAM_GLOBAL_DISABLE_ENV)
        .ok()
        .is_some_and(|value| engram_disable_env_value_is_truthy(&value))
}

pub(crate) fn engram_disable_env_value_is_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

pub(crate) fn validate_engram_project_enablement(
    project: &Project,
    settings: &EngramProjectSettings,
    host_workdir: &FsPath,
) -> std::result::Result<EngramAuthorityStoreKey, ApiError> {
    let (binary_path, project_file, home) =
        validate_engram_project_connection_paths(project, settings)?;
    let project_root = FsPath::new(&project.root_path);
    let result = run_engram_readiness(
        &binary_path,
        &project_file,
        &home,
        project_root,
        host_workdir,
    )?;
    validate_engram_readiness(&result, &project_file, &home)
}

pub(crate) fn validate_engram_project_connection_paths(
    project: &Project,
    settings: &EngramProjectSettings,
) -> std::result::Result<(PathBuf, PathBuf, PathBuf), ApiError> {
    let project_root = FsPath::new(&project.root_path);
    let project_file = project_root.join(".engram-project");
    let metadata = fs::metadata(&project_file).map_err(|err| {
        ApiError::bad_request(format!(
            "cannot enable Engram: `{}` is missing or unreadable: {err}",
            project_file.display()
        ))
    })?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(ApiError::bad_request(format!(
            "cannot enable Engram: `{}` must be a non-empty file",
            project_file.display()
        )));
    }
    let (binary_path, home) = validate_engram_host_connection_paths(settings)?;
    Ok((binary_path, project_file, home))
}

pub(crate) fn validate_engram_host_connection_paths(
    settings: &EngramProjectSettings,
) -> std::result::Result<(PathBuf, PathBuf), ApiError> {
    let binary_path = settings
        .binary_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| ApiError::bad_request("cannot enable Engram without binaryPath"))?;
    let binary_is_path = binary_path.components().count() > 1;
    if (binary_is_path && (!binary_path.is_absolute() || !binary_path.is_file()))
        || (!binary_is_path && binary_path.as_os_str().is_empty())
    {
        return Err(ApiError::bad_request(
            "cannot enable Engram: binaryPath must be a command on PATH or an existing absolute file",
        ));
    }
    let home = settings
        .home
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| ApiError::bad_request("cannot enable Engram without home"))?;
    if !home.is_absolute() || !home.is_dir() {
        return Err(ApiError::bad_request(
            "cannot enable Engram: home must be an existing absolute directory",
        ));
    }
    Ok((binary_path, home))
}

/// The deadline is a parameter so the expiry branch is reachable in a test
/// without waiting out the production window. Callers outside tests always pass
/// [`ENGRAM_ENABLEMENT_DOCTOR_TIMEOUT`].
#[cfg(test)]
pub(crate) fn run_engram_doctor_result_within(
    binary_path: &FsPath,
    project_file: &FsPath,
    home: &FsPath,
    project_root: &FsPath,
    doctor_timeout: Duration,
    host_workdir: &FsPath,
) -> std::result::Result<EngramDoctorResult, ApiError> {
    let output = run_engram_diagnostic_within(
        binary_path,
        project_file,
        home,
        project_root,
        "doctor",
        doctor_timeout,
        host_workdir,
    )?;
    if output.status.success() {
        let result: EngramDoctorResult =
            serde_json::from_slice(&output.stdout).map_err(|error| {
                ApiError::bad_request(format!("Engram doctor returned invalid JSON: {error}"))
            })?;
        return Ok(result);
    }
    Err(ApiError::bad_request(format!(
        "Engram doctor failed ({}): {} {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    )))
}

// Shared bounded process transport; callers retain distinct admission contracts.
pub(crate) fn run_engram_diagnostic_within(
    binary_path: &FsPath,
    project_file: &FsPath,
    home: &FsPath,
    project_root: &FsPath,
    diagnostic: &str,
    doctor_timeout: Duration,
    host_workdir: &FsPath,
) -> std::result::Result<std::process::Output, ApiError> {
    run_engram_diagnostic_args_within(
        binary_path,
        project_file,
        home,
        project_root,
        diagnostic,
        &[],
        doctor_timeout,
        host_workdir,
    )
}

fn run_engram_diagnostic_args_within(
    binary_path: &FsPath,
    project_file: &FsPath,
    home: &FsPath,
    project_root: &FsPath,
    diagnostic: &str,
    args: &[&str],
    doctor_timeout: Duration,
    host_workdir: &FsPath,
) -> std::result::Result<std::process::Output, ApiError> {
    run_engram_diagnostic_args_until(
        binary_path,
        project_file,
        home,
        project_root,
        diagnostic,
        args,
        std::time::Instant::now() + doctor_timeout,
        doctor_timeout,
        host_workdir,
    )
}

// The absolute deadline includes process startup and pipe collection. Callers
// sharing a budget across subprocesses must not restart it for each child.
pub(crate) fn run_engram_diagnostic_args_until(
    binary_path: &FsPath,
    project_file: &FsPath,
    home: &FsPath,
    project_root: &FsPath,
    diagnostic: &str,
    args: &[&str],
    deadline: std::time::Instant,
    doctor_timeout: Duration,
    host_workdir: &FsPath,
) -> std::result::Result<std::process::Output, ApiError> {
    let observed_at = std::time::Instant::now();
    // Only registered test fixtures separate startup from the measured budget.
    // These bindings are absent from production builds.
    #[cfg(test)]
    let fixture_deadline = test_engram_doctor_deadline_state(home, diagnostic);
    #[cfg(test)]
    let deadline =
        test_engram_doctor_fixture_startup_deadline(fixture_deadline.as_deref(), deadline);
    if std::time::Instant::now() >= deadline {
        return Err(ApiError::bad_request(format!(
            "Engram {diagnostic} exceeded the {} second enablement deadline before process launch",
            doctor_timeout.as_secs()
        )));
    }
    // Drain both streams on their own threads and terminate the whole process
    // tree, exactly as the authority-revocation path below does. Neither is
    // optional here. Waiting for exit before reading DEADLOCKS a doctor whose
    // report outgrows the pipe buffer, and the deadline then reports it as a
    // slow audit — indistinguishable from the case this bound exists for, so a
    // reader would raise the bound again and never find the cause. And a bare
    // kill would reach only the direct child: engram_command deliberately wraps
    // `.cmd`/`.bat` and `.ps1` shims in an interpreter, leaving the real doctor
    // a grandchild that keeps auditing and holding the store after we returned.
    #[cfg(windows)]
    let resolved = {
        validate_engram_host_launch_paths(project_file, home, host_workdir)
            .map_err(|error| ApiError::bad_request(error.message))?;
        let resolved = resolve_engram_host_program(binary_path, project_root)
            .map_err(|error| ApiError::bad_request(error.message))?;
        resolved
    };
    #[cfg(windows)]
    let mut command = engram_host_command_for_program(&resolved, project_root, host_workdir)
        .map_err(|error| ApiError::bad_request(error.message))?;
    #[cfg(not(windows))]
    let mut command =
        engram_host_command(binary_path, project_root, project_file, home, host_workdir)
            .map_err(|error| ApiError::bad_request(error.message))?;
    command
        .arg("--project-file")
        .arg(project_file)
        .arg("--home")
        .arg(home)
        .arg(diagnostic)
        .args(args)
        .arg("--json");
    // Test-only: the child's temporary files stay in its Engram home
    // (src/engram_test_child_temp.rs).
    #[cfg(test)]
    for (name, value) in engram_test_child_temp_env(home) {
        command.env(name, value);
    }
    configure_terminal_process_tree(&mut command);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            ApiError::bad_request(format!("Engram {diagnostic} failed to start: {err}"))
        })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ApiError::bad_request("Engram doctor stdout is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ApiError::bad_request("Engram doctor stderr is unavailable"))?;
    let process = Arc::new(SharedChild::new(child).map_err(|err| {
        ApiError::bad_request(format!("failed sharing the Engram doctor process: {err}"))
    })?);
    let process_tree = EngramProcessTree::attach(&process).map_err(|err| {
        if let Err(error) = kill_child_process(&process, "Engram doctor") {
            eprintln!("engram warning> failed terminating unattached doctor: {error:#}");
        }
        reap_engram_doctor_in_background(&process);
        ApiError::bad_request(format!(
            "failed preparing the Engram doctor process tree: {err:#}"
        ))
    })?;
    process_tree.resume_after_attach(&process).map_err(|err| {
        stop_engram_doctor_before_reap(&process_tree, &process);
        ApiError::bad_request(format!("failed resuming the Engram doctor: {err:#}"))
    })?;
    let stdout_reader = start_engram_doctor_reader(stdout, "stdout");
    let stderr_reader = start_engram_doctor_reader(stderr, "stderr");
    // The real fixture must publish descendant/probe readiness before its
    // test-only budget is armed. No registry lock is held while waiting.
    #[cfg(test)]
    let deadline = match test_engram_doctor_fixture_armed_deadline(
        fixture_deadline.as_deref(),
        deadline,
        doctor_timeout,
    ) {
        Ok(deadline) => deadline,
        Err(error) => {
            stop_engram_doctor_before_reap(&process_tree, &process);
            wait_for_test_engram_expired_child(&process);
            return Err(error);
        }
    };
    // Test seam: a registered launch outlasted its budget upon resumption
    // (or, for armed doctor-tree fixtures, at the first measured observation).
    // Apply the same cleanup/error even if the child has already exited;
    // Unix does not suspend it (src/engram_readiness_test_seam.rs).
    #[cfg(test)]
    if test_engram_diagnostic_launch_expired(home) {
        stop_engram_doctor_before_reap(&process_tree, &process);
        wait_for_test_engram_expired_child(&process);
        return Err(ApiError::bad_request(format!(
            "Engram {diagnostic} exceeded the {} second enablement deadline",
            doctor_timeout.as_secs()
        )));
    }
    let mut warned = false;
    let status = loop {
        warn_slow_engram_call(observed_at, &mut warned, diagnostic);
        match process.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            outcome => {
                // No waiter has reaped this process. Signal before starting the
                // detached reap so a Unix group ID cannot have been recycled.
                stop_engram_doctor_before_reap(&process_tree, &process);
                return Err(ApiError::bad_request(match outcome {
                    Ok(None) => format!(
                        "Engram {diagnostic} exceeded the {} second enablement deadline",
                        doctor_timeout.as_secs()
                    ),
                    Err(error) => format!("failed waiting for Engram doctor: {error}"),
                    Ok(Some(_)) => unreachable!(),
                }));
            }
        }
    };
    // A direct child exit is NOT EOF: descendants can still own either pipe.
    // Like terminal's bounded reader collection, receive against a deadline,
    // never join a blocking reader. Unlike terminal output, partial doctor
    // JSON is not useful: timeout/oversize are errors, never healthy reports.
    let output = (|| {
        let stdout = collect_engram_doctor_output(stdout_reader, "stdout", deadline)?;
        let stderr = collect_engram_doctor_output(stderr_reader, "stderr", deadline)?;
        Ok::<_, ApiError>((stdout, stderr))
    })();
    // Post-reap cleanup uses a stable Windows job handle, and deliberately
    // does not signal a recycled process-group ID on Unix. Cleanup failure
    // must not make collection unbounded. Unix escaped/remaining descendants
    // may keep detached readers alive until they close their pipes.
    if let Err(error) = process_tree
        .inner
        .cleanup_after_shell_exit(&process, "Engram doctor")
    {
        eprintln!("engram warning> doctor post-exit cleanup failed: {error:#}");
    }
    let (collected_stdout, collected_stderr) = output?;
    Ok(std::process::Output {
        status,
        stdout: collected_stdout,
        stderr: collected_stderr,
    })
}

#[derive(Clone, Deserialize)]
pub(crate) struct EngramDoctorControl {
    #[serde(default)]
    pub(crate) schema_version: Option<u16>,
    #[serde(default)]
    pub(crate) epoch: Option<i64>,
    #[serde(default)]
    pub(crate) policy: Option<String>,
    #[serde(default)]
    pub(crate) obligation_rules: Option<String>,
}

#[cfg(test)]
#[derive(Clone, Deserialize)]
pub(crate) struct EngramDoctorResult {
    pub(crate) healthy: bool,
    pub(crate) database: PathBuf,
    pub(crate) project_id: String,
}

#[cfg(any(not(windows), test))]
pub(crate) fn engram_command(binary_path: &FsPath) -> Command {
    let extension = binary_path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();

    #[cfg(windows)]
    if extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat") {
        let mut command = Command::new("cmd.exe");
        command.args(["/D", "/S", "/C"]);
        command.arg(binary_path);
        return command;
    }

    #[cfg(windows)]
    if extension.eq_ignore_ascii_case("ps1") {
        let mut command = Command::new("powershell.exe");
        command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"]);
        command.arg(binary_path);
        return command;
    }

    #[cfg(not(windows))]
    if extension.eq_ignore_ascii_case("sh") {
        let mut command = Command::new("sh");
        command.arg(binary_path);
        return command;
    }

    Command::new(binary_path)
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EngramEffect {
    Observe,
    Communicate,
    MutateLocal,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EngramNextIntent {
    Continue,
    Wait,
    Exit,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub(crate) enum EngramControlRequest {
    WorkSessionRead {},
    ExecutionObserve {
        #[serde(flatten)]
        observation: EngramInterTurnObservation,
    },
    AcceptanceBindingRead {
        work_id: String,
        expected_work_revision: i64,
        run_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        after: Option<String>,
    },
    AcceptanceVerificationRead {
        work_id: String,
        expected_work_revision: i64,
        run_id: String,
        run_cut: i64,
        criterion: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        after: Option<String>,
    },
    NamedRootRead {
        run_id: String,
        claim_id: String,
    },
    /// Host-private: the initial sighting of the root a work's run binds at
    /// `run_cut` (the head when absent). Needs no routing token or bound
    /// session (`acceptance_named_root_sighting.rs`).
    NamedRootSightingRead {
        work_ref: String,
        run_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        run_cut: Option<i64>,
    },
    NamedRootBind {
        claim_id: String,
        claim_fence: i64,
        workspace_id: String,
        generation: i64,
        named_at: String,
        kind: EngramNamedRootKind,
        #[serde(skip_serializing_if = "Option::is_none")]
        end_reason: Option<EngramNamedRootEndReason>,
        idempotency_key: String,
    },
    /// Retained closed host-private verification-bind wire shape. TermAl has
    /// no batch-binding decision or request sender. Existing bound records
    /// remain readable as acceptance evidence.
    VerificationBind {
        idempotency_key: String,
        original: String,
        measurement: EngramBindMeasurement,
        targets: Vec<EngramVerificationBindTarget>,
    },
    /// Host-private: one registered committed-input launcher run's
    /// verification record (`engram_verification_record.rs`), sent on the
    /// initiating session's ordinary connection.
    /// `record` is the body exactly as first built and persisted beside the
    /// run, so a retry sends the same bytes under the same operation key; the
    /// operation key and body are immutable across retries.
    VerificationRecord {
        /// Engram's fixed operation key, `termal-run:<run id>`.
        operation_key: String,
        #[serde(flatten)]
        record: Value,
    },
    /// Closed SourceId-only bridge. Delivery/recovery owns the frozen key,
    /// native provenance and destination; Engram alone decides credit.
    RemoteVerificationRecord {
        #[serde(flatten)]
        record: engram_remote_verification::Request,
    },
}

impl EngramControlRequest {
    pub(crate) fn operation(&self) -> &'static str {
        match self {
            Self::WorkSessionRead { .. } => "work_session_read",
            Self::ExecutionObserve { .. } => "execution_observe",
            Self::AcceptanceBindingRead { .. } => "acceptance_binding_read",
            Self::AcceptanceVerificationRead { .. } => "acceptance_verification_read",
            Self::NamedRootRead { .. } => "named_root_read",
            Self::NamedRootSightingRead { .. } => "named_root_sighting_read",
            Self::NamedRootBind { .. } => "named_root_bind",
            Self::VerificationBind { .. } => "verification_bind",
            Self::VerificationRecord { .. } => "verification_record",
            Self::RemoteVerificationRecord { .. } => "remote_verification_record",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct EngramControlWorkBinding {
    pub(crate) root_execution_id: String,
    pub(crate) work_id: String,
    pub(crate) run_id: String,
    pub(crate) work_revision: i64,
    pub(crate) claim_id: String,
    pub(crate) claim_fence: i64,
}

pub(crate) fn describe_engram_work_binding(binding: Option<&EngramControlWorkBinding>) -> String {
    binding.map_or_else(
        || "none".to_owned(),
        |binding| {
            format!(
                "work {} revision {} fence {}",
                binding.work_id, binding.work_revision, binding.claim_fence
            )
        },
    )
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct EngramExecutionObservationInput {
    pub(crate) observation_id: String,
    pub(crate) action_fingerprint: String,
    pub(crate) effect: EngramEffect,
    pub(crate) outcome: EngramExecutionOutcome,
    pub(crate) source_changed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_basis: Option<EngramExecutionSourceBasis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) observed_at: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EngramExecutionOutcome {
    Succeeded,
    Failed,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct EngramExecutionSourceBasis {
    pub(crate) workspace_id: String,
    // Legacy records keep their original text; owner-backed observations use
    // only source_id. Naming and admission retain their separate contracts.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) source_revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_id: Option<EngramSourceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_root_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_root_state: Option<EngramSourceRootState>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum EngramControlResponse {
    Ok { result: Value },
    Error { error: EngramControlErrorWire },
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct EngramControlErrorBody {
    pub(crate) code: String,
    pub(crate) message: String,
}

/// An error as Engram sends it. Newer builds add a typed `reason` beside
/// `code` (for example why a named-root read was refused); it is kept in the
/// message, so every reader of the error sees it and older builds read as
/// before.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct EngramControlErrorWire {
    pub(crate) code: String,
    pub(crate) message: String,
    #[serde(default)]
    pub(crate) reason: Option<String>,
    #[serde(default)]
    pub(crate) details: Value,
}

impl EngramControlErrorWire {
    /// The field a registered-verification refusal names: a plain label
    /// (lowercase ASCII letters, digits, `_` and `.`, at most 64 bytes),
    /// never its reason or any other detail.
    fn verification_refused_field(&self) -> Option<String> {
        if self.code != "verification_record_refused" {
            return None;
        }
        let field = self.details.get("field").and_then(Value::as_str)?;
        (!field.is_empty()
            && field.len() <= 64
            && field.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'.'
            }))
        .then(|| field.to_owned())
    }

    /// The transport error this answer is, with its refused field kept as a
    /// typed label beside the message.
    pub(crate) fn into_transport_error(self) -> EngramTransportError {
        let refused_field = self.verification_refused_field();
        let mut error = EngramTransportError::remote(self.into_body());
        error.refused_field = refused_field;
        error
    }

    pub(crate) fn into_body(self) -> EngramControlErrorBody {
        let message = match self.reason {
            Some(reason) if !reason.trim().is_empty() => {
                format!("{} (reason: {})", self.message, reason.trim())
            }
            _ => self.message,
        };
        // Registered-verification refusals carry safe field/reason labels in
        // details. Keep those labels through the existing error message; do
        // not expose other details or the rejected request's values.
        let message = if self.code == "verification_record_refused" {
            match (
                self.details.get("field").and_then(Value::as_str),
                self.details.get("reason").and_then(Value::as_str),
            ) {
                (Some(field), Some(reason))
                    if [field, reason].iter().all(|text| {
                        !text.is_empty()
                            && text.len() <= 4096
                            && !text.chars().any(char::is_control)
                    }) =>
                {
                    format!("{message} (field: {field}; reason: {reason})")
                }
                _ => message,
            }
        } else {
            message
        };
        EngramControlErrorBody {
            code: self.code,
            message,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EngramTransportErrorKind {
    LocalState,
    Deadline,
    Transport,
    Protocol,
    Remote,
    #[cfg(test)]
    Backoff,
}

#[derive(Clone, Debug)]
pub(crate) struct EngramTransportError {
    pub(crate) causal_failure: Option<EngramCausalFailure>,
    pub(crate) kind: EngramTransportErrorKind,
    pub(crate) code: Option<String>,
    /// Engram's own refusal code, when a failed CLI call printed its JSON
    /// error (`decode_engram_cli_json`). It sits beside `code`, which stays
    /// the transport's own code, so that callers matching on `code` are
    /// unchanged. Read by `engram_store_transient`.
    pub(crate) refusal_code: Option<String>,
    /// Structured store-open cause, qualified by the readiness decoder.
    /// CLI error prose and control reply messages never populate this field.
    pub(crate) refusal_kind: Option<String>,
    pub(crate) message: String,
    /// The request field a registered-verification refusal names
    /// (`verification_record_refused` with `details.field`), as a safe label
    /// kept out of `message` (`EngramControlErrorWire::into_transport_error`).
    pub(crate) refused_field: Option<String>,
    /// The process this call meant to run never started, so it cannot have
    /// written anything. Only a failed spawn says so: every later failure
    /// leaves a write's outcome open. It also marks Engram control as
    /// unavailable (`EngramCausalFailure::control_availability`).
    pub(crate) process_never_started: bool,
    /// The control transport failed before it handed any byte of this
    /// call's request frame to the control worker, the only writer to the
    /// process (`engram_control_transport.rs`). The request provably never
    /// reached Engram, so its remote application is "not started"; unlike
    /// `process_never_started` it says nothing about control availability,
    /// so an exhausted budget or a worker race stays a hold.
    pub(crate) request_never_sent: bool,
    /// The control sidecar this failure was observed on, when the transport
    /// saw one (`engram_control_transport.rs`); `None` is never filled in.
    pub(crate) control_process: Option<EngramControlProcessEvidence>,
}

impl EngramTransportError {
    pub(crate) fn with_control_process(mut self, evidence: EngramControlProcessEvidence) -> Self {
        self.control_process = Some(evidence);
        self
    }

    pub(crate) fn local_state(message: impl Into<String>) -> Self {
        Self {
            kind: EngramTransportErrorKind::LocalState,
            causal_failure: None,
            code: Some("control_binding_unavailable".to_owned()),
            refusal_code: None,
            refusal_kind: None,
            message: message.into(),
            refused_field: None,
            process_never_started: true,
            request_never_sent: false,
            control_process: None,
        }
    }

    pub(crate) fn deadline(message: impl Into<String>) -> Self {
        Self {
            kind: EngramTransportErrorKind::Deadline,
            causal_failure: None,
            code: Some("deadline_exceeded".to_owned()),
            refusal_code: None,
            refusal_kind: None,
            message: message.into(),
            refused_field: None,
            process_never_started: false,
            request_never_sent: false,
            control_process: None,
        }
    }

    pub(crate) fn deadline_before_launch(message: impl Into<String>) -> Self {
        Self {
            process_never_started: true,
            request_never_sent: true,
            ..Self::deadline(message)
        }
    }

    pub(crate) fn transport(message: impl Into<String>) -> Self {
        Self {
            kind: EngramTransportErrorKind::Transport,
            causal_failure: None,
            code: Some("control_unavailable".to_owned()),
            refusal_code: None,
            refusal_kind: None,
            message: message.into(),
            refused_field: None,
            process_never_started: false,
            request_never_sent: false,
            control_process: None,
        }
    }

    /// Marks a failure the control transport saw before it handed any byte
    /// of this call's request frame to the control worker, which is the
    /// only writer to the process: the request provably never reached
    /// Engram. The error's kind is kept for `counts_for_circuit_breaker`
    /// and `disables_session`, and control availability is unchanged; only
    /// the remote application becomes "not started".
    pub(crate) fn request_never_sent(mut self) -> Self {
        self.request_never_sent = true;
        self
    }

    /// Whether the host stopped this request before handing it to the
    /// control transport (a guard, a preparation or a budget check), so it
    /// is unsent and is no evidence about the transport's health. A
    /// failure the transport itself proved unsent (`request_never_sent`)
    /// is not one: it still counts for the circuit and backoff, and a
    /// protocol failure still disables the session.
    #[cfg(test)]
    pub(crate) fn stopped_before_transport(&self) -> bool {
        !self.request_never_sent
            && self.causal_failure.as_ref().is_some_and(|cause| {
                cause.remote_application == EngramRemoteApplication::NotStarted
                    && !cause.transport_unsent
            })
    }

    /// A transport failure before any process existed.
    pub(crate) fn spawn_failed(message: impl Into<String>) -> Self {
        Self {
            process_never_started: true,
            control_process: None,
            ..Self::transport(message)
        }
    }

    pub(crate) fn protocol(message: impl Into<String>) -> Self {
        Self {
            kind: EngramTransportErrorKind::Protocol,
            causal_failure: None,
            code: Some("unknown_control_schema".to_owned()),
            refusal_code: None,
            refusal_kind: None,
            message: message.into(),
            refused_field: None,
            process_never_started: false,
            request_never_sent: false,
            control_process: None,
        }
    }

    pub(crate) fn remote(error: EngramControlErrorBody) -> Self {
        Self {
            kind: EngramTransportErrorKind::Remote,
            causal_failure: None,
            code: Some(error.code),
            refusal_code: None,
            refusal_kind: None,
            message: error.message,
            refused_field: None,
            process_never_started: false,
            request_never_sent: false,
            control_process: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn counts_for_circuit_breaker(&self) -> bool {
        matches!(
            self.kind,
            EngramTransportErrorKind::Deadline | EngramTransportErrorKind::Transport
        ) || self.is_control_startup_busy()
    }

    #[cfg(test)]
    pub(crate) fn disables_session(&self) -> bool {
        self.kind == EngramTransportErrorKind::Protocol
            || (self.kind == EngramTransportErrorKind::Remote
                && matches!(
                    self.code.as_deref(),
                    Some(
                        "control_unavailable"
                            | "store_corrupt"
                            | "unknown_control_schema"
                            | "control_policy_missing"
                            | "work_claim_mismatch"
                            | ENGRAM_CONTROL_STARTUP_REFUSED_CODE
                    )
                ))
    }

    pub(crate) fn keeps_control_process_alive(&self) -> bool {
        self.kind == EngramTransportErrorKind::Remote && !self.is_terminal_control_startup()
    }

    /// Engram's terminal control-startup reply: the one error line its control
    /// process writes in answer to the first frame before it exits nonzero,
    /// with that frame never dispatched. Matched on the remote code alone,
    /// never on the message. Startup itself may have written, so this is
    /// neither a process that never started nor a certified non-application.
    fn is_terminal_control_startup(&self) -> bool {
        self.kind == EngramTransportErrorKind::Remote
            && matches!(
                self.code.as_deref(),
                Some(ENGRAM_CONTROL_STARTUP_BUSY_CODE | ENGRAM_CONTROL_STARTUP_REFUSED_CODE)
            )
    }

    /// A busy control startup: the store could not be acquired. Not fatal; it
    /// takes the recovery an unavailable control takes (a backoff, then the
    /// bounded status/rebind re-admission of the exact retained request).
    #[cfg(test)]
    fn is_control_startup_busy(&self) -> bool {
        self.kind == EngramTransportErrorKind::Remote
            && self.code.as_deref() == Some(ENGRAM_CONTROL_STARTUP_BUSY_CODE)
    }
}

/// Engram's terminal control-startup codes (`is_terminal_control_startup`).
const ENGRAM_CONTROL_STARTUP_BUSY_CODE: &str = "control_startup_busy";
const ENGRAM_CONTROL_STARTUP_REFUSED_CODE: &str = "control_startup_refused";

impl std::fmt::Display for EngramTransportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct EngramConnectionConfig {
    pub(crate) binary_path: PathBuf,
    pub(crate) project_file: PathBuf,
    pub(crate) home: PathBuf,
    pub(crate) project_root: PathBuf,
    pub(crate) actor_id: String,
    pub(crate) actor_context: Option<String>,
    pub(crate) session_id: String,
}

pub(crate) trait EngramControlTransport: Send + Sync {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError>;

    /// One bounded readiness read supplies the entire accounting policy tuple
    /// and rechecks the selected store. It is a snapshot, never a policy lease.
    fn read_observation_policy(
        &self,
        connection: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey,
        timeout: Duration,
        host_workdir: &FsPath,
    ) -> Result<EngramObservationPolicyBasis, EngramTransportError> {
        read_engram_observation_policy(connection, store, timeout, host_workdir)
    }

    /// The work binding the session should be bound with. `preference` names
    /// the binding it is bound with now, which the selection keeps while that
    /// claim is still bindable, and the one Engram refused as stale, which it
    /// leaves out (`select_engram_held_binding`).
    fn read_work_binding(
        &self,
        _connection: &EngramConnectionConfig,
        _preference: EngramBindingPreference<'_>,
        _timeout: Duration,
    ) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        Ok(None)
    }

    /// Every claim the session holds (`read_engram_held_claims_from_cli`),
    /// which a work's source root is named against and ended by
    /// (`engram_source_roots.rs`). A transport that cannot list them fails,
    /// so no root is named and none ends on its account.
    fn read_held_claims(
        &self,
        _connection: &EngramConnectionConfig,
        _timeout: Duration,
    ) -> std::result::Result<EngramHeldClaims, EngramTransportError> {
        Err(EngramTransportError::protocol(
            "this Engram transport does not list held claims",
        ))
    }

    fn shutdown_session(&self, session_id: &str);

    /// Whether this transport owns a live, published control child to clean
    /// up. A childless transport's capability generation is not a process.
    fn has_live_child(&self, _session_id: &str) -> bool {
        false
    }

    /// Which child currently serves `session_id`'s connection: a number that
    /// changes on every spawn or respawn, or none while no live child does.
    /// A capability read under one generation says nothing about another
    /// (`engram_capabilities.rs`). A transport with no child lifecycle has
    /// one connection for its whole life.
    fn connection_generation(&self, _session_id: &str) -> Option<u64> {
        Some(0)
    }

    /// The reset's shared budget does not certify process cleanup. Production
    /// retains the exact retiring child in its existing registry on timeout.
    fn shutdown_session_until(
        &self,
        _session_id: &str,
        _clock: &EngramBudgetClock,
        _deadline: std::time::Instant,
    ) -> Result<(), EngramTransportError> {
        Err(EngramTransportError::local_state(
            "This control transport has no bounded cleanup owner",
        ))
    }
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct RecordedEngramControlRequest {
    pub(crate) connection: EngramConnectionConfig,
    pub(crate) request: Value,
}

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_ENGRAM_BOOT_TRANSPORT: std::cell::RefCell<Option<Arc<dyn EngramControlTransport>>> =
        std::cell::RefCell::new(None);
    /// The budget clock a test boot chooses before its own recovery runs.
    pub(crate) static TEST_ENGRAM_BOOT_BUDGET_CLOCK: std::cell::RefCell<Option<EngramBudgetClock>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) enum ScriptedEngramControlResponse {
    Reply(std::result::Result<Value, EngramTransportError>),
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct ScriptedEngramControlTransport {
    pub(crate) named_roots: Mutex<Option<TestEngramNamedRoots>>,
    pub(crate) requests: Mutex<Vec<RecordedEngramControlRequest>>,
    pub(crate) responses: Mutex<VecDeque<ScriptedEngramControlResponse>>,
    pub(crate) work_bindings: Mutex<
        VecDeque<std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError>>,
    >,
    /// Explicit fixture claim association used after any scripted reads.
    /// The default is no claim, preserving exhaustion in negative tests.
    current_work_binding: Mutex<Option<EngramControlWorkBinding>>,
    /// What each held-claims read returns, in order; the last one repeats,
    /// and none scripted fails the read.
    pub(crate) held_claims:
        Mutex<VecDeque<std::result::Result<EngramHeldClaims, EngramTransportError>>>,
    pub(crate) held_claim_reads: std::sync::atomic::AtomicUsize,
    held_claim_hook: Mutex<Option<Box<dyn FnOnce(Duration) + Send>>>,
    /// What each `named_root_sighting_read` returns, in order; with none
    /// scripted, the read answers that no root is bound
    /// (`scripted_no_root_sighting`).
    pub(crate) sighting_reads: Mutex<VecDeque<std::result::Result<Value, EngramTransportError>>>,
    pub(crate) shutdowns: Mutex<Vec<String>>,
    /// Errors the next `read_observation_policy` probes return, in order,
    /// before the readiness model answers; and how many probes it answered.
    pub(crate) observation_policy_errors: Mutex<VecDeque<EngramTransportError>>,
    observation_policy_reads: std::sync::atomic::AtomicUsize,
}

/// A version-1 `named_root_sighting_read` answer binding no root, for the
/// work, run and cut the read asked about, in the project its connection's
/// declaration names, as the real control process routes it.
#[cfg(test)]
pub(crate) fn scripted_no_root_sighting(
    connection: &EngramConnectionConfig,
    work_ref: &str,
    run_id: &str,
    run_cut: Option<i64>,
) -> Value {
    let project_id = fs::read_to_string(&connection.project_file)
        .map(|declaration| declaration.trim().to_owned())
        .unwrap_or_default();
    let cut = run_cut.unwrap_or(0);
    json!({
        "schema_version": 1, "project_id": project_id, "work_id": work_ref, "run_id": run_id,
        "read_cut": cut, "head_cut": cut, "current_binding": null, "binding_changed": false,
        "root": {"state": "none"}
    })
}

#[cfg(test)]
impl ScriptedEngramControlTransport {
    pub(crate) fn new(
        responses: impl IntoIterator<Item = ScriptedEngramControlResponse>,
    ) -> Arc<Self> {
        Arc::new(Self {
            named_roots: Mutex::new(None),
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(responses.into_iter().collect()),
            work_bindings: Mutex::new(VecDeque::new()),
            current_work_binding: Mutex::new(None),
            held_claims: Mutex::new(VecDeque::new()),
            held_claim_reads: std::sync::atomic::AtomicUsize::new(0),
            held_claim_hook: Mutex::new(None),
            sighting_reads: Mutex::new(VecDeque::new()),
            shutdowns: Mutex::new(Vec::new()),
            observation_policy_errors: Mutex::new(VecDeque::new()),
            observation_policy_reads: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Scripts what the next `named_root_sighting_read`s return, in order.
    pub(crate) fn script_sighting_reads(
        &self,
        reads: impl IntoIterator<Item = std::result::Result<Value, EngramTransportError>>,
    ) {
        self.sighting_reads
            .lock()
            .expect("scripted Engram sighting reads mutex poisoned")
            .extend(reads);
    }

    pub(crate) fn set_current_work_binding(&self, binding: Option<EngramControlWorkBinding>) {
        *self
            .current_work_binding
            .lock()
            .expect("scripted current binding mutex poisoned") = binding;
    }

    /// Replaces earlier held-claim reads; the last read repeats until more are scripted.
    pub(crate) fn replace_held_claims(
        &self,
        reads: impl IntoIterator<Item = std::result::Result<EngramHeldClaims, EngramTransportError>>,
    ) {
        let mut held_claims = self
            .held_claims
            .lock()
            .expect("scripted Engram held claims mutex poisoned");
        held_claims.clear();
        held_claims.extend(reads);
    }

    pub(crate) fn held_claim_reads(&self) -> usize {
        self.held_claim_reads
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn on_next_held_claim_read(&self, hook: impl FnOnce(Duration) + Send + 'static) {
        *self
            .held_claim_hook
            .lock()
            .expect("scripted held hook mutex poisoned") = Some(Box::new(hook));
    }

    pub(crate) fn requests(&self) -> Vec<RecordedEngramControlRequest> {
        self.requests
            .lock()
            .expect("scripted Engram requests mutex poisoned")
            .clone()
    }

    pub(crate) fn shutdowns(&self) -> Vec<String> {
        self.shutdowns
            .lock()
            .expect("scripted Engram shutdowns mutex poisoned")
            .clone()
    }
}

#[cfg(test)]
impl EngramControlTransport for ScriptedEngramControlTransport {
    fn read_observation_policy(
        &self,
        _connection: &EngramConnectionConfig,
        expected_store: &EngramAuthorityStoreKey,
        _timeout: Duration,
        _host_workdir: &FsPath,
    ) -> Result<EngramObservationPolicyBasis, EngramTransportError> {
        self.observation_policy_reads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Some(error) = self.observation_policy_errors.lock().unwrap().pop_front() {
            return Err(error);
        }
        let mut roots = self.named_roots.lock().unwrap();
        let roots = roots.as_mut().ok_or_else(|| {
            EngramTransportError::protocol("no observation readiness model configured")
        })?;
        if roots
            .project_id
            .as_deref()
            .unwrap_or("github.com/example/source-root")
            != expected_store.project_id
        {
            return Err(EngramTransportError::protocol(
                "observation readiness model belongs to another project",
            ));
        }
        Ok(EngramObservationPolicyBasis::AccountIfEligible {
            project_policy_epoch: 1,
            policy: "a".repeat(32),
            obligation_rule_set: "b".repeat(32),
        })
    }

    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        _timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        self.requests
            .lock()
            .expect("scripted Engram requests mutex poisoned")
            .push(RecordedEngramControlRequest {
                connection: connection.clone(),
                request: serde_json::to_value(request).expect("Engram request should serialize"),
            });
        if let EngramControlRequest::NamedRootSightingRead {
            work_ref,
            run_id,
            run_cut,
        } = request
        {
            return self
                .sighting_reads
                .lock()
                .expect("scripted Engram sighting reads mutex poisoned")
                .pop_front()
                .unwrap_or_else(|| {
                    Ok(scripted_no_root_sighting(
                        connection, work_ref, run_id, *run_cut,
                    ))
                });
        }
        if let Some(roots) = self.named_roots.lock().unwrap().as_mut() {
            if let Some(reply) = roots.request(&connection.session_id, request) {
                return reply;
            }
        }
        if matches!(
            request,
            EngramControlRequest::AcceptanceVerificationRead { .. }
        ) {
            // This general fixture models the historical producer. Complete
            // private reads use their own explicitly scripted page transport.
            return Err(EngramTransportError::remote(EngramControlErrorBody {
                code: "unsupported_operation".to_owned(),
                message: "private verification metadata is not scripted".to_owned(),
            }));
        }
        let response = self
            .responses
            .lock()
            .expect("scripted Engram responses mutex poisoned")
            .pop_front()
            .unwrap_or_else(|| {
                ScriptedEngramControlResponse::Reply(Err(EngramTransportError::protocol(
                    "scripted Engram transport has no response for request",
                )))
            });
        let ScriptedEngramControlResponse::Reply(reply) = response;
        reply
    }

    fn read_work_binding(
        &self,
        _connection: &EngramConnectionConfig,
        _preference: EngramBindingPreference<'_>,
        _timeout: Duration,
    ) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.work_bindings
            .lock()
            .expect("scripted Engram work bindings mutex poisoned")
            .pop_front()
            .unwrap_or_else(|| {
                Ok(self
                    .current_work_binding
                    .lock()
                    .expect("scripted current binding mutex poisoned")
                    .clone())
            })
    }

    fn read_held_claims(
        &self,
        _connection: &EngramConnectionConfig,
        timeout: Duration,
    ) -> std::result::Result<EngramHeldClaims, EngramTransportError> {
        self.held_claim_reads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let hook = self
            .held_claim_hook
            .lock()
            .expect("scripted held hook mutex poisoned")
            .take();
        if let Some(hook) = hook {
            hook(timeout);
        }
        let mut reads = self
            .held_claims
            .lock()
            .expect("scripted Engram held claims mutex poisoned");
        match reads.len() {
            0 => Err(EngramTransportError::protocol(
                "no held claims scripted for this test transport",
            )),
            1 => reads.front().cloned().expect("one scripted read"),
            _ => reads.pop_front().expect("a scripted read"),
        }
    }

    fn shutdown_session(&self, session_id: &str) {
        self.shutdowns
            .lock()
            .expect("scripted Engram shutdowns mutex poisoned")
            .push(session_id.to_owned());
    }

    fn shutdown_session_until(
        &self,
        session_id: &str,
        _clock: &EngramBudgetClock,
        _deadline: std::time::Instant,
    ) -> Result<(), EngramTransportError> {
        self.shutdown_session(session_id);
        Ok(())
    }
}

pub(crate) struct EngramProcessTree {
    pub(crate) inner: TerminalProcessTree,
    /// Whether the tree has been signalled, and what that signal left to
    /// confirm after the reap (`TerminalGroupSignal`).
    pub(crate) terminated: Mutex<Option<TerminalGroupSignal>>,
}

impl EngramProcessTree {
    pub(crate) fn attach(process: &Arc<SharedChild>) -> Result<Self> {
        Ok(Self {
            inner: TerminalProcessTree::attach(process)?,
            terminated: Mutex::new(None),
        })
    }

    pub(crate) fn terminate(&self, process: &Arc<SharedChild>) -> Result<()> {
        let mut terminated = self
            .terminated
            .lock()
            .expect("Engram process-tree mutex poisoned");
        if terminated.is_some() {
            return Ok(());
        }
        *terminated = Some(self.inner.kill_before_reap(process, "Engram control")?);
        Ok(())
    }

    /// Whether the tree, terminated and then reaped by its worker, is gone. A
    /// group that refused its signal because every member had already exited
    /// must be absent now; any other terminated tree is.
    pub(crate) fn confirmed_after_reap(&self) -> bool {
        match *self
            .terminated
            .lock()
            .expect("Engram process-tree mutex poisoned")
        {
            None => false,
            Some(TerminalGroupSignal::Delivered) => true,
            #[cfg(not(windows))]
            Some(TerminalGroupSignal::ConfirmAbsenceAfterReap(pgid)) => {
                terminal_process_group_absent_after_reap(pgid)
            }
            #[cfg(windows)]
            Some(TerminalGroupSignal::ConfirmAbsenceAfterReap(_)) => false,
        }
    }

    pub(crate) fn resume_after_attach(&self, process: &Arc<SharedChild>) -> Result<()> {
        self.inner.resume_after_attach(process)
    }
}

pub(crate) fn run_engram_json_command_with_lock_retry(
    connection: &EngramConnectionConfig,
    args: &[&str],
    timeout: Duration,
    label: &str,
    host_workdir: &FsPath,
) -> std::result::Result<Value, EngramTransportError> {
    let output =
        run_engram_cli_command_with_lock_retry(connection, args, timeout, label, host_workdir)?;
    decode_engram_cli_json(output, label)
}

/// Exit status and captured streams of one bounded Engram CLI call. A non-zero
/// exit is an observation here, not a transport failure: a caller that relays
/// Engram's refusal needs the text, and one that only wants JSON folds it into
/// an error itself.
#[derive(Clone, Debug)]
pub(crate) struct EngramCliOutput {
    pub(crate) success: bool,
    /// How the process ended, as data. `status` below is only for messages.
    pub(crate) exit: EngramCliExit,
    pub(crate) status: String,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

/// How a CLI process ended. Only a process that ran to its own exit can be
/// taken at its word about what it did; one that was killed or crashed may
/// have stopped anywhere, a committed write included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EngramCliExit {
    /// A byte-sized exit code; this includes runtime failures such as panic
    /// code 101 and does not by itself prove a transactional refusal.
    Code(u8),
    /// A signal, or a status outside the exit codes a program returns on its
    /// own (on Windows a crash reports an NTSTATUS value such as 0xC0000005).
    Abnormal,
}

impl EngramCliExit {
    pub(crate) fn from_status(status: &std::process::ExitStatus) -> Self {
        // `code()` is `None` for a Unix signal; a crash status does not fit a byte.
        status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .map_or(Self::Abnormal, Self::Code)
    }

    pub(crate) fn code(self) -> Option<u8> {
        match self {
            Self::Code(code) => Some(code),
            Self::Abnormal => None,
        }
    }
}

impl EngramCliOutput {
    /// What the process said about a failure: stderr, else stdout.
    pub(crate) fn failure_detail(&self) -> String {
        let detail = String::from_utf8_lossy(&self.stderr).trim().to_owned();
        if detail.is_empty() {
            String::from_utf8_lossy(&self.stdout).trim().to_owned()
        } else {
            detail
        }
    }

    /// A lock diagnostic is eligible for retry only with Engram's ordinary
    /// failure exit. Runtime failures (including Rust panic code 101) can
    /// mention a lock after a write and must not imply that nothing ran.
    pub(crate) fn reports_locked_store(&self) -> bool {
        !self.success
            && self.exit.code() == Some(1)
            && self
                .failure_detail()
                .to_ascii_lowercase()
                .contains("database is locked")
    }
}

/// The same one-retry SQLite lock policy as the JSON reader, for a caller that
/// keeps the exit observation. Only safe for reads and for writes Engram
/// replays under an idempotency key.
pub(crate) fn run_engram_cli_command_with_lock_retry(
    connection: &EngramConnectionConfig,
    args: &[&str],
    timeout: Duration,
    label: &str,
    host_workdir: &FsPath,
) -> std::result::Result<EngramCliOutput, EngramTransportError> {
    let timeout = engram_operation_timeout(timeout);
    if timeout.is_zero() {
        return Err(EngramTransportError::deadline_before_launch(
            "Engram admission budget exhausted before process launch",
        ));
    }
    let started = std::time::Instant::now();
    let mut warned = false;
    retry_engram_cli_command_with_budget(
        |_| {
            run_engram_cli_command_until(
                connection,
                args,
                started,
                timeout,
                label,
                host_workdir,
                &mut warned,
            )
        },
        || started.elapsed(),
        timeout,
        std::thread::sleep,
    )
}

pub(crate) fn retry_engram_cli_command_with_budget(
    mut run: impl FnMut(Duration) -> std::result::Result<EngramCliOutput, EngramTransportError>,
    mut elapsed: impl FnMut() -> Duration,
    timeout: Duration,
    sleep: impl FnOnce(Duration),
) -> std::result::Result<EngramCliOutput, EngramTransportError> {
    retry_engram_cli_command_on_locked_store(
        || {
            let remaining = timeout
                .checked_sub(elapsed())
                .filter(|remaining| !remaining.is_zero())
                .ok_or_else(|| {
                    EngramTransportError::deadline("Engram call budget exhausted during lock retry")
                })?;
            run(remaining)
        },
        sleep,
    )
}

/// The retry policy apart from the process it runs: exactly one more attempt,
/// after the delay, and only when the first reported a locked store. The
/// second result stands whatever it is.
pub(crate) fn retry_engram_cli_command_on_locked_store(
    mut run: impl FnMut() -> std::result::Result<EngramCliOutput, EngramTransportError>,
    sleep: impl FnOnce(Duration),
) -> std::result::Result<EngramCliOutput, EngramTransportError> {
    let first = run()?;
    if first.reports_locked_store() {
        sleep(ENGRAM_WORK_BINDING_LOCK_RETRY_DELAY);
        return run();
    }
    Ok(first)
}

#[cfg(all(test, windows))]
pub(crate) fn run_engram_json_command(
    connection: &EngramConnectionConfig,
    args: &[&str],
    timeout: Duration,
    label: &str,
    host_workdir: &FsPath,
) -> std::result::Result<Value, EngramTransportError> {
    let output = run_engram_cli_command(connection, args, timeout, label, host_workdir)?;
    decode_engram_cli_json(output, label)
}

pub(crate) fn decode_engram_cli_json(
    output: EngramCliOutput,
    label: &str,
) -> std::result::Result<Value, EngramTransportError> {
    if !output.success {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let mut error = EngramTransportError::transport(if detail.is_empty() {
            format!("Engram {label} exited with {}", output.status)
        } else {
            format!("Engram {label} failed: {detail}")
        });
        error.refusal_code = engram_cli_refusal_code(&output);
        return Err(error);
    }
    engram_decode_slice(&output.stdout, &format!("invalid Engram {label} response"))
}

/// Engram's refusal code from a failed `--json` CLI call: its stderr is one
/// JSON value `{"error": {"code": ...}}`. Only a process that ran to its own
/// exit is taken at its word, and only a plain code (lowercase ASCII letters,
/// digits and `_`, at most 64 bytes) is kept. Anything else gives `None`.
fn engram_cli_refusal_code(output: &EngramCliOutput) -> Option<String> {
    if output.exit == EngramCliExit::Abnormal {
        return None;
    }
    let value: Value = serde_json::from_slice(output.stderr.trim_ascii()).ok()?;
    let code = value.get("error")?.get("code")?.as_str()?;
    (!code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'))
    .then(|| code.to_owned())
}

/// Whether `run_engram_cli_command` failed before any Engram process ran, so
/// its command provably never reached Engram: the launch was refused while
/// the command was built (a `local_state` launch-path refusal or a
/// `spawn_failed` program resolution in `engram_host_launch.rs`) or at the
/// spawn itself (`spawn_failed`). Every later failure, from attaching the
/// process tree to reading its output, is a transport or deadline error with
/// an unknown outcome: on Unix the child already runs before the attach.
pub(crate) fn engram_cli_command_never_ran(error: &EngramTransportError) -> bool {
    error.process_never_started || error.kind == EngramTransportErrorKind::LocalState
}

// The process wait remains in the CLI owner; injectable clock and polling
// boundaries let fixture calls exercise slow responses without real delays.
pub(crate) fn wait_engram_cli_command<T>(
    mut poll: impl FnMut() -> std::io::Result<Option<T>>,
    mut elapsed: impl FnMut() -> Duration,
    mut pause: impl FnMut(),
    timeout: Duration,
    warning_after: Duration,
    label: &str,
    mut warn: impl FnMut(Duration),
) -> std::result::Result<T, EngramTransportError> {
    let mut warned = false;
    loop {
        let spent = elapsed();
        if !warned && spent >= warning_after {
            warned = true;
            warn(spent);
        }
        match poll() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if spent < timeout => pause(),
            Ok(None) => {
                return Err(EngramTransportError::deadline(format!(
                    "Engram {label} exceeded {} ms",
                    timeout.as_millis()
                )));
            }
            Err(error) => {
                return Err(EngramTransportError::transport(format!(
                    "failed waiting for Engram {label}: {error}"
                )));
            }
        }
    }
}

pub(crate) fn run_engram_cli_command(
    connection: &EngramConnectionConfig,
    args: &[&str],
    timeout: Duration,
    label: &str,
    host_workdir: &FsPath,
) -> std::result::Result<EngramCliOutput, EngramTransportError> {
    run_engram_cli_command_until(
        connection,
        args,
        std::time::Instant::now(),
        engram_operation_timeout(timeout),
        label,
        host_workdir,
        &mut false,
    )
}

fn run_engram_cli_command_until(
    connection: &EngramConnectionConfig,
    args: &[&str],
    started: std::time::Instant,
    timeout: Duration,
    label: &str,
    host_workdir: &FsPath,
    warned: &mut bool,
) -> std::result::Result<EngramCliOutput, EngramTransportError> {
    if started.elapsed() >= timeout {
        return Err(EngramTransportError::deadline_before_launch(
            "Engram admission budget exhausted before process launch",
        ));
    }
    let mut command = engram_host_command(
        &connection.binary_path,
        &connection.project_root,
        &connection.project_file,
        &connection.home,
        host_workdir,
    )?;
    configure_terminal_process_tree(&mut command);
    apply_engram_connection_environment(&mut command, connection);
    let mut child = command
        .arg("--project-file")
        .arg(&connection.project_file)
        .arg("--home")
        .arg(&connection.home)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            EngramTransportError::spawn_failed(format!("failed spawning Engram {label}: {error}"))
        })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        EngramTransportError::transport(format!("Engram {label} stdout is unavailable"))
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        EngramTransportError::transport(format!("Engram {label} stderr is unavailable"))
    })?;
    let process = Arc::new(SharedChild::new(child).map_err(|error| {
        EngramTransportError::transport(format!("failed sharing Engram {label}: {error}"))
    })?);
    let kill_label = format!("Engram {label}");
    let process_tree = EngramProcessTree::attach(&process).map_err(|error| {
        let _ = kill_child_process(&process, &kill_label);
        let _ = process.wait();
        EngramTransportError::transport(format!(
            "failed preparing Engram {label} process tree: {error:#}"
        ))
    })?;
    process_tree
        .resume_after_attach(&process)
        .map_err(|error| {
            let _ = process_tree.terminate(&process);
            let _ = process.wait();
            EngramTransportError::transport(format!("failed resuming Engram {label}: {error:#}"))
        })?;
    let stdout_reader = spawn_engram_cli_output_reader(stdout, label);
    let stderr_reader = spawn_engram_cli_output_reader(stderr, label);
    let warning_after = if *warned {
        Duration::MAX
    } else {
        ENGRAM_CALL_WARNING_AFTER
    };
    let status = wait_engram_cli_command(
        || process.try_wait(),
        || started.elapsed(),
        || std::thread::sleep(Duration::from_millis(5)),
        timeout,
        warning_after,
        label,
        |elapsed| {
            *warned = true;
            eprintln!(
                "engram warning> {label} still running after {} ms",
                elapsed.as_millis()
            );
        },
    );
    if status.is_err() {
        let _ = process_tree.terminate(&process);
        let _ = process.wait();
    }
    let stdout = join_engram_cli_output(stdout_reader, label, "stdout")?;
    let stderr = join_engram_cli_output(stderr_reader, label, "stderr")?;
    let status = status?;
    Ok(EngramCliOutput {
        success: status.success(),
        exit: EngramCliExit::from_status(&status),
        status: status.to_string(),
        stdout,
        stderr,
    })
}

pub(crate) fn apply_engram_connection_environment(
    command: &mut Command,
    connection: &EngramConnectionConfig,
) {
    command
        .env(ENGRAM_HOME_ENV, &connection.home)
        .env(ENGRAM_ACTOR_ID_ENV, &connection.actor_id)
        .env(ENGRAM_SESSION_ID_ENV, &connection.session_id);
    if let Some(actor_context) = connection.actor_context.as_deref() {
        command.env(ENGRAM_ACTOR_CONTEXT_ENV, actor_context);
    } else {
        command.env_remove(ENGRAM_ACTOR_CONTEXT_ENV);
    }
    // Test-only: the child's temporary files stay in its Engram home
    // (src/engram_test_child_temp.rs).
    #[cfg(test)]
    for (name, value) in engram_test_child_temp_env(&connection.home) {
        command.env(name, value);
    }
}

fn stop_engram_doctor_before_reap(tree: &EngramProcessTree, process: &Arc<SharedChild>) {
    if let Err(error) = tree.terminate(process) {
        eprintln!("engram warning> failed terminating doctor process tree: {error:#}");
    }
    reap_engram_doctor_in_background(process);
}

fn reap_engram_doctor_in_background(process: &Arc<SharedChild>) {
    // Reaping is necessary, but a failed termination must not turn a bounded
    // API call into an unbounded wait. The waiter owns the handle until exit.
    let process = Arc::clone(process);
    std::thread::spawn(move || {
        if let Err(error) = process.wait() {
            eprintln!("engram warning> failed reaping doctor process: {error}");
        }
    });
}

pub(crate) fn read_engram_doctor_output(reader: impl std::io::Read) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    std::io::Read::take(reader, (ENGRAM_DOCTOR_MAX_OUTPUT_BYTES + 1) as u64)
        .read_to_end(&mut output)?;
    if output.len() > ENGRAM_DOCTOR_MAX_OUTPUT_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Engram doctor output exceeds TermAl's {} byte per-stream capture budget; a larger valid report is not supported",
                ENGRAM_DOCTOR_MAX_OUTPUT_BYTES
            ),
        ));
    }
    Ok(output)
}

fn start_engram_doctor_reader(
    reader: impl std::io::Read + Send + 'static,
    stream: &'static str,
) -> mpsc::Receiver<std::io::Result<Vec<u8>>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let result = read_engram_doctor_output(reader);
        if let Err(error) = &result {
            eprintln!("engram warning> failed reading doctor {stream}: {error}");
        }
        let _ = sender.send(result);
    });
    receiver
}

pub(crate) fn collect_engram_doctor_output(
    receiver: mpsc::Receiver<std::io::Result<Vec<u8>>>,
    stream: &str,
    deadline: std::time::Instant,
) -> std::result::Result<Vec<u8>, ApiError> {
    match receiver.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
        Ok(result) => result.map_err(|error| {
            ApiError::bad_request(format!("failed reading Engram doctor {stream}: {error}"))
        }),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(ApiError::bad_request(format!(
            "Engram doctor {stream} output collection exceeded the enablement deadline"
        ))),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(ApiError::bad_request(format!(
            "Engram doctor {stream} reader stopped without returning output"
        ))),
    }
}

/// `label` names the operation in the overflow diagnostic, as it does in every
/// other failure of the same call.
pub(crate) fn read_engram_cli_output(
    reader: impl std::io::Read,
    label: &str,
) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    std::io::Read::take(reader, (ENGRAM_CONTROL_MAX_FRAME_BYTES + 1) as u64)
        .read_to_end(&mut output)?;
    if output.len() > ENGRAM_CONTROL_MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Engram {label} output exceeds the maximum control frame"),
        ));
    }
    Ok(output)
}

pub(crate) fn spawn_engram_cli_output_reader(
    reader: impl std::io::Read + Send + 'static,
    label: &str,
) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>> {
    let label = label.to_owned();
    std::thread::spawn(move || read_engram_cli_output(reader, &label))
}

pub(crate) fn join_engram_cli_output(
    reader: std::thread::JoinHandle<std::io::Result<Vec<u8>>>,
    label: &str,
    stream: &str,
) -> std::result::Result<Vec<u8>, EngramTransportError> {
    reader
        .join()
        .map_err(|_| {
            EngramTransportError::transport(format!("Engram {label} {stream} reader panicked"))
        })?
        .map_err(|error| {
            EngramTransportError::transport(format!(
                "failed reading Engram {label} {stream}: {error}"
            ))
        })
}

#[derive(Clone)]
pub(crate) struct EngramHostAdapter {
    pub(crate) host_workdir: PathBuf,
    pub(crate) transport: Arc<dyn EngramControlTransport>,
    /// What each connection's last ordinary work-session answer supports
    /// (`engram_capabilities.rs`).
    pub(crate) capabilities: Arc<EngramConnectionCapabilities>,
}

impl Default for EngramHostAdapter {
    fn default() -> Self {
        Self::new(PathBuf::new())
    }
}

impl EngramHostAdapter {
    pub(crate) fn new(host_workdir: PathBuf) -> Self {
        Self {
            transport: Arc::new(ProcessEngramControlTransport::new(host_workdir.clone())),
            host_workdir,
            capabilities: Arc::default(),
        }
    }
}

impl EngramHostAdapter {
    pub(crate) fn read_observation_policy(
        &self,
        connection: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey,
        timeout: Duration,
    ) -> Result<EngramObservationPolicyBasis, EngramTransportError> {
        self.transport.read_observation_policy(
            connection,
            store,
            engram_operation_timeout(timeout),
            &self.host_workdir,
        )
    }

    pub(crate) fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        if timeout.is_zero() {
            let mut error = EngramTransportError::deadline_before_launch(
                "Engram admission budget exhausted before process launch",
            );
            error.causal_failure = Some(EngramCausalFailure::request(&error, request));
            return Err(error);
        }
        // Every work-session answer re-reads what this connection supports,
        // under the child that answered it: the child serving before the call
        // and after it must be the same one. A respawn under the call (or a
        // first start, which no earlier reading names) leaves the answer
        // unattributed, so the connection is read afresh.
        let before = self.transport.connection_generation(&connection.session_id);
        let reply = self
            .transport
            .request(connection, request, engram_operation_timeout(timeout));
        let after = self.transport.connection_generation(&connection.session_id);
        if before.is_some() && before == after {
            self.capabilities
                .note_reply(connection, after, request, &reply);
        } else if matches!(request, EngramControlRequest::WorkSessionRead { .. }) {
            self.capabilities.forget(&connection.session_id);
        }
        reply.map_err(|mut error| {
            error.causal_failure = Some(EngramCausalFailure::request(&error, request));
            error
        })
    }

    pub(crate) fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.transport
            .read_work_binding(connection, preference, engram_operation_timeout(timeout))
    }

    // A pure identity/capability read on the ordinary child, never a control
    // bind/status fallback. The caller owns the exact store and bounded budget.
    pub(crate) fn read_work_session(
        &self,
        connection: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey,
        timeout: Duration,
    ) -> Result<String, EngramTransportError> {
        let timeout = engram_operation_timeout(timeout);
        let started = std::time::Instant::now();
        let read = || {
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(EngramTransportError::deadline(
                    "ordinary work-session read expired",
                ));
            }
            let value = self.request(
                connection,
                &EngramControlRequest::WorkSessionRead {},
                remaining,
            )?;
            if value.get("work_session").and_then(Value::as_bool) != Some(true)
                || !engram_reply_supports_host_work(&value)
                || value.get("project_id").and_then(Value::as_str)
                    != Some(store.project_id.as_str())
                || value.get("session_id").and_then(Value::as_str)
                    != Some(connection.session_id.as_str())
            {
                self.capabilities.forget(&connection.session_id);
                return Err(EngramTransportError::protocol(
                    "ordinary work-session identity differs from its connection/store",
                ));
            }
            Ok(connection.session_id.clone())
        };
        let session = read()?;
        // A cold spawn's first answer is intentionally not attributed to a
        // previously absent child. Read again under its now-known generation.
        if !self.capabilities.work_session(
            connection,
            self.transport.connection_generation(&connection.session_id),
        ) {
            return read();
        }
        Ok(session)
    }

    pub(crate) fn read_held_claims(
        &self,
        connection: &EngramConnectionConfig,
        timeout: Duration,
    ) -> std::result::Result<EngramHeldClaims, EngramTransportError> {
        self.transport
            .read_held_claims(connection, engram_operation_timeout(timeout))
    }

    pub(crate) fn verification_record_route_support(
        &self,
        connection: &EngramConnectionConfig,
    ) -> EngramVerificationRecordSupport {
        self.capabilities.work_session_verification_record(
            connection,
            self.transport.connection_generation(&connection.session_id),
        )
    }

    #[allow(dead_code)] // Capability read alone never activates delivery.
    pub(crate) fn remote_verification_record_route_support(
        &self,
        connection: &EngramConnectionConfig,
    ) -> EngramVerificationRecordSupport {
        self.capabilities.work_session_remote_verification_record(
            connection,
            self.transport.connection_generation(&connection.session_id),
        )
    }

    pub(crate) fn shutdown_session(&self, session_id: &str) {
        self.capabilities.forget(session_id);
        self.transport.shutdown_session(session_id);
    }

    pub(crate) fn shutdown_session_until(
        &self,
        session_id: &str,
        claim: &EngramProjectResetClaim,
    ) -> Result<(), EngramTransportError> {
        self.capabilities.forget(session_id);
        self.transport
            .shutdown_session_until(session_id, &claim.clock, claim.deadline)
    }
}

#[cfg(test)]
impl AppState {
    pub(crate) fn install_test_engram_transport(&self, transport: Arc<dyn EngramControlTransport>) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        inner.engram_host_adapter = Arc::new(EngramHostAdapter {
            host_workdir: engram_host_workdir(self.persistence_path.as_ref())
                .expect("fixture persistence directory"),
            transport,
            capabilities: Arc::default(),
        });
    }

    pub(crate) fn new_with_paths_and_engram_transport_for_test(
        default_workdir: String,
        persistence_path: PathBuf,
        orchestrator_templates_path: PathBuf,
        transport: Arc<dyn EngramControlTransport>,
    ) -> Result<Self> {
        TEST_ENGRAM_BOOT_TRANSPORT.with(|slot| {
            let previous = slot.replace(Some(transport));
            let result = Self::new_with_paths(
                default_workdir,
                persistence_path,
                orchestrator_templates_path,
            );
            slot.replace(previous);
            result
        })
    }

    /// As `new_with_paths_and_engram_transport_for_test`, with the state's
    /// budget clock chosen before the boot's own recovery takes a snapshot.
    pub(crate) fn new_with_paths_engram_transport_and_clock_for_test(
        default_workdir: String,
        persistence_path: PathBuf,
        orchestrator_templates_path: PathBuf,
        transport: Arc<dyn EngramControlTransport>,
        clock: EngramBudgetClock,
    ) -> Result<Self> {
        TEST_ENGRAM_BOOT_BUDGET_CLOCK.with(|slot| {
            let previous = slot.replace(Some(clock));
            let result = Self::new_with_paths_and_engram_transport_for_test(
                default_workdir,
                persistence_path,
                orchestrator_templates_path,
                transport,
            );
            slot.replace(previous);
            result
        })
    }
}

#[derive(Clone)]
pub(crate) struct EngramSessionState {
    /// Original persisted control metadata retained only for inspection and
    /// round-trip integrity. It never creates work or authorizes execution.
    pub(crate) historical_control: BTreeMap<String, Value>,
    /// An actual pending session deletion, independent of observation enablement.
    pub(crate) observation_delete_requested: bool,
    /// The ordinary work-session association, refreshed independently of turns.
    pub(crate) work_binding: Option<EngramControlWorkBinding>,
    /// An acknowledged explicit name selects only a later opening. Like
    /// work_binding, this selection is runtime-only and is not restored.
    pub(crate) pending_naming_selection: Option<EngramPendingNamingSelection>,
    // Process-local exact report readbacks; bounded by retained reports.
    pub(crate) durable_runtime_captures: BTreeSet<String>,
    /// Command facts observed under the exact ordinary trace owner.
    pub(crate) active_turn_checks: Vec<EngramTurnCheck>,
    // Pre-prompt native evidence is process-local. Restart never adopts it.
    pub(crate) declared_run_turn: Option<DeclaredRunTurn>,
    /// Launcher full gates whose result marked only their launch, carried
    /// past the turn that launched them until they settle
    /// (`engram_carried_checks.rs`). In memory; a marker of each is persisted
    /// so a restart can tell the holder the run lost its credit.
    pub(crate) carried_checks: Vec<EngramCarriedCheck>,
    /// The Codex tests of this session still running when their turn closed,
    /// carried or not (`engram_reported_ends.rs`). Kept apart from
    /// `carried_checks`: refusing or dropping a test's evidence does not end
    /// its process, so a check that starts in its worktree is overlapped by
    /// it until its end is reported or its runtime exits or is replaced, and
    /// by nothing else. In memory only: a host restart clears it, since the
    /// host can then no longer observe the process. A runtime's exit, clear
    /// or replacement does not prove the command ended either: a stated
    /// limitation (`EngramRunningCodexTest`).
    pub(crate) running_codex_tests: Vec<EngramRunningCodexTest>,
    /// The run directories carried checks of this session settled or
    /// dropped used, newest last (`engram_note_consumed_runs`). In memory.
    pub(crate) carried_consumed_runs: Vec<PathBuf>,
    /// Background full-gate launches of this session that left without a
    /// run found for them, by root key and launch time, newest last
    /// (`engram_note_unmatched_launch`), including independently observed
    /// command locations that cannot supply a source check. In memory.
    pub(crate) carried_unmatched_launches: Vec<(String, String)>,
    /// Commands running in that turn, so a check knows whether another
    /// command ran beside it, each with the directory its runtime reported,
    /// which a repeated start may leave out. In memory only.
    pub(crate) running_command_keys: BTreeMap<String, Option<String>>,
    /// The worktrees each running command of the session may write in, by
    /// its runtime key, `None` for one TermAl could not name
    /// (`engram_turn_checks.rs`): its reported directory's, or the ones its
    /// shell may be in. Resolved off the state lock as the command starts,
    /// so overlap marking under the lock knows where a command writes even
    /// when that is not the session's workdir. Kept whether or not a turn
    /// is mediated, until the command ends or the session starts its next
    /// turn; bounded (`ENGRAM_RUNNING_COMMAND_LIMIT`). In memory only.
    pub(crate) running_command_worktrees: Vec<(String, Vec<Option<String>>)>,
    /// The key of the worktree the session's workdir lies in, with the
    /// workdir it was resolved for (`engram_session_worktree`): resolved off
    /// the state lock whenever the session reports a command or an edit, so
    /// overlap marking under the lock knows where the session writes without
    /// a process-wide cache that other sessions' paths could evict. In memory
    /// only.
    pub(crate) workdir_worktree: Option<(String, EngramWriterWorkdir)>,
    /// SSH child that supplied the current remote session metadata. Path
    /// refreshes cannot replace this with the child currently installed.
    pub(crate) remote_writer_process: Option<Arc<()>>,
    /// Capture workers of the session's checks running now (source snapshots
    /// and toolchain probes, `EngramCapture::spawn`), counted by each worker
    /// until it finishes, whether or not its check is still kept. A new check
    /// starts only while fewer than `ENGRAM_CAPTURE_WORKER_LIMIT` run. In
    /// memory only.
    pub(crate) capture_workers: Arc<std::sync::atomic::AtomicUsize>,
    /// Running commands whose check a repeated start invalidated: they
    /// start no check again until they end, since a new one would begin
    /// its record after writes the old one saw. In memory only; cleared with
    /// `running_command_keys`.
    pub(crate) withheld_command_keys: BTreeSet<String>,
    /// Running commands already given an uncredited-check notice. Separate
    /// from withholding: invalidating a check need not publish a notice.
    /// In memory only; cleared with `running_command_keys`.
    pub(crate) uncredited_command_notice_keys: BTreeSet<String>,
    /// Checks a turn boundary closed with outcome Unknown because no end was
    /// reported for them, newest last, so an end reported later is told to
    /// the holder once (`engram_close_unended_checks`). Bounded
    /// (`ENGRAM_UNENDED_CLOSURE_LIMIT`); in memory only.
    pub(crate) unended_check_closures: std::collections::VecDeque<EngramUnendedClosure>,
    /// The place in the turn the next check takes, so check ids stay unique
    /// after a check is dropped. Reset with `active_turn_checks`.
    pub(crate) next_turn_check_sequence: usize,
    /// Where the shell of the session's runtime is presumed to be, for
    /// commands whose runtime reports no directory (Claude's shell keeps a
    /// `cd`), after a command moved it (`EngramShellDirectory`). In memory
    /// only; kept across turns, as the shell is.
    pub(crate) shell_directory: Option<EngramShellDirectory>,
    /// Historical checkpoint endpoint for unadmitted reporting only. Does
    /// not change the ordinary mediated comparison after a host restart.
    pub(crate) runtime_checkpoint: Option<EngramRuntimeCheckpointBasis>,
    /// Measurement taken for an evaluation request, consumed at that turn's
    /// first ordinary close. It cannot bridge a later missing baseline and
    /// creates neither an admitted turn nor an evaluation receipt.
    pub(crate) evaluation_checkpoint: Option<EngramEvaluationCheckpoint>,
    /// Observation-only history is independent of grants and admission.
    pub(crate) unadmitted_active_turn: Option<EngramRuntimeObservedTurn>,
    pub(crate) unadmitted_closing_jobs: Vec<EngramRuntimeClosingJob>,
    pub(crate) unadmitted_reports: Vec<EngramRuntimeObservationReport>,
    /// Process-local coalescing flag, never restored as a live worker.
    pub(crate) unadmitted_worker_running: bool,
    /// The unique turn reference of the last observed turn this process
    /// closed. In memory only, and never a generation number: those restart
    /// with the host, so a report from before a restart cannot pass for a
    /// later turn that reached the same number.
    pub(crate) runtime_closed_turn_ref: Option<String>,
    pub(crate) named_root: Option<EngramNamedRootState>,
    /// The new names this session gave during its current turn, one per
    /// claim, each with that turn's grant (`engram_source_roots.rs`): while
    /// the grant is still active and its turn did not open in the root, no
    /// turn has (`acceptance_named_root_sighting.rs`). In memory only.
    pub(crate) roots_named_in_turn: Vec<EngramRootNamedInTurn>,
    /// Host lines put before the agent's prompt about where its turns on
    /// claimed work are measured (`engram_source_root_line`), one per line,
    /// until the runtime accepts a prompt that carried them: a dispatch
    /// refused before that keeps them, and a line set after the prompt was
    /// built waits for the next one. Set only through
    /// `set_pending_source_root_line`. In memory only.
    pub(crate) pending_source_root_line: Option<String>,
    /// In-memory order of optional reads and published command feedback.
    /// An older asynchronous notice cannot replace a newer refusal.
    pub(crate) uncredited_feedback_revision: u64,
    /// For each retained-history repair line given to the pending lines, the
    /// authority store and work whose history it tells about
    /// (`set_pending_retained_history_repair_line`), so that only an
    /// acknowledged forward name of exactly that store and work retires it.
    /// In memory only, like the pending lines.
    pub(crate) retained_history_repair_origins: Vec<(String, EngramAuthorityStoreKey, String)>,
    /// Opt-in witness of complete notices before the prompt summary merges them.
    /// Armed only by a fixture for its own session and taken after the close.
    #[cfg(test)]
    pub(crate) test_source_root_lines: Option<Vec<String>>,
    /// The lines the prompt of the given turn generation carried, set as the
    /// prompt is built (`turn_dispatch.rs`), less any given again since
    /// (`set_pending_source_root_line`); when the runtime accepts it,
    /// exactly those lines leave the pending ones
    /// (`acknowledge_engram_source_root_line_delivery`).
    pub(crate) source_root_line_delivery: Option<(String, u64)>,
    /// The disabled declaration last told to the agent, as its hash and line
    /// (`engram_declared_tests.rs`), so one is told once per content. In
    /// memory only.
    pub(crate) declaration_disabled_told: Option<String>,
    /// Current scoped status, projected again at provider handoff. Unrelated
    /// undelivered scopes remain pending for a later return to their claim.
    pub(crate) source_root_notices: Vec<EngramSourceRootNotice>,
    // Runtime owner of the grant-free naming bind. A settings reset may
    // replace the session state; an old reply must not release a new owner.
    pub(crate) source_root_bind_owner: Option<Uuid>,
    /// Base-tier context is injected on the first prompt in this TermAl
    /// process and again after an observed agent compaction.
    pub(crate) context_nudge_pending: bool,
    pub(crate) context_nudge_in_progress: bool,
    pub(crate) context_nudge_in_progress_generation: Option<u64>,
    pub(crate) context_nudge_generation: u64,
    pub(crate) pending_context_nudge: Option<String>,
    pub(crate) context_nudge_delivery_generation: Option<u64>,
    pub(crate) context_nudge_delivery_turn_generation: Option<u64>,
    /// A compaction requests fresh orientation after any previously fetched
    /// peek snapshot has reached the runtime. This is local prompt buffering,
    /// not Engram delivery state. Cleared when that fresh read starts.
    pub(crate) context_refresh_needed: bool,
    /// Best-effort dedup of the last 64 item ids, each at most 256 bytes.
    /// Evicted, missing or oversized ids can signal again; delivery safety
    /// must therefore not depend on deduplication.
    pub(crate) signalled_compaction_item_ids: VecDeque<String>,
    /// A user Stop left the queue paused for an explicit Resume: an operator
    /// queue pause. No automatic retry passes it. Saved with the session;
    /// cleared when the pause is lifted.
    pub(crate) operator_paused: bool,
    /// The exact queue head held by user Stop. This is a local Stop fact,
    /// independent of historical control or provider delivery evidence.
    /// Saved with the session; a different head makes it stale.
    pub(crate) stopped_prompt_id: Option<String>,
}

// Raw history may contain original credentials. Diagnostics expose counts,
// never the JSON or retained request bodies.
impl std::fmt::Debug for EngramSessionState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EngramSessionState")
            .field("historical_entries", &self.historical_control.len())
            .field("work_binding", &self.work_binding)
            .field(
                "has_active_observation",
                &self.unadmitted_active_turn.is_some(),
            )
            .field("closing_observations", &self.unadmitted_closing_jobs.len())
            .field("observation_reports", &self.unadmitted_reports.len())
            .field(
                "observation_delete_requested",
                &self.observation_delete_requested,
            )
            .field("operator_paused", &self.operator_paused)
            .finish_non_exhaustive()
    }
}

impl Default for EngramSessionState {
    fn default() -> Self {
        Self {
            historical_control: BTreeMap::new(),
            observation_delete_requested: false,
            work_binding: None,
            pending_naming_selection: None,
            durable_runtime_captures: BTreeSet::new(),
            active_turn_checks: Vec::new(),
            declared_run_turn: None,
            carried_checks: Vec::new(),
            running_codex_tests: Vec::new(),
            carried_consumed_runs: Vec::new(),
            carried_unmatched_launches: Vec::new(),
            running_command_keys: BTreeMap::new(),
            running_command_worktrees: Vec::new(),
            workdir_worktree: None,
            remote_writer_process: None,
            capture_workers: Arc::default(),
            withheld_command_keys: BTreeSet::new(),
            uncredited_command_notice_keys: BTreeSet::new(),
            unended_check_closures: std::collections::VecDeque::new(),
            next_turn_check_sequence: 0,
            shell_directory: None,
            runtime_checkpoint: None,
            evaluation_checkpoint: None,
            unadmitted_active_turn: None,
            unadmitted_closing_jobs: Vec::new(),
            unadmitted_reports: Vec::new(),
            unadmitted_worker_running: false,
            runtime_closed_turn_ref: None,
            named_root: None,
            roots_named_in_turn: Vec::new(),
            pending_source_root_line: None,
            uncredited_feedback_revision: 0,
            retained_history_repair_origins: Vec::new(),
            #[cfg(test)]
            test_source_root_lines: None,
            source_root_line_delivery: None,
            declaration_disabled_told: None,
            source_root_notices: Vec::new(),
            source_root_bind_owner: None,
            context_nudge_pending: true,
            context_nudge_in_progress: false,
            context_nudge_in_progress_generation: None,
            context_nudge_generation: 0,
            pending_context_nudge: None,
            context_nudge_delivery_generation: None,
            context_nudge_delivery_turn_generation: None,
            context_refresh_needed: false,
            signalled_compaction_item_ids: VecDeque::new(),
            operator_paused: false,
            stopped_prompt_id: None,
        }
    }
}

impl EngramSessionState {
    pub(crate) fn observed_turn_ref(&self) -> Option<&str> {
        self.unadmitted_active_turn
            .as_ref()
            .map(|trace| trace.host_turn_ref.as_str())
    }

    pub(crate) fn observed_opening(&self) -> Option<&EngramRuntimeOpeningRoot> {
        self.unadmitted_active_turn.as_ref()?.opening_root.as_ref()
    }

    pub(crate) fn observed_source_root(&self) -> Option<&EngramTurnSourceRoot> {
        self.observed_opening()?.source_root.as_ref()
    }

    pub(crate) fn observed_source_root_mut(&mut self) -> Option<&mut EngramTurnSourceRoot> {
        self.unadmitted_active_turn
            .as_mut()?
            .opening_root
            .as_mut()?
            .source_root
            .as_mut()
    }

    pub(crate) fn observed_naming_identity(&self) -> Option<(EngramAuthorityStoreKey, String)> {
        let opening = self.observed_opening()?;
        Some((opening.store.clone(), opening.binding.work_id.clone()))
    }

    /// A cached orientation is ready for admission even with a deferred refresh.
    /// Runtime acceptance releases this local buffer; it never acknowledges an
    /// Engram page. All host orientation reads use non-advancing peek.
    pub(crate) fn context_needs_refresh(&self) -> bool {
        self.pending_context_nudge.is_none()
            && (self.context_nudge_pending || self.context_nudge_in_progress)
    }

    pub(crate) fn invalidate_context_nudge(&mut self) {
        self.context_nudge_generation = self.context_nudge_generation.saturating_add(1);
        self.context_nudge_pending = true;
        self.pending_context_nudge = None;
        self.context_refresh_needed = false;
    }

    pub(crate) fn mark_context_refresh_needed(&mut self, compaction_item_id: Option<&str>) -> bool {
        if let Some(id) = compaction_item_id.filter(|id| id.len() <= 256) {
            if self
                .signalled_compaction_item_ids
                .iter()
                .any(|known| known == id)
            {
                return false;
            }
            if self.signalled_compaction_item_ids.len() == 64 {
                self.signalled_compaction_item_ids.pop_front();
            }
            self.signalled_compaction_item_ids.push_back(id.to_owned());
        }
        self.context_refresh_needed = true;
        self.context_nudge_pending = true;
        true
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EngramDeliveredRange {
    pub(crate) from: i64,
    pub(crate) to: i64,
    pub(crate) head: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EngramControlDirectiveCard {
    directive_id: String,
    pub(crate) kind: String,
    pub(crate) audience: String,
    pub(crate) satisfaction: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EngramControlStage {
    Dispatch,
    Checkpoint,
    Restart,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EngramControlCardDecision {
    Grant,
    Defer,
    Refuse,
    Degraded,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EngramControlCardDispatch {
    SentOnGrant,
    /// Ordinary root dispatch after optional control became unavailable
    /// before Begin; also decodes historical shadow-mode cards.
    SentWithoutGrant,
    Withheld,
    Queued,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EngramControlFailMode {
    Enforced,
    Shadow,
    Degraded,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EngramControlLatencyCard {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) evaluate: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) begin: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) checkpoint: Option<u64>,
    /// The wait for the durable acknowledgement of the retained evaluate
    /// before it was sent, inside `evaluate`'s span.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) admission_fence: Option<u64>,
    /// The wait for the durable acknowledgement of the prepared begin, after
    /// evaluate and before the begin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) preparation_fence: Option<u64>,
    pub(crate) total: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EngramControlCard {
    #[serde(default, skip_serializing_if = "engram_causal_unpublished")]
    pub(crate) causal_failure: Option<EngramCausalFailure>,
    pub(crate) schema_version: u16,
    pub(crate) stage: EngramControlStage,
    pub(crate) assurance: String,
    pub(crate) decision: EngramControlCardDecision,
    pub(crate) dispatch: EngramControlCardDispatch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) refusal_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) defer_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) grant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) directives: Vec<EngramControlDirectiveCard>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) delivered_range: Option<EngramDeliveredRange>,
    pub(crate) latency_ms: EngramControlLatencyCard,
    pub(crate) fail_mode: EngramControlFailMode,
    #[serde(default)]
    pub(crate) repair_armed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) next_intent: Option<EngramNextIntent>,
    /// On the checkpoint of a turn bound to claimed work: where its source
    /// basis was taken (`engram_source_roots.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_root: Option<EngramControlSourceRootCard>,
}

pub(crate) fn parse_engram_result<T: for<'de> Deserialize<'de>>(
    value: Value,
) -> std::result::Result<T, EngramTransportError> {
    engram_decode_value(value, "invalid Engram result schema")
}

// Every decode of received Engram bytes or values reports through this.
// Serde's Display includes offending values (including capabilities), and
// path map keys and enum labels may also be remote-controlled. The text keeps
// only the constant label, the error category and a bounded index-only
// location, never received text.
fn engram_value_free_decode_error(
    label: &str,
    category: serde_json::error::Category,
    path: Option<&serde_path_to_error::Path>,
) -> EngramTransportError {
    let mut location = "$".to_owned();
    for (depth, segment) in path.into_iter().flat_map(|path| path.iter()).enumerate() {
        if depth == 16 {
            location.push_str("[...]");
            break;
        }
        match segment {
            serde_path_to_error::Segment::Seq { index } => location.push_str(&format!("[{index}]")),
            serde_path_to_error::Segment::Map { .. } => location.push_str("[field]"),
            serde_path_to_error::Segment::Enum { .. } => location.push_str("[variant]"),
            serde_path_to_error::Segment::Unknown => location.push_str("[unknown]"),
        }
    }
    EngramTransportError::protocol(format!(
        "{label}: kind={category:?}; field path={location} (names withheld)"
    ))
}

pub(crate) fn engram_decode_value<T: for<'de> Deserialize<'de>>(
    value: Value,
    label: &str,
) -> std::result::Result<T, EngramTransportError> {
    serde_path_to_error::deserialize(value).map_err(|err| {
        engram_value_free_decode_error(label, err.inner().classify(), Some(err.path()))
    })
}

// As serde_json::from_slice, including its trailing-input check.
pub(crate) fn engram_decode_slice<T: for<'de> Deserialize<'de>>(
    bytes: &[u8],
    label: &str,
) -> std::result::Result<T, EngramTransportError> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let decoded = serde_path_to_error::deserialize(&mut deserializer).map_err(|err| {
        engram_value_free_decode_error(label, err.inner().classify(), Some(err.path()))
    })?;
    deserializer
        .end()
        .map_err(|err| engram_value_free_decode_error(label, err.classify(), None))?;
    Ok(decoded)
}

/// Builds the Engram principal for one hosted agent kind. Display names and
/// model aliases are never identity inputs; exact runtime detail is carried in
/// the separate actor context.
pub(crate) fn engram_seat_id(developer_name: &str, session: &Session) -> String {
    format!(
        "{developer_name}/{}",
        engram_agent_context_kind(session.agent)
    )
}

fn engram_agent_context_kind(agent: Agent) -> &'static str {
    match agent {
        Agent::Codex => "codex",
        Agent::Claude => "claude",
        Agent::Cursor => "cursor",
        Agent::Gemini => "gemini",
        Agent::OpenCode => "opencode",
        Agent::Kimi => "kimi",
    }
}

pub(crate) const ENGRAM_ACTOR_CONTEXT_MAX_BYTES: usize = 200;

fn normalize_engram_actor_context_value(value: &str) -> String {
    let mut normalized = String::new();
    let mut previous_was_collapsed_control = false;
    for character in value.trim().chars() {
        if character.is_control() {
            if !previous_was_collapsed_control && !normalized.is_empty() {
                normalized.push(' ');
            }
            previous_was_collapsed_control = true;
        } else {
            match character {
                '%' => normalized.push_str("%25"),
                ';' => normalized.push_str("%3B"),
                '=' => normalized.push_str("%3D"),
                _ => normalized.push(character),
            }
            previous_was_collapsed_control = false;
        }
    }
    normalized.trim().to_owned()
}

fn append_engram_actor_context_field(context: &mut String, key: &str, value: &str) {
    let value = normalize_engram_actor_context_value(value);
    if value.is_empty() {
        return;
    }
    let separator_bytes = usize::from(!context.is_empty());
    let field_bytes = key.len() + 1 + value.len();
    if context.len() + separator_bytes + field_bytes > ENGRAM_ACTOR_CONTEXT_MAX_BYTES {
        return;
    }
    if !context.is_empty() {
        context.push(';');
    }
    context.push_str(key);
    context.push('=');
    context.push_str(&value);
}

/// Renders exact agent/model/reasoning detail separately from the stable agent
/// principal. The 200-byte local limit stays below Engram's 256-byte boundary,
/// and fields that do not fit are omitted instead of being cut mid-value.
pub(crate) fn engram_actor_context(session: &Session) -> Option<String> {
    let mut context = String::new();
    append_engram_actor_context_field(
        &mut context,
        "agent",
        engram_agent_context_kind(session.agent),
    );
    append_engram_actor_context_field(&mut context, "model", &session.model);
    let reasoning = match session.agent {
        Agent::Codex => session
            .reasoning_effort
            .map(CodexReasoningEffort::as_api_value),
        Agent::Claude => session
            .claude_effort
            .and_then(ClaudeEffortLevel::as_cli_value),
        Agent::OpenCode => session
            .opencode_effort
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != OPENCODE_CONFIG_AUTO),
        Agent::Kimi => session
            .kimi_effort
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != "auto"),
        Agent::Cursor | Agent::Gemini => None,
    };
    if let Some(reasoning) = reasoning {
        append_engram_actor_context_field(&mut context, "reasoning", reasoning);
    }
    (!context.is_empty()).then_some(context)
}

/// Returns the actor identity that Engram operations must use for this
/// runtime. A live installed descriptor is authoritative even when an ACP
/// handshake later resolves `session.model` to a more specific display value.
/// Once rotation is pending, the next runtime's identity is derived from the
/// newly committed session settings instead.
pub(crate) fn engram_runtime_actor_identity(
    developer_name: &str,
    record: &SessionRecord,
) -> (String, Option<String>) {
    if !record.runtime_reset_required
        && let Some(installed) = record.engram_mcp_installed.as_ref()
    {
        return (installed.actor_id.clone(), installed.actor_context.clone());
    }
    (
        engram_seat_id(developer_name, &record.session),
        engram_actor_context(&record.session),
    )
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("{digest:x}")
}

#[derive(Clone)]
pub(crate) struct EngramBindingTarget {
    pub(crate) runtime_snapshot: Option<RuntimeToken>,
    pub(crate) work_binding: Option<EngramControlWorkBinding>,
    pub(crate) source_root_read_fence: EngramRootReadFence,
    pub(crate) adapter: Arc<EngramHostAdapter>,
    pub(crate) budget_clock: EngramBudgetClock,
    #[cfg(test)]
    pub(crate) test_root_setup_deadline: Option<std::time::Instant>,
    pub(crate) connection: EngramConnectionConfig,
    pub(crate) settings: EngramProjectSettings,
    pub(crate) project_id: String,
    pub(crate) project_reset_owner_generation: Option<u64>,
    pub(crate) external_ref: String,
}

impl EngramBindingTarget {
    #[cfg(test)]
    pub(crate) fn root_operation_deadline(
        &self,
        started: std::time::Instant,
    ) -> std::time::Instant {
        if let Some(deadline) = self.test_root_setup_deadline {
            return deadline;
        }
        started + self.settings.call_timeout()
    }

    pub(crate) fn rpc_timeout_until(
        &self,
        deadline: std::time::Instant,
    ) -> Result<Duration, EngramTransportError> {
        let remaining = deadline.saturating_duration_since(self.budget_clock.now());
        if remaining.is_zero() {
            return Err(EngramTransportError::deadline(
                "Engram operation budget exhausted before transport",
            ));
        }
        Ok(self.settings.call_timeout().min(remaining))
    }
}

pub(crate) fn engram_project_for_session_locked<'a>(
    inner: &'a StateInner,
    session_id: &str,
) -> Option<&'a Project> {
    if !inner.delegation_inventory_complete() {
        return None;
    }
    let mut current_session_id = session_id.to_owned();
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(current_session_id.clone()) {
            return None;
        }
        let record = inner
            .find_session_index(&current_session_id)
            .and_then(|index| inner.sessions.get(index))?;
        if let Some(project_id) = record.session.project_id.as_deref() {
            return inner.find_project(project_id);
        }
        let delegation = inner
            .delegations
            .iter()
            .find(|delegation| delegation.child_session_id == current_session_id)?;
        current_session_id = delegation.parent_session_id.clone();
    }
}

pub(crate) fn is_lowercase_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A SHA-256 content fingerprint.
pub(crate) fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64 && is_lowercase_hex(value)
}

/// An Engram record id. Engram mints a record's id as a random UUID written as
/// 32 hex digits; ids stored before that are 64 hex digits. Both are opaque.
pub(crate) fn is_engram_record_id(value: &str) -> bool {
    matches!(value.len(), 32 | 64) && is_lowercase_hex(value)
}

impl AppState {
    pub(crate) fn engram_session_has_child_binding_shape_locked(
        inner: &StateInner,
        session_id: &str,
    ) -> bool {
        inner
            .delegations
            .iter()
            .any(|delegation| delegation.child_session_id == session_id)
            || inner
                .find_session_index(session_id)
                .and_then(|index| inner.sessions.get(index))
                .is_some_and(|record| record.session.parent_delegation_id.is_some())
    }

    pub(crate) fn engram_binding_target_for_session_shape_locked(
        inner: &StateInner,
        session_id: &str,
        require_runtime_enabled: bool,
    ) -> std::result::Result<Option<EngramBindingTarget>, String> {
        Self::engram_binding_target_for_session_shape_with_reset_access_locked(
            inner,
            session_id,
            require_runtime_enabled,
            None,
        )
    }

    fn engram_binding_target_for_session_shape_with_reset_access_locked(
        inner: &StateInner,
        session_id: &str,
        require_runtime_enabled: bool,
        project_reset_owner_generation: Option<u64>,
    ) -> std::result::Result<Option<EngramBindingTarget>, String> {
        if Self::engram_session_has_child_binding_shape_locked(inner, session_id) {
            // A delegation child has a narrower authority shape than its
            // parent. Its durable marker remains authoritative if the
            // delegation row is missing, so child-target failure must not
            // fall back to a parent-shaped project session.
            Self::engram_binding_target_for_child_with_reset_access_locked(
                inner,
                session_id,
                require_runtime_enabled,
                project_reset_owner_generation,
            )
        } else {
            Self::engram_binding_target_for_parent_with_reset_access_locked(
                inner,
                session_id,
                require_runtime_enabled,
                project_reset_owner_generation,
            )
        }
    }

    #[cfg(test)]
    pub(crate) fn engram_session_is_enabled_locked(inner: &StateInner, session_id: &str) -> bool {
        engram_source_root_base_is_enabled_locked(inner, session_id)
    }

    fn engram_binding_target_for_child_with_reset_access_locked(
        inner: &StateInner,
        session_id: &str,
        require_runtime_enabled: bool,
        project_reset_owner_generation: Option<u64>,
    ) -> std::result::Result<Option<EngramBindingTarget>, String> {
        // The eligibility predicates live in engram_session_authority.rs,
        // shared with the narrow reader view.
        let Some(authority) = engram_child_session_authority_locked(
            inner,
            session_id,
            require_runtime_enabled,
            project_reset_owner_generation,
        )?
        else {
            return Ok(None);
        };
        let child = authority.record;
        let delegation = authority
            .delegation
            .expect("a child session's authority names its delegation");
        let project = authority.project;
        let settings = authority.settings;
        let binary_path = PathBuf::from(authority.binary_path);
        let home = PathBuf::from(authority.home);
        let root = PathBuf::from(&project.root_path);
        let (actor_id, actor_context) =
            engram_runtime_actor_identity(&inner.preferences.engram.developer_name, child);
        #[cfg(test)]
        count_engram_binding_target_build();
        Ok(Some(EngramBindingTarget {
            runtime_snapshot: child.runtime.runtime_token(),
            work_binding: child.engram.work_binding.clone(),
            source_root_read_fence: EngramRootReadFence::capture(inner),
            adapter: inner.engram_host_adapter.clone(),
            budget_clock: inner.engram_budget_clock_snapshot(),
            #[cfg(test)]
            test_root_setup_deadline: None,
            connection: EngramConnectionConfig {
                binary_path,
                project_file: root.join(".engram-project"),
                home,
                project_root: root,
                actor_id,
                actor_context,
                session_id: authority.connection_session_id.to_owned(),
            },
            settings: settings.clone(),
            project_id: project.id.clone(),
            project_reset_owner_generation,
            external_ref: format!("termal:delegation:{}", delegation.id),
        }))
    }

    fn engram_binding_target_for_parent_with_reset_access_locked(
        inner: &StateInner,
        parent_session_id: &str,
        require_runtime_enabled: bool,
        project_reset_owner_generation: Option<u64>,
    ) -> std::result::Result<Option<EngramBindingTarget>, String> {
        // The eligibility predicates live in engram_session_authority.rs,
        // shared with the narrow reader view.
        let Some(authority) = engram_parent_session_authority_locked(
            inner,
            parent_session_id,
            require_runtime_enabled,
            project_reset_owner_generation,
        )?
        else {
            return Ok(None);
        };
        let parent = authority.record;
        let project = authority.project;
        let settings = authority.settings;
        let binary_path = PathBuf::from(authority.binary_path);
        let home = PathBuf::from(authority.home);
        let root = PathBuf::from(&project.root_path);
        let (actor_id, actor_context) =
            engram_runtime_actor_identity(&inner.preferences.engram.developer_name, parent);
        #[cfg(test)]
        count_engram_binding_target_build();
        Ok(Some(EngramBindingTarget {
            runtime_snapshot: parent.runtime.runtime_token(),
            adapter: inner.engram_host_adapter.clone(),
            budget_clock: inner.engram_budget_clock_snapshot(),
            #[cfg(test)]
            test_root_setup_deadline: None,
            connection: EngramConnectionConfig {
                binary_path,
                project_file: root.join(".engram-project"),
                home,
                project_root: root,
                actor_id,
                actor_context,
                session_id: authority.connection_session_id.to_owned(),
            },
            settings: settings.clone(),
            project_id: project.id.clone(),
            project_reset_owner_generation,
            external_ref: format!("termal:session:{parent_session_id}"),
            work_binding: parent.engram.work_binding.clone(),
            source_root_read_fence: EngramRootReadFence::capture(inner),
        }))
    }

    pub(crate) fn shutdown_engram_session_process(&self, session_id: &str) {
        let adapter = self
            .inner
            .lock()
            .expect("state mutex poisoned")
            .engram_host_adapter
            .clone();
        adapter.shutdown_session(session_id);
    }
}

pub(crate) fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

impl EngramProjectSettings {
    pub(crate) fn same_work_service_settings(&self, other: &Self) -> bool {
        let mut left = self.clone();
        let mut right = other.clone();
        left.acceptance_evaluation = None;
        right.acceptance_evaluation = None;
        left.execution_observability = true;
        right.execution_observability = true;
        left == right
    }
}
