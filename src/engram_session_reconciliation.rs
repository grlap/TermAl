// Strict project Save recovery for an errored, detached session whose retained
// turn grant no longer exists. Owns admission of Engram's read-only inspection
// receipt, not grant retirement, binding, store repair or general reset policy.
// Producer contract: control-session-inspect v1 (Engram w-1d7c9c27e2eb).

// One wall-clock budget starts at the first eligible inspection, not per
// target. OS filesystem calls cannot be cancelled; check again after them and
// refuse late evidence rather than admitting it or starting another process.
#[derive(Default)]
struct EngramAbsenceInspectionBudget {
    deadline: Option<std::time::Instant>,
}

impl EngramAbsenceInspectionBudget {
    fn start_or_continue(&mut self) -> std::time::Instant {
        *self
            .deadline
            .get_or_insert_with(|| engram_absence_now() + ENGRAM_READINESS_TIMEOUT)
    }

    fn validate(&self) -> Result<(), ApiError> {
        if let Some(deadline) = self.deadline {
            validate_engram_absence_deadline(deadline)?;
        }
        Ok(())
    }
}

fn engram_absence_now() -> std::time::Instant {
    #[cfg(test)]
    if let Some(now) = TEST_ENGRAM_ABSENCE_NOW.with(|clock| clock.get()) {
        return now;
    }
    std::time::Instant::now()
}

fn validate_engram_absence_deadline(deadline: std::time::Instant) -> Result<(), ApiError> {
    if engram_absence_now() >= deadline {
        return Err(ApiError::conflict(
            "Engram absence inspection exceeded the shared 10 second Save budget",
        ));
    }
    Ok(())
}

fn validate_engram_absence_executable(binary: &FsPath) -> Result<(), ApiError> {
    // Do not forward control-plane selectors to cmd.exe's second parser. Keep
    // the producer's selector grammar intact for native executables/PS scripts.
    if cfg!(windows)
        && binary
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
    {
        return Err(ApiError::conflict(
            "Engram absence inspection does not support Windows .cmd/.bat wrappers; configure the native Engram executable",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn engram_absence_powershell_command(
    binary: &FsPath,
    project_file: &FsPath,
    home: &FsPath,
    selectors: &[&str],
) -> Result<Command, ApiError> {
    // powershell.exe -File reparses native argv (including trailing spaces).
    // Use a fixed script with JSON data supplied privately in the child's
    // environment, then splat the decoded strings without evaluating them.
    let mut argv = Vec::new();
    for value in [
        binary.as_os_str(),
        std::ffi::OsStr::new("--project-file"),
        project_file.as_os_str(),
        std::ffi::OsStr::new("--home"),
        home.as_os_str(),
    ] {
        argv.push(value.to_str().ok_or_else(|| {
            ApiError::conflict("Engram PowerShell inspection requires Unicode paths")
        })?);
    }
    argv.push("control-session-inspect");
    argv.extend_from_slice(selectors);
    argv.push("--json");
    let data =
        serde_json::to_string(&argv).map_err(|error| ApiError::internal(error.to_string()))?;
    const SCRIPT: &str = r#"$ErrorActionPreference = 'Stop'
$inspectionVector = ConvertFrom-Json -InputObject $env:TERMAL_ENGRAM_INSPECTION_ARGV
Remove-Item Env:TERMAL_ENGRAM_INSPECTION_ARGV
$inspectionScript = [string]$inspectionVector[0]
$inspectionArguments = @($inspectionVector | Select-Object -Skip 1)
& $inspectionScript @inspectionArguments
if ($null -ne $LASTEXITCODE) { exit $LASTEXITCODE }
"#;
    let bytes = SCRIPT
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
        ])
        .arg(base64::engine::general_purpose::STANDARD.encode(bytes))
        .env("TERMAL_ENGRAM_INSPECTION_ARGV", data);
    Ok(command)
}

#[derive(Deserialize)]
struct EngramSessionInspectionReceipt {
    schema_version: u32,
    scope: String,
    mutation_enabled: bool,
    project_id: String,
    database: PathBuf,
    session_id: String,
    retained_grant_id: String,
    host_path_policy: EngramReadinessPathPolicy,
    session_present: bool,
    session_grants_present: bool,
    retained_grant_present: bool,
}

fn engram_checkpoint_can_inspect_absence(error: &EngramTransportError) -> bool {
    error.kind == EngramTransportErrorKind::Remote
        && error.code.as_deref() == Some("control_session_not_bound")
}

fn validate_engram_absence_receipt(
    receipt: &EngramSessionInspectionReceipt,
    target: &EngramBindingTarget,
) -> Result<(), ApiError> {
    let path = &receipt.host_path_policy;
    if receipt.schema_version != 1
        || receipt.scope != "control_session_inspect"
        || receipt.mutation_enabled
        || receipt.session_id != target.connection.session_id
        || Some(receipt.retained_grant_id.as_str()) != target.active_grant_id.as_deref()
        || receipt.retained_grant_id.is_empty()
        || path.status != "matched"
        || path.stored.as_ref().is_none_or(|s| s.is_empty())
        || path.stored != path.resolved
        || receipt.session_present
        || receipt.session_grants_present
        || receipt.retained_grant_present
    {
        return Err(ApiError::conflict(
            "Engram inspection did not prove absence of the exact session and all retained authority",
        ));
    }
    let identity = validate_engram_diagnostic_identity(
        &receipt.project_id,
        &receipt.database,
        &target.connection.project_file,
        &target.connection.home,
    )?;
    if target.settings.authority_store_key.as_ref() != Some(&identity) {
        return Err(ApiError::conflict(
            "Engram inspection does not match the retained authority store identity",
        ));
    }
    Ok(())
}

// Evidence is deliberately not added to the list of successful checkpoints:
// nothing was retired in the store. An aborted/failed Save must retain the old
// local grant and require new evidence on retry, never reuse this snapshot.
struct EngramAbsentSessionEvidence {
    target: EngramBindingTarget,
    dispatch_generation: u64,
    active_turn_generation: u64,
}

impl EngramAbsentSessionEvidence {
    // Pure in-memory validation: callers may hold the global state mutex.
    // current.settings equality also fences the persisted authority_store_key.
    fn validate_locked(&self, inner: &StateInner) -> Result<(), ApiError> {
        let generations = validate_engram_absence_target_locked(inner, &self.target)?;
        if generations != (self.dispatch_generation, self.active_turn_generation) {
            return Err(ApiError::conflict(
                "Engram inspection target generation changed",
            ));
        }
        Ok(())
    }

    fn validate_store_off_lock(&self) -> Result<(), ApiError> {
        validate_engram_absence_store(&self.target)
    }
}

#[cfg(test)]
thread_local! {
    // Thread-local so parallel tests can pause a real Save at the filesystem
    // boundary without intercepting another project's validation.
    static TEST_ENGRAM_ABSENCE_STORE_CHECK: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        std::cell::RefCell::new(None);
    static TEST_ENGRAM_ABSENCE_NOW: std::cell::Cell<Option<std::time::Instant>> =
        const { std::cell::Cell::new(None) };
}

fn validate_engram_absence_store(target: &EngramBindingTarget) -> Result<(), ApiError> {
    #[cfg(test)]
    TEST_ENGRAM_ABSENCE_STORE_CHECK.with(|hook| {
        let callback = hook.borrow_mut().take();
        if let Some(mut callback) = callback {
            callback();
            *hook.borrow_mut() = Some(callback);
        }
    });
    let key = target
        .settings
        .authority_store_key
        .as_ref()
        .ok_or_else(|| {
            ApiError::conflict(
                "Cannot reconcile without the retained Engram authority store identity",
            )
        })?;
    let actual = validate_engram_diagnostic_identity(
        &key.project_id,
        &key.database_path,
        &target.connection.project_file,
        &target.connection.home,
    )?;
    if &actual != key {
        return Err(ApiError::conflict(
            "Engram inspection store routing changed",
        ));
    }
    Ok(())
}

fn validate_engram_absence_target_locked(
    inner: &StateInner,
    target: &EngramBindingTarget,
) -> Result<(u64, u64), ApiError> {
    let refuse = || {
        ApiError::conflict(
            "Engram absence recovery requires the unchanged, errored, detached session under its owned project reset",
        )
    };
    let owner = target.project_reset_owner_generation.ok_or_else(refuse)?;
    let current = project_engram_binding_target_during_owned_reset_locked(
        inner,
        &target.connection.session_id,
        &target.project_id,
        owner,
    )
    .map_err(|_| refuse())?
    .ok_or_else(refuse)?;
    if current.project_id != target.project_id
        || current.connection != target.connection
        || current.settings != target.settings
        || current.routing_token != target.routing_token
        || current.active_grant_id != target.active_grant_id
        || target.routing_token.as_ref().is_none_or(|s| s.is_empty())
        || target.active_grant_id.as_ref().is_none_or(|s| s.is_empty())
    {
        return Err(refuse());
    }
    let record = inner
        .find_session_index(&target.connection.session_id)
        .and_then(|index| inner.sessions.get(index))
        .ok_or_else(refuse)?;
    if record.session.status != SessionStatus::Error
        || !matches!(record.runtime, SessionRuntime::None)
        || !record.engram.project_reset_in_progress
        || !record.engram.checkpoint_in_progress
        || record.engram.checkpoint_owner_generation != Some(owner)
        || record.engram.bind_in_progress
        || record.engram.pending_dispatch.is_some()
    {
        return Err(refuse());
    }
    Ok((
        record.engram.dispatch_generation,
        record.active_turn_generation,
    ))
}

impl AppState {
    fn inspect_absent_engram_reset_session(
        &self,
        target: &EngramBindingTarget,
        deadline: std::time::Instant,
    ) -> Result<EngramAbsentSessionEvidence, ApiError> {
        validate_engram_absence_deadline(deadline)?;
        validate_engram_absence_executable(&target.connection.binary_path)?;
        let (dispatch_generation, active_turn_generation) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            validate_engram_absence_target_locked(&inner, target)?
        };
        validate_engram_absence_store(target)?;
        let connection = &target.connection;
        let declaration = read_engram_diagnostic_declaration(&connection.project_file)?;
        let grant = target
            .active_grant_id
            .as_deref()
            .expect("validated retained grant");
        validate_engram_absence_deadline(deadline)?;
        // Use flag=value so a leading '-' in an otherwise valid selector is
        // always data, not another CLI option.
        let session_arg = format!("--target-session-id={}", connection.session_id);
        let grant_arg = format!("--retained-grant-id={grant}");
        let output = run_engram_diagnostic_args_until(
            &connection.binary_path,
            &connection.project_file,
            &connection.home,
            &connection.project_root,
            "control-session-inspect",
            &[&session_arg, &grant_arg],
            deadline,
            ENGRAM_READINESS_TIMEOUT,
        )?;
        validate_engram_absence_deadline(deadline)?;
        if !output.status.success() {
            return Err(ApiError::conflict(format!(
                "Engram absence inspection failed ({}): {} {}",
                output.status,
                engram_diagnostic_excerpt(&output.stdout),
                engram_diagnostic_excerpt(&output.stderr),
            )));
        }
        if output.stdout.len() > ENGRAM_DIAGNOSTIC_REPORT_LIMIT {
            return Err(ApiError::conflict(
                "Engram absence inspection receipt is oversized",
            ));
        }
        if read_engram_diagnostic_declaration(&connection.project_file)? != declaration {
            return Err(ApiError::conflict(
                "Engram declaration changed during absence inspection",
            ));
        }
        let receipt: EngramSessionInspectionReceipt = serde_json::from_slice(&output.stdout)
            .map_err(|_| {
                ApiError::conflict("Engram absence inspection returned an invalid receipt")
            })?;
        validate_engram_absence_receipt(&receipt, target)?;
        let evidence = EngramAbsentSessionEvidence {
            target: target.clone(),
            dispatch_generation,
            active_turn_generation,
        };
        evidence.validate_store_off_lock()?;
        validate_engram_absence_deadline(deadline)?;
        evidence.validate_locked(&self.inner.lock().expect("state mutex poisoned"))?;
        Ok(evidence)
    }
}
