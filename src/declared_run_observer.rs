// Owns pre-prompt native reservations, original provider start/END wiring,
// endpoint comparison and immutable terminal handoff to registration.
// Full source identity remains owned by Engram's pinned source adapter and
// artifact verdicts by the existing readers. Lock order is StateInner then execution;
// Git/artifact reads hold neither. S0 publishes only before prompt handoff,
// R never moves, and END freezes artifact judgment before later tool events.

// The owner may use 30 s to capture source. Opening also reserves the existing
// authority-read budget for declaration/preimage and publication stages;
// registration durability itself is unchanged. Expiry withholds credit,
// not dispatch. Every source stage derives its cap from the same budget.
const NATIVE_S0_CAPTURE_BUDGET: Duration = ENGRAM_SOURCE_COMMAND_BUDGET;
const NATIVE_S0_AUTHORITY_MARGIN: Duration = TEST_RUN_REGISTRATION_DURABILITY_BUDGET;

fn native_opening_authority_deadline(clock: &EngramBudgetClock, capture_deadline: std::time::Instant,
    requested: std::time::Instant) -> std::time::Instant {
    capture_deadline.min(test_run_registration_deadline(clock, requested))
}

// Host-private provenance, persisted with the original execution before its
// registration. A request file cannot supply or change this reservation.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct DeclaredRunReservation {
    host_instance: String,
    execution_id: String,
    session_id: String,
    run_id: String,
    run_directory: String,
    common_dir: String,
    request_identity: String,
    registered_at: String,
    source: DeclaredRunSource,
    declaration: EngramDeclaredBinding,
    store: EngramAuthorityStoreKey,
    binding: EngramControlWorkBinding,
    named: TestRunNamedRoot,
    // Provider time is attributed data, never the host reservation clock.
    provider_time: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct DeclaredRunTerminal {
    results_digest: String,
    source: Option<DeclaredRunSource>,
    #[serde(default)]
    source_refusal: Option<String>,
    check: EngramVerificationRecordCheck,
    // Original provider endpoint, never inferred from mutable result JSON.
    #[serde(default)]
    reported_success: bool,
}

#[derive(Clone, Debug)]
struct DeclaredRunObservedEnd {
    ended_at: String,
    exit: Option<i64>,
    reported_success: bool,
    interrupted: bool,
    check: EngramVerificationRecordCheck,
}

// The artifact reader already combines the original process endpoint with its
// result. Display that frozen verdict without inventing an OS exit number.
fn declared_run_result_state(check: &EngramVerificationRecordCheck) -> &'static str {
    match check.outcome {
        EngramVerificationRecordCheckOutcome::Passed => "passed",
        EngramVerificationRecordCheckOutcome::Failed => "failed",
        _ => "unknown",
    }
}

#[derive(Debug)]
struct DeclaredRunExecution {
    reservation: DeclaredRunReservation,
    request: Value,
    registered: bool,
    worker_active: bool,
    end_capture_started: bool,
    end: Option<DeclaredRunObservedEnd>,
    failure: Option<String>,
    verification_refusal: Option<String>,
}

struct DeclaredRunWorkerSlot(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for DeclaredRunWorkerSlot {
    fn drop(&mut self) { self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst); }
}

fn declared_run_prepare_directory(start: &DeclaredRunReservation) -> Result<PathBuf> {
    let directory = PathBuf::from(&start.run_directory);
    let parent = directory.parent().context("native run parent absent")?;
    let git_dir = fs::canonicalize(parent.parent().context("native Git directory absent")?)?;
    let common = fs::canonicalize(test_runs_git_common_dir(&start.source.root)
        .context("native Git common directory absent")?)?;
    if engram_exact_path_key(&common) != start.common_dir
        || test_run_registration_relative_parent(&common, &git_dir.join("review-runs")).is_none()
        || parent.file_name() != Some(std::ffi::OsStr::new("review-runs"))
        || directory.file_name().and_then(|name| name.to_str()) != Some(start.run_id.as_str())
    {
        anyhow::bail!("native run directory is outside its original Git metadata");
    }
    // Create only the one known metadata directory. An existing redirect is
    // resolved and refused before creating any run or writing either file.
    match fs::create_dir(parent) {
        Ok(()) => {},
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {},
        Err(error) => return Err(error.into()),
    }
    if fs::canonicalize(parent)? != git_dir.join("review-runs")
        || !test_run_registration_target(&common, &directory)
    {
        anyhow::bail!("native run metadata directory was redirected");
    }
    fs::create_dir(&directory)?;
    Ok(directory)
}

fn declared_run_frame_match(turn: &DeclaredRunTurn, frame: &Value, workdir: &str)
    -> Option<(EngramCheckCommand, EngramDeclaredBinding)> {
    let snapshot = turn.snapshot.as_ref()?;
    let (ran, cwd) = match &turn.runtime {
        RuntimeToken::Codex(_) => (frame.get("command")?.as_str()?, FsPath::new(frame.get("cwd")?.as_str()?)),
        RuntimeToken::Claude(_) => {
            let input = frame.get("input")?;
            let ran = input.get("command")?.as_str()?;
            if engram_declared_candidate(ran)?.0.directory.as_deref()
                .is_none_or(|path| !FsPath::new(path).is_absolute()) { return None; }
            (ran, FsPath::new(workdir))
        }
        _ => return None,
    };
    snapshot.match_command(ran, cwd)
}

#[derive(Clone, Debug)]
struct DeclaredRunTurn {
    runtime: RuntimeToken,
    generation: u64,
    keys: TestRunWorktreeKeys,
    store: EngramAuthorityStoreKey,
    binding: EngramControlWorkBinding,
    named: TestRunNamedRoot,
    // The name may have been refreshed after its retained proof was recorded.
    // Recheck the selected root's fence here; the existing held-claim read
    // validates and refreshes the binding before publishing S0.
    root_claim_fence: i64,
    snapshot: Option<DeclaredRunSnapshot>,
    opening_refusal: Option<String>,
    opening_refusal_code: Option<&'static str>,
    first_tool: DeclaredRunFirstTool,
    timings: Arc<SourceReadTimings>,
    native_start_attempted: bool,
    execution: Option<Arc<Mutex<DeclaredRunExecution>>>,
}

impl DeclaredRunTurn {
    fn is_current(&self, record: &SessionRecord) -> bool {
        record.active_turn_generation == self.generation
            && record.runtime.matches_runtime_token(&self.runtime)
            && record.session.status == SessionStatus::Active
            && !record.runtime_stop_in_progress
    }
}

fn declared_run_opening_notice(record: &SessionRecord) -> Option<String> {
    let turn = record.engram.declared_run_turn.as_ref().filter(|turn| turn.is_current(record))?;
    if turn.snapshot.is_some() { return None; }
    let code = turn.opening_refusal_code.unwrap_or("opening_invalid");
    Some(format!("[TermAl] Native declared-command credit is unavailable for this turn ({code}). S0 stages: {}. The prompt proceeds. To seek credit, run the declared command as the first tool activity of a later turn after the source and named-root authority are valid.", turn.timings.summary()))
}

fn declared_run_opening_authority_locked(
    inner: &StateInner, session_id: &str, pending: &DeclaredRunTurn,
) -> Result<EngramSourceOwnerAuthority> {
    let index = inner.find_session_index(session_id).context("native opening session unavailable")?;
    let record = &inner.sessions[index];
    let current = record.engram.declared_run_turn.as_ref()
        .context("native opening reservation unavailable")?;
    if !pending.is_current(record) || current.runtime != pending.runtime
        || current.generation != pending.generation || current.keys != pending.keys
        || current.store != pending.store || current.binding != pending.binding
        || current.named != pending.named || current.root_claim_fence != pending.root_claim_fence
        || current.snapshot.is_some()
        || current.first_tool != DeclaredRunFirstTool::Waiting
    {
        anyhow::bail!("native opening reservation changed during capture");
    }
    let authority = engram_source_owner_authority_locked(inner, session_id)
        .map_err(|error| anyhow!(error.message))?;
    let (binding, named) = test_run_registration_named_binding(
        inner, session_id, Some(&pending.store), Some(&pending.keys))
        .map_err(|error| anyhow!(error.message))?;
    if authority.settings.authority_store_key.as_ref() != Some(&pending.store)
        || binding.as_ref() != Some(&pending.binding) || named.as_ref() != Some(&pending.named)
        || test_run_named_root_fence(inner, session_id, Some(&pending.store), named.as_ref())
            != Some(pending.root_claim_fence)
    {
        anyhow::bail!("native opening named-root authority changed during capture");
    }
    Ok(authority)
}

impl AppState {
    // One admitted observation supplies both schema/pin and this named root's
    // S0 identity. The public project-root metadata GET remains separate.
    fn capture_declared_run_opening_with_owner(
        &self, session_id: &str, pending: &DeclaredRunTurn, deadline: std::time::Instant,
        mut run: impl FnMut(&FsPath, &FsPath, &[String], std::time::Instant, usize)
            -> Result<std::process::Output>,
    ) -> Result<(DeclaredRunSnapshot, EngramSourceOwnerAuthority)> {
        let authority = declared_run_opening_authority_locked(
            &self.inner.lock().expect("state mutex poisoned"), session_id, pending)?;
        let configured = authority.settings.binary_path.as_deref().filter(|value| !value.is_empty())
            .context("Engram source owner binaryPath is not configured for this project")?;
        let root = FsPath::new(&pending.named.workspace_id);
        let cwd = engram_host_workdir(self.persistence_path.as_ref())?;
        // Bare-name trust remains scoped to the configured project, even when
        // the measured named worktree differs. No worktree-owned PATH search.
        let binary = resolve_engram_source_program(FsPath::new(configured),
            FsPath::new(&authority.project_root), deadline)?;
        let measurement = pending.timings.begin("source_owner_observation");
        let (pin, observation) = engram_source_owner_observe_with(&binary, root, &cwd, deadline, &mut run)?;
        let observed_at = test_run_timestamp();
        drop(measurement);
        let mut snapshot = declared_run_snapshot_with_owner(root, &pin, deadline,
            Some(pending.timings.clone()), |_, _, until| observation.into_source_id(until))?;
        snapshot.source.measured_at = observed_at;
        if resolve_engram_source_program(FsPath::new(configured),
            FsPath::new(&authority.project_root), deadline)? != binary
        {
            anyhow::bail!("native opening source owner changed during capture");
        }
        let current = declared_run_opening_authority_locked(
            &self.inner.lock().expect("state mutex poisoned"), session_id, pending)?;
        if current != authority {
            anyhow::bail!("native opening source owner configuration changed during capture");
        }
        engram_source_before_deadline(deadline)?;
        Ok((snapshot, authority))
    }

    fn native_declared_scope_current(&self, start: &DeclaredRunReservation) -> bool {
        let Some(keys) = test_run_worktree_keys(&start.source.root) else { return false; };
        let inner = self.inner.lock().expect("state mutex poisoned");
        if inner.find_session_index(&start.session_id).is_none() { return false; }
        if let Some(execution) = inner.test_runs.declared_executions.get(&start.run_id) {
            let job = execution.lock().expect("native execution mutex poisoned");
            if job.reservation == *start && (job.failure.is_some() || job.verification_refusal.is_some()) { return false; }
        }
        let (named_binding, named) = match test_run_registration_named_binding(
            &inner, &start.session_id, Some(&start.store), Some(&keys)) {
            Ok(binding) => binding,
            Err(error) => {
                eprintln!("[termal] native named-root scope refused: {error:?}");
                return false;
            }
        };
        keys.common_dir == start.common_dir && named.as_ref() == Some(&start.named)
            && named_binding.as_ref().is_some_and(|named| engram_same_recovery_run(named, &start.binding))
            && test_run_named_root_fence(&inner, &start.session_id, Some(&start.store), Some(&start.named))
                == Some(start.binding.claim_fence)
            && inner.test_run_observations.get(&start.execution_id)
                .is_some_and(|row| row.declared_native.as_ref() == Some(start))
    }

    fn finish_native_declared_run(&self, start: &DeclaredRunReservation, request: &Value,
        end: &DeclaredRunObservedEnd, execution: &Arc<Mutex<DeclaredRunExecution>>) -> Result<()> {
        let state = declared_run_result_state(&end.check);
        let stage = json!({"name":"focused", "state":state, "code":end.exit,
            "reportedSuccess":end.reported_success,
            "started":start.registered_at, "ended":end.ended_at, "command":request["observedCommand"]});
        // END already froze this judgment before returning to provider-event
        // processing. Registration waits cannot substitute a later artifact.
        let check = end.check.clone();
        let deadline = std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT;
        let live = self.inner.lock().expect("state mutex poisoned").engram_turn_basis_captures_live.clone();
        let declaration = start.declaration.clone();
        let source_owner = start.source.source_owner.clone();
        #[cfg(test)]
        let source_capture = self.inner.lock().expect("state mutex poisoned").test_declared_run_source.clone();
        let after = bounded_content_revision_capture(&live, &self.capture_lifetime,
            ENGRAM_TURN_BASIS_CAPTURE_LIMIT, deadline.saturating_duration_since(std::time::Instant::now()),
            move || {
                let capture = || {
                    #[cfg(test)]
                    if let Some(capture) = source_capture {
                        return capture(&declaration, &source_owner, deadline);
                    }
                    declared_run_source(&declaration, &source_owner, deadline)
                };
                Some(capture().map_err(|error| format!("{error:#}").chars().take(2048).collect::<String>()))
            });
        let (after, source_refusal) = match after {
            Some(Ok(source)) => (Some(source), None),
            Some(Err(reason)) => (None, Some(reason)),
            None => (None, Some("native after-run source capture unavailable: deadline/capacity/cleanup".to_owned())),
        };
        let endpoint_matches = after.as_ref().is_some_and(|after|
            after.root == start.source.root && after.source_id == start.source.source_id);
        let source_change = after.as_ref().filter(|after| after.root == start.source.root
            && after.source_owner == start.source.source_owner && after.source_id != start.source.source_id)
            .map(|after| engram_source_id_change_detail(&engram_source_id_differences(&start.source.source_id, &after.source_id)));
        let mut eligible = endpoint_matches && self.native_declared_scope_current(start);
        if eligible {
            let clock = self.engram_budget_clock();
            let deadline = test_run_registration_deadline(&clock, std::time::Instant::now());
            let mut diagnostics = TestRunRegistrationDiagnostics::new(&self.server_instance_id);
            let keys = test_run_worktree_keys(&start.source.root);
            // The registered run id intentionally replays its immutable
            // binding. A new probe must refresh the live held claim here.
            let probe = format!("native-closing-{}", Uuid::new_v4());
            eligible = self.read_test_run_current_named_binding(&start.session_id, &probe,
                keys.as_ref(), &clock, deadline, &mut diagnostics).ok().is_some_and(|current| {
                let inner = self.inner.lock().expect("state mutex poisoned");
                current.resolve_locked(&inner, &start.session_id, Some(&start.store), keys.as_ref()).ok()
                    == Some((Some(start.binding.clone()), Some(start.named.clone())))
            });
        }
        eligible &= self.native_declared_scope_current(start);
        eligible &= {
            let job = execution.lock().expect("native execution mutex poisoned");
            job.reservation == *start && job.failure.is_none() && job.verification_refusal.is_none()
        };
        let results = json!({
            "runId":start.run_id,"state":state,"exitCode":end.exit,
            "started":start.registered_at,"ended":test_run_timestamp(),"interrupted":end.interrupted,
            "stages":[stage],
            "input":{"kind":"source_id","source_id":start.source.source_id},
            "post_run":{"source_id":after.as_ref().map(|source| &source.source_id)},
            "nativeSourceUnavailableReason":source_refusal,
            "nativeEligible":eligible,
        });
        let bytes = serde_json::to_vec_pretty(&results)?;
        let terminal = DeclaredRunTerminal { results_digest: file_content_hash(&bytes), source: after, source_refusal, check,
            reported_success: end.reported_success };
        let expected = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let expected = inner.test_run_registrations.update(&start.run_id, |registration| {
                if registration.declared_native.as_ref() != Some(start) || registration.declared_terminal.is_some() {
                    return None;
                }
                registration.declared_terminal = Some(terminal.clone());
                Some(registration.clone())
            }).flatten().ok_or_else(|| anyhow!("native registration changed before its terminal"))?;
            let row = inner.test_run_observations.update(&start.execution_id, |row| {
                if row.declared_native.as_ref() == Some(start) {
                    if let Some(artifact) = row.artifact.as_mut() {
                        artifact.terminal_digest = Some(terminal.results_digest.clone());
                    }
                }
                row.clone()
            }).ok_or_else(|| anyhow!("native original execution disappeared"))?;
            self.persist_test_command_observation_locked(&mut inner, row);
            expected
        };
        let durable = self.inner.lock().expect("state mutex poisoned")
            .test_runs.observations_durable.contains(&start.execution_id);
        if !durable {
            let (fence, waiter) = PersistFence::new(PersistFenceTarget::TestRunRegistration(expected),
                std::time::Instant::now() + TEST_RUN_REGISTRATION_DURABILITY_BUDGET);
            self.persist_tx.send(PersistRequest::Fence(Box::new(fence)))
                .map_err(|_| anyhow!("native terminal persistence unavailable"))?;
            waiter.wait().map_err(|error| anyhow!("native terminal was not durably confirmed: {error:?}"))?;
        }
        engram_verification_record_write(&PathBuf::from(&start.run_directory).join("results.json"), &bytes)?;
        if let Some(detail) = source_change {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if let Some(index) = inner.find_session_index(&start.session_id) {
                EngramHost::native_run_withheld(&mut inner.sessions[index],
                    format!("{ENGRAM_CHECK_CREDIT_LINE_PREFIX} native run {} received no credit: {detail}.", start.run_id));
            }
        }
        Ok(())
    }

    fn start_native_declared_run(&self, session_id: &str, provenance: &EngramObservationProvenance,
        key: Option<&str>, is_command: bool, frame: &Value) {
        let registered_at = test_run_timestamp();
        let Some(key) = key.filter(|key| !key.is_empty() && is_command) else { return; };
        let (turn, mut origin, workdir) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else { return; };
            let record = &inner.sessions[index];
            let Some(turn) = record.engram.declared_run_turn.as_ref().filter(|turn|
                declared_run_original_turn(record, provenance) && turn.is_current(record)
                    && turn.first_tool.accepts(key)
                    && (!turn.native_start_attempted || turn.execution.is_some())) else { return; };
            let captured = (turn.clone(), TestRunCommandOrigin::capture(&inner, index, provenance, key),
                record.session.workdir.clone());
            // An incomplete first frame cannot move the original R to a
            // later description. Duplicates only replay an existing R.
            inner.sessions[index].engram.declared_run_turn.as_mut().unwrap().native_start_attempted = true;
            captured
        };
        let Some(snapshot) = turn.snapshot.as_ref() else { return; };
        let matched = declared_run_frame_match(&turn, frame, &workdir);
        if let Some(execution) = turn.execution.as_ref() {
            let mut job = execution.lock().expect("native execution mutex poisoned");
            if matched.as_ref().is_none_or(|(_, binding)| binding != &job.reservation.declaration) {
                job.verification_refusal = Some("original native tool identity was reused with different command facts".to_owned());
            }
            return;
        }
        let Some((command, declaration)) = matched else { return; };
        let Ok(argv) = engram_declared_words(&declaration.entry.command) else { return; };
        let Some(program) = argv.first() else { return; };
        let run_id = format!("test-{}", Uuid::new_v4());
        let directory = snapshot.git_dir.join("review-runs").join(&run_id);
        let execution_id = origin.execution_id();
        let request = json!({
            "runId": run_id, "owner": session_id, "root": snapshot.source.root,
            "full": false, "input": "source_id", "detached": false, "started": registered_at,
            "observedCommand": argv, "nativeExecution": execution_id,
            "sourceOwner":snapshot.source.source_owner,
            "nativeSourceId":snapshot.source.source_id,
            "nativeOpeningStages": turn.timings.summary(),
            "stages": [{"name":"focused", "command":program, "args":&argv[1..], "cwd":declaration.entry.cwd}],
        });
        let Ok(request_identity) = test_run_registration_identity(&request) else { return; };
        let provider_time = ["timestamp", "startedAt", "startTime", "started_at"]
            .into_iter().filter_map(|name| frame.get(name).filter(|value|
                value.as_str().is_some_and(|value| value.len() <= 128) || value.is_number())
                .map(|value| (name.to_owned(), value.clone()))).collect();
        let reservation = DeclaredRunReservation {
            host_instance: self.server_instance_id.clone(), execution_id, session_id: session_id.to_owned(),
            run_id, run_directory: test_run_display_path(&directory), request_identity, registered_at,
            common_dir: turn.keys.common_dir.clone(),
            source: snapshot.source.clone(), declaration: declaration.clone(), store: turn.store.clone(),
            binding: turn.binding.clone(), named: turn.named.clone(), provider_time,
        };
        let execution = Arc::new(Mutex::new(DeclaredRunExecution {
            reservation: reservation.clone(), request, registered: false, worker_active: true,
            end: None, end_capture_started: false, failure: None, verification_refusal: None,
        }));
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else { return; };
            let record = &mut inner.sessions[index];
            if !turn.is_current(record) { return; }
            let Some(current) = record.engram.declared_run_turn.as_mut().filter(|current|
                current.execution.is_none()
                    && current.first_tool.accepts(key)) else { return; };
            current.execution = Some(execution.clone());
            origin.work_binding = Some(turn.binding.clone());
            origin.source_root = inner.engram_work_source_roots.iter().find(|root|
                root.store == turn.store && root.root == turn.named.workspace_id
                    && root.generation == turn.named.generation).cloned();
            origin.validated_unbound = false;
        }
        self.observe_test_command_start_with_native(origin, &command, &reservation.named.workspace_id,
            &reservation.registered_at, Some(&declaration), Some(reservation.clone()));
        self.inner.lock().expect("state mutex poisoned").test_runs.declared_executions
            .insert(reservation.run_id.clone(), execution.clone());
        self.spawn_native_declared_worker(execution);
    }

    fn spawn_native_declared_worker(&self, execution: Arc<Mutex<DeclaredRunExecution>>) {
        let live = self.inner.lock().expect("state mutex poisoned").engram_turn_basis_captures_live.clone();
        use std::sync::atomic::Ordering;
        if live.try_update(Ordering::SeqCst, Ordering::SeqCst,
            |count| (count < ENGRAM_TURN_BASIS_CAPTURE_LIMIT).then_some(count + 1)).is_err() {
            let mut job = execution.lock().expect("native execution mutex poisoned");
            job.failure = Some("native registration capacity unavailable".to_owned());
            job.worker_active = false;
            return;
        }
        let slot = DeclaredRunWorkerSlot(live);
        let state = self.clone();
        let failed = execution.clone();
        if let Err(error) = self.capture_lifetime.spawn("native-declared-run", move || {
            let _slot = slot;
            if let Err(error) = state.drive_native_declared_run(&execution) {
                let session_id = {
                    let mut job = execution.lock().expect("native execution mutex poisoned");
                    job.failure = Some(format!("{error:#}"));
                    job.worker_active = false;
                    eprintln!("[termal] native declared verification withheld for {}: {error:#}", job.reservation.run_id);
                    job.reservation.session_id.clone()
                };
                state.start_runtime_observation_worker(&session_id);
            }
        }) {
            let mut job = failed.lock().expect("native execution mutex poisoned");
            job.failure = Some(error.to_string());
            job.worker_active = false;
        }
    }

    fn drive_native_declared_run(&self, execution: &Arc<Mutex<DeclaredRunExecution>>) -> Result<()> {
        let (start, request, registered) = {
            let job = execution.lock().expect("native execution mutex poisoned");
            if job.failure.is_some() || job.verification_refusal.is_some() {
                anyhow::bail!("original native execution was refused: {:?} {:?}", job.failure, job.verification_refusal);
            }
            (job.reservation.clone(), job.request.clone(), job.registered)
        };
        let directory = PathBuf::from(&start.run_directory);
        if !registered {
            if !self.native_declared_scope_current(&start) {
                anyhow::bail!("native opening authority changed before registration");
            }
            // Confirm the immutable start even if the provider has already
            // appended an end to its observation. No clock is rewritten.
            let durable = self.inner.lock().expect("state mutex poisoned")
                .test_runs.observations_durable.contains(&start.execution_id);
            if !durable {
                let (fence, waiter) = PersistFence::new(
                    PersistFenceTarget::DeclaredRunReservation(Box::new(start.clone())),
                    std::time::Instant::now() + TEST_RUN_REGISTRATION_DURABILITY_BUDGET);
                self.persist_tx.send(PersistRequest::Fence(Box::new(fence)))
                    .map_err(|_| anyhow!("native reservation persistence unavailable"))?;
                waiter.wait().map_err(|error| anyhow!("native reservation was not durable: {error:?}"))?;
            }
            // Admission precedes every write; registration repeats containment.
            declared_run_prepare_directory(&start)?;
            engram_verification_record_write(&directory.join("request.json"), &serde_json::to_vec_pretty(&request)?)?;
            let running = json!({"runId":start.run_id,"state":"running", "started":start.registered_at,
                "stages":[{"name":"focused","state":"running","started":start.registered_at,
                    "command":request["observedCommand"]}]});
            engram_verification_record_write(&directory.join("results.json"), &serde_json::to_vec_pretty(&running)?)?;
            self.register_test_run_reserved(&start.session_id, RegisterTestRunRequest {
                run_id: start.run_id.clone(), run_directory: start.run_directory.clone(),
            }, TestRunRegistrationDiagnostics::new(&self.server_instance_id), Some(&start))
                .map_err(|error| anyhow!("native registration refused: {error:?}"))?;
            execution.lock().expect("native execution mutex poisoned").registered = true;
        }
        let end = {
            let mut job = execution.lock().expect("native execution mutex poisoned");
            let Some(end) = job.end.clone() else { job.worker_active = false; return Ok(()); };
            end
        };
        self.finish_native_declared_run(&start, &request, &end, execution)?;
        execution.lock().expect("native execution mutex poisoned").worker_active = false;
        self.inner.lock().expect("state mutex poisoned").test_runs.declared_executions.remove(&start.run_id);
        Ok(())
    }

    fn end_native_declared_run(&self, session_id: &str, provenance: &EngramObservationProvenance,
        key: &str, exit: Option<EngramCommandExit>, interrupted: bool) {
        let execution = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else { return; };
            let record = &inner.sessions[index];
            let Some(turn) = record.engram.declared_run_turn.as_ref().filter(|turn|
                turn.generation == record.active_turn_generation
                    && record.runtime.matches_runtime_token(&turn.runtime)
                    && declared_run_original_turn(record, provenance)) else { return; };
            let Some(execution) = turn.execution.clone() else { return; };
            let expected = TestRunCommandOrigin::capture(&inner, index, provenance, key).execution_id();
            if execution.lock().expect("native execution mutex poisoned").reservation.execution_id != expected { return; }
            execution
        };
        self.freeze_native_declared_end(session_id, provenance, key, execution, exit, interrupted);
    }

    fn freeze_native_declared_end(&self, session_id: &str, provenance: &EngramObservationProvenance,
        key: &str, execution: Arc<Mutex<DeclaredRunExecution>>, exit: Option<EngramCommandExit>, interrupted: bool) {
        // An acknowledgement of an ongoing command is never END, even if
        // its result artifact is already complete. The original task's END
        // freezes the artifact and only then permits the after-run capture.
        if !interrupted && exit == Some(EngramCommandExit::NotFinished) { return; }
        let ended_at = test_run_timestamp();
        let (start, request) = {
            let mut job = execution.lock().expect("native execution mutex poisoned");
            if job.end_capture_started || job.end.is_some() { return; }
            job.end_capture_started = true;
            (job.reservation.clone(), job.request.clone())
        };
        let code = match exit { Some(EngramCommandExit::Code(code)) => Some(code), _ => None };
        // Claude reports successful completion without an OS exit number.
        // Preserve that distinction; the frozen artifact remains the oracle.
        let reported_success = exit == Some(EngramCommandExit::ReportedSuccess);
        let state = match code { Some(0) => "passed", Some(_) => "failed",
            None if reported_success => "passed", None => "unknown" };
        let stage = json!({"name":"focused", "state":state, "code":code,
            "started":start.registered_at,"ended":ended_at,"command":request["observedCommand"]});
        // Bounded artifact inspection occurs off both state/execution locks,
        // synchronously at the original END boundary, before another tool can
        // replace the artifact while registration is waiting on durability.
        let mut check = engram_verification_record_declared_check(&stage,
            &engram_verification_record_planned_argv(&request), &start.declaration,
            exit.unwrap_or(EngramCommandExit::Unknown));
        if interrupted && let Ok(check) = &mut check {
            check.outcome = EngramVerificationRecordCheckOutcome::Indeterminate;
            check.summary = Some("The process was interrupted before its artifact END was frozen.".to_owned());
        }
        let ended_test_runs = self.observe_test_command_end(session_id, provenance, key,
            if exit == Some(EngramCommandExit::NotFinished) { None } else { exit });
        let outcome = check.as_ref().ok().map(|check| match check.outcome {
            EngramVerificationRecordCheckOutcome::Passed => EngramExecutionOutcome::Succeeded,
            EngramVerificationRecordCheckOutcome::Failed => EngramExecutionOutcome::Failed,
            _ => EngramExecutionOutcome::Unknown,
        });
        // This native END already closed the row, so the later ordinary
        // recorder callback has no ended ids on which to apply the verdict.
        // Registration failure withholds credit, but must not leave this row
        // with a passing exit-derived state when its artifact is not passing.
        self.apply_declared_verdict_to_test_run(&ended_test_runs, outcome);
        let spawn = {
            let mut job = execution.lock().expect("native execution mutex poisoned");
            if job.failure.is_some() || job.verification_refusal.is_some() { return; }
            let check = match check {
                Ok(check) => check,
                Err(error) => { job.failure = Some(format!("native END artifact refused: {error}")); return; }
            };
            job.end = Some(DeclaredRunObservedEnd { ended_at, exit: code, reported_success,
                interrupted, check });
            if job.worker_active { false } else { job.worker_active = true; true }
        };
        if spawn { self.spawn_native_declared_worker(execution); }
    }

    fn end_native_declared_task(&self, session_id: &str, token: &RuntimeToken, key: &str, interrupted: bool) {
        let provenance = EngramObservationProvenance::Claude(ClaudeObservationProvenance {
            token: token.clone(), origin: ClaudeWorkOrigin::Unattributed,
        });
        let execution = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else { return; };
            let record = &inner.sessions[index];
            if !record.runtime.matches_runtime_token(token) { return; }
            let runtime = test_run_runtime_identity(record, &provenance);
            let tool = sha256_hex(key.as_bytes());
            let mut matching = inner.test_run_observations.values().filter(|row|
                row.summary.owner_session_id.as_deref() == Some(session_id)
                    && row.runtime_identity == runtime && row.tool_identity == tool
                    && row.summary.ended_at.is_none() && row.declared_native.is_some());
            let (Some(row), None) = (matching.next(), matching.next()) else { return; };
            let start = row.declared_native.as_ref().unwrap();
            let Some(execution) = inner.test_runs.declared_executions.get(&start.run_id)
                .filter(|job| job.lock().expect("native execution mutex poisoned").reservation == *start)
                .cloned() else { return; };
            execution
        };
        self.freeze_native_declared_end(session_id, &provenance, key, execution, None, interrupted);
    }

    // Off-lock and before either provider receives this prompt. Failure only
    // withholds native credit; it never prevents the ordinary prompt.
    fn prepare_declared_run_before_handoff(&self, session_id: &str, runtime: &RuntimeToken, generation: u64) {
        #[cfg(test)]
        let now: Arc<dyn Fn() -> std::time::Instant + Send + Sync> = self.inner.lock().expect("state mutex poisoned")
            .test_declared_run_clock.clone().unwrap_or_else(|| Arc::new(std::time::Instant::now));
        #[cfg(not(test))]
        let now = std::time::Instant::now;
        let pending = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else { return; };
            inner.sessions[index].engram.declared_run_turn = None;
            let record = &inner.sessions[index];
            if !matches!(runtime, RuntimeToken::Codex(_) | RuntimeToken::Claude(_))
                || !record.runtime.matches_runtime_token(runtime)
                || record.active_turn_generation != generation
                || record.runtime_stop_in_progress
            { return; }
            let Some(store) = record.session.project_id.as_deref()
                .and_then(|id| inner.projects.iter().find(|project| project.id == id))
                .filter(|project| project.remote_id == LOCAL_REMOTE_ID)
                .and_then(|project| project.engram.as_ref())
                .and_then(|settings| settings.authority_store_key.clone()) else { return; };
            // Naming and its held-claim read own authority, including when
            // ordinary turn attribution is absent (control OFF/human prompt).
            let selected = match engram_turn_root_capture_locked(&inner, session_id) {
                EngramRootCapture::Recorded { workspace_id, generation, .. } => Some((workspace_id, generation)),
                _ => None,
            };
            let mut entries = inner.engram_work_source_roots.iter().filter(|entry|
                entry.store == store && entry.named_by_session == session_id
                    && selected.as_ref().is_none_or(|(root, generation)|
                        entry.root == *root && entry.generation as i64 == *generation));
            let (Some(entry), None) = (entries.next(), entries.next()) else { return; };
            let keys = TestRunWorktreeKeys {
                root: engram_exact_path_key(FsPath::new(&entry.root)),
                common_dir: entry.common_dir_key.clone(),
            };
            let named = match test_run_registration_named_binding(
                &inner, session_id, Some(&store), Some(&keys)) {
                Ok(binding) => binding,
                Err(error) => {
                    eprintln!("[termal] native opening named root refused: {error:?}");
                    return;
                }
            };
            let (Some(binding), Some(named)) = named else { return; };
            let Some(root_claim_fence) = test_run_named_root_fence(
                &inner, session_id, Some(&store), Some(&named)) else { return; };
            let pending = DeclaredRunTurn {
                runtime: runtime.clone(), generation, keys, store, binding, named, root_claim_fence,
                snapshot: None, opening_refusal: None, opening_refusal_code: None,
                first_tool: DeclaredRunFirstTool::default(),
                timings: Arc::default(),
                native_start_attempted: false,
                execution: None,
            };
            inner.sessions[index].engram.declared_run_turn = Some(pending.clone());
            pending
        };
        let root = PathBuf::from(&pending.named.workspace_id);
        // Cheap applicability only. A present declaration's committed membership
        // is still proved inside the same capture, never by an extra source read.
        if matches!(fs::symlink_metadata(root.join(ENGRAM_DECLARATION_FILE)),
            Err(error) if error.kind() == io::ErrorKind::NotFound) {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if let Some(index) = inner.find_session_index(session_id) {
                if inner.sessions[index].engram.declared_run_turn.as_ref()
                    .is_some_and(|turn| turn.runtime == pending.runtime && turn.generation == pending.generation) {
                    inner.sessions[index].engram.declared_run_turn = None;
                }
            }
            return;
        }
        let clock = self.engram_budget_clock();
        let deadline = now() + NATIVE_S0_CAPTURE_BUDGET + NATIVE_S0_AUTHORITY_MARGIN;
        let live = self.inner.lock().expect("state mutex poisoned").engram_turn_basis_captures_live.clone();
        let expected_keys = pending.keys.clone();
        let capture_state = self.clone();
        let capture_session = session_id.to_owned();
        let capture_pending = pending.clone();
        #[cfg(test)]
        let snapshot_capture = self.inner.lock().expect("state mutex poisoned").test_declared_run_snapshot.clone();
        let snapshot = bounded_content_revision_capture(&live, &self.capture_lifetime,
            ENGRAM_TURN_BASIS_CAPTURE_LIMIT, deadline.saturating_duration_since(now()), move || {
                let owner_capture = || {
                    capture_state.capture_declared_run_opening_with_owner(
                        &capture_session, &capture_pending, deadline, engram_source_owner_run)
                };
                #[cfg(test)]
                let captured = match snapshot_capture {
                    Some(capture) => {
                        let authority = declared_run_opening_authority_locked(
                            &capture_state.inner.lock().expect("state mutex poisoned"),
                            &capture_session, &capture_pending);
                        authority.and_then(|authority| capture(&root, deadline).map(|snapshot| (snapshot, authority)))
                    },
                    None => owner_capture(),
                };
                #[cfg(not(test))]
                let captured = owner_capture();
                Some(captured.and_then(|(snapshot, authority)| {
                    if test_run_worktree_keys(&root).as_ref() != Some(&expected_keys) {
                        anyhow::bail!("native opening root/common directory changed");
                    }
                    Ok((snapshot, authority))
                }).map_err(|error| {
                    let reason: String = format!("{error:#}").chars().take(2048).collect();
                    // The caller can exhaust its wait before this worker
                    // reports the existing Git operation/timing error.
                    eprintln!("[termal] native opening capture failed: {reason}");
                    reason
                }))
            });
        let (snapshot, source_owner_authority) = match snapshot {
            Some(Ok(snapshot)) => snapshot,
            Some(Err(error)) => { self.declared_run_opening_refused(session_id, &pending, &error); return; },
            None => { self.declared_run_opening_refused(session_id, &pending, "opening capture unavailable: deadline/capacity/cleanup"); return; },
        };
        let mut diagnostics = TestRunRegistrationDiagnostics::new(&self.server_instance_id);
        let probe = format!("native-opening-{}", Uuid::new_v4());
        let authority_deadline = native_opening_authority_deadline(&clock, deadline, now());
        let current = {
            let _timing = pending.timings.begin("opening_authority");
            self.read_test_run_current_named_binding(session_id, &probe,
                Some(&pending.keys), &clock, authority_deadline, &mut diagnostics)
        };
        let current = match current {
            Ok(current) => current,
            Err(error) => { self.declared_run_opening_refused(session_id, &pending, &format!("opening claim read: {error:?}")); return; },
        };
        if now() >= deadline {
            self.declared_run_opening_refused(session_id, &pending, "opening capture deadline elapsed");
            return;
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if declared_run_opening_authority_locked(&inner, session_id, &pending)
            .ok().as_ref() != Some(&source_owner_authority) { return; }
        let Some(index) = inner.find_session_index(session_id) else { return; };
        if !pending.is_current(&inner.sessions[index]) { return; }
        let Ok((Some(binding), Some(named))) = current.resolve_locked(
            &inner, session_id, Some(&pending.store), Some(&pending.keys)) else { return; };
        let Some(turn) = inner.sessions[index].engram.declared_run_turn.as_mut().filter(|turn|
            turn.runtime == pending.runtime && turn.generation == pending.generation
                && turn.keys == pending.keys
                && turn.first_tool == DeclaredRunFirstTool::Waiting) else { return; };
        if now() >= deadline {
            turn.opening_refusal = Some(format!("opening capture deadline elapsed before publication; s0_stages={}",
                turn.timings.summary()));
            turn.opening_refusal_code = Some("opening_deadline");
            eprintln!("[termal] native opening unavailable: {}", turn.opening_refusal.as_deref().unwrap());
            return;
        }
        turn.binding = binding;
        turn.named = named;
        turn.snapshot = Some(snapshot);
    }

    fn declared_run_opening_refused(&self, session_id: &str, expected: &DeclaredRunTurn, reason: &str) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else { return; };
        let Some(turn) = inner.sessions[index].engram.declared_run_turn.as_mut().filter(|turn|
            turn.runtime == expected.runtime && turn.generation == expected.generation && turn.snapshot.is_none()) else { return; };
        turn.opening_refusal = Some(format!("{}; s0_stages={}", reason.chars().take(2048).collect::<String>(),
            turn.timings.summary()));
        turn.opening_refusal_code = Some(if reason.contains("deadline") || reason.contains("TimedOut") {
            "opening_deadline"
        } else if reason.contains("declaration_not_committed") {
            "declaration_not_committed"
        } else if reason.starts_with("opening claim read:") {
            "authority_unavailable"
        } else { "opening_unavailable" });
        eprintln!("[termal] native opening unavailable: {}", turn.opening_refusal.as_deref().unwrap());
    }

    fn observe_declared_tool_activity(&self, session_id: &str, provenance: &EngramObservationProvenance,
        key: Option<&str>, is_command: bool) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else { return; };
        let record = &inner.sessions[index];
        let current = declared_run_original_turn(record, provenance);
        let valid = current && record.engram.declared_run_turn.as_ref().is_some_and(|turn| turn.is_current(record));
        if let Some(turn) = inner.sessions[index].engram.declared_run_turn.as_mut() {
            if valid { turn.first_tool.observe(key, is_command); }
            else { turn.first_tool = DeclaredRunFirstTool::Ineligible; }
        }
    }
}

fn declared_run_original_turn(record: &SessionRecord, provenance: &EngramObservationProvenance) -> bool {
    // A human-originated prompt is legitimate. The separate ordinary-turn
    // attribution policy does not grant or remove registered-run authority.
    match provenance {
        EngramObservationProvenance::Runtime { token, turn_generation } =>
            record.runtime.matches_runtime_token(token) && *turn_generation == record.active_turn_generation,
        EngramObservationProvenance::Claude(origin) =>
            record.runtime.matches_runtime_token(&origin.token)
                && matches!(origin.origin, ClaudeWorkOrigin::Attempt { turn_generation }
                    if turn_generation == record.active_turn_generation)
                && !unmediated_claude_turn_hides_observations(record),
        EngramObservationProvenance::Ambient => false,
    }
}
