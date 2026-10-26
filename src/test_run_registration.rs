// Registration boundary beside src/test_runs.rs and src/test_runs_api.rs.
// Owns durable launch identity, acknowledgement and bounded POST/GET diagnostic
// spans; does not execute tests,
// mint verification credit, settle tracker work, or authorize a restart.

#[cfg(test)]
thread_local! {
    // Deterministic connected-worker witness, after enqueue and before wait.
    static TEST_TEST_RUN_REGISTRATION_FENCE_SENT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

fn test_run_timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct TestRunRegistration {
    run_id: String,
    #[serde(default)]
    project_id: Option<String>,
    run_directory: String,
    // Host-captured canonical native bytes, independent of wire path spelling.
    #[serde(default)]
    locator_identity: Option<String>,
    worktree: String,
    request_identity: String,
    initiating_session_id: String,
    owning_session_id: String,
    creator_pid: Option<u32>,
    registered_at: String,
    started_at: Option<String>,
    preset: TestRunPreset,
    detached: Option<bool>,
    stage_names: Vec<String>,
    // Captured by the host, never accepted from the launcher's payload.
    work_binding: Option<EngramControlWorkBinding>,
    #[serde(default)]
    authority_store: Option<EngramAuthorityStoreKey>,
    validated_unbound: bool,
    #[serde(default)]
    monitoring_interrupted_at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct TestRunRegistrationView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    history_compacted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    artifact_unavailable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    compaction_reason: Option<String>,
    #[serde(default)]
    registration_state: Option<String>,
    #[serde(default)]
    tracking_durability: Option<String>,
    #[serde(default)]
    initiating_session_id: Option<String>,
    #[serde(default)]
    bound_work_ids: Vec<String>,
    #[serde(default)]
    creator_pid: Option<u32>,
    #[serde(default)]
    runner_pid: Option<u32>,
    #[serde(default)]
    monitoring_interrupted_at: Option<String>,
    // Null must be emitted: an absent/unknown correlation is never safe.
    #[serde(default)]
    restart_would_void_evidence: Option<bool>,
    #[serde(default)]
    restart_evidence_reason: String,
    #[serde(default)]
    observed_at: Option<String>,
    #[serde(default)]
    verification_record_ids: Vec<String>,
    #[serde(default)]
    source_fingerprints: Vec<String>,
    #[serde(default)]
    artifact_locations: Vec<String>,
    #[serde(default)]
    tracking_provenances: Vec<String>,
    // An exact artifact correlation replaces these provider-only identities
    // atomically in the client's projection; it never deletes durable evidence.
    #[serde(default)]
    superseded_run_ids: Vec<String>,
    #[serde(default)]
    start_time_origin: Option<String>,
    #[serde(default)]
    planned_stage_names: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RegisterTestRunRequest {
    run_id: String,
    run_directory: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RegisterTestRunResponse {
    contract: &'static str,
    version: u32,
    run_id: String,
    registered: bool,
}

// Diagnostics never decide durability or admission. Keep the existing error
// status and JSON envelope, with bounded, single-line phase information.
// Per operation: at most 16 completed phases and 8 lock observations; response
// detail is capped at 2048 bytes. These are transient observations, not the
// parent's retained diagnostic history or a durability/settlement authority.
fn test_run_diagnostic_id(value: &str) -> String {
    value.chars().filter(|ch| ch.is_ascii_alphanumeric() || matches!(*ch, '-' | '_')).take(128).collect()
}

struct TestRunRegistrationDiagnostics {
    started: std::time::Instant,
    phase_started: std::time::Instant,
    phase: &'static str,
    category: Option<&'static str>,
    cause: Option<String>,
    fence_error: Option<String>,
    writer_progress: Option<String>,
    attempt_id: String,
    operation_id: String,
    attempt_ordinal: String,
    server_instance: String,
    method: &'static str,
    enabled: bool,
    request_digest: Option<String>,
    lock_timings: Arc<Mutex<TestRunRegistrationLockTimings>>,
    spans: Vec<(&'static str, Duration, Duration)>,
    fence_deadline: Option<std::time::Instant>,
    durability_confirmed: bool,
    published: bool,
    failure_phase: Option<(&'static str, Duration)>,
    trace: Option<TestRunPhaseTrace>,
}

impl TestRunRegistrationDiagnostics {
    fn new(server_instance: &str) -> Self {
        let started = std::time::Instant::now();
        Self {
            started,
            phase_started: started,
            phase: "validate_request",
            category: None,
            cause: None,
            fence_error: None,
            writer_progress: None,
            attempt_id: Uuid::new_v4().to_string(),
            operation_id: "unavailable".to_owned(),
            attempt_ordinal: "unavailable".to_owned(),
            server_instance: test_run_truncate(server_instance, 64),
            method: "POST",
            enabled: true,
            request_digest: None,
            lock_timings: Arc::new(Mutex::new(TestRunRegistrationLockTimings::default())),
            spans: Vec::with_capacity(16),
            fence_deadline: None,
            durability_confirmed: false,
            published: false,
            failure_phase: None,
            trace: None,
        }
    }

    fn enter(&mut self, phase: &'static str) {
        if let Some(trace) = &self.trace { trace.mark(phase); }
        if self.spans.len() < 16 {
            self.spans.push((self.phase, self.phase_started.duration_since(self.started), self.phase_started.elapsed()));
        }
        self.phase = phase;
        self.phase_started = std::time::Instant::now();
    }

    fn correlate(&mut self, headers: &axum::http::HeaderMap) {
        self.operation_id = headers.get("x-termal-registration-operation")
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty() && value.len() <= 64
                && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
            .unwrap_or("unavailable").to_owned();
        self.attempt_ordinal = headers.get("x-termal-registration-attempt")
            .and_then(|value| value.to_str().ok())
            .filter(|value| matches!(*value, "1" | "2"))
            .unwrap_or("unavailable").to_owned();
    }

    fn lock<'a>(&self, state: &'a StateMutex<StateInner>) -> TestRunRegistrationGuard<'a> {
        TestRunRegistrationGuard {
            guard: state.lock().expect("state mutex poisoned"),
            timings: Arc::clone(&self.lock_timings),
            phase: self.phase,
        }
    }

    fn fields(&self, run_id: &str, category: &str, status: u16) -> String {
        let lock_timings = self.lock_timings.lock().expect("registration timing mutex poisoned");
        let waited = lock_timings.waited;
        let held = lock_timings.held;
        let locks = lock_timings.observations.iter().map(|(phase, wait, hold)| {
            format!("{phase}:{}/{}", wait.as_millis(), hold.as_millis())
        }).collect::<Vec<_>>().join(",");
        let (phase, phase_elapsed) = self.failure_phase.unwrap_or((self.phase, self.phase_started.elapsed()));
        let mut spans = self.spans.iter().map(|(phase, start, elapsed)| {
            format!("{phase}:{}-{}", start.as_millis(), (*start + *elapsed).as_millis())
        }).collect::<Vec<_>>().join(",");
        spans.push_str(&format!(",{}:{}-{}", self.phase,
            self.phase_started.duration_since(self.started).as_millis(), self.started.elapsed().as_millis()));
        let fields = format!(
            "phase={phase}; category={category}; elapsedMs={}; phaseElapsedMs={}; stateInnerWaitMs={}; stateInnerHoldMs={}; writerProgress={}; fenceError={}; attemptId={}; runId={}; operationId={}; attempt={}; serverInstance={}; method={}; httpStatus={status}; requestDigest={}; fenceBudgetMs={}; fenceRemainingMs={}; durabilityConfirmed={}; published={}; locks={}; spans={}",
            self.started.elapsed().as_millis(), phase_elapsed.as_millis(),
            waited.as_millis(), held.as_millis(), self.writer_progress.as_deref().unwrap_or("not_waiting"),
            test_run_truncate(self.fence_error.as_deref().unwrap_or("none"), 256).replace(['\r', '\n'], " "),
            self.attempt_id, test_run_diagnostic_id(run_id),
            self.operation_id, self.attempt_ordinal, self.server_instance, self.method,
            self.request_digest.as_deref().unwrap_or("unavailable"),
            if self.method == "POST" { "5000" } else { "not_applicable" },
            self.fence_deadline.map(|deadline| deadline.saturating_duration_since(std::time::Instant::now()).as_millis().to_string()).unwrap_or_else(|| "not_started".to_owned()),
            self.durability_confirmed, self.published, test_run_truncate(&locks, 384), test_run_truncate(&spans, 640),
        );
        test_run_truncate(&fields, 1600)
    }

    fn report(&self, run_id: &str, error: &mut ApiError) {
        let category = self.category.unwrap_or(if error.status.is_client_error() {
            "request_refused"
        } else {
            "handler_failure"
        });
        let message = format!("{}; {}", test_run_truncate(&error.message, 384), self.fields(run_id, category, error.status.as_u16()));
        error.message = test_run_truncate(&message, 2048)
            .replace('\r', " ")
            .replace('\n', " ");
        eprintln!(
            "[termal] test run {} failed run={}: {}; cause={}",
            self.method,
            test_run_diagnostic_id(run_id),
            error.message,
            test_run_truncate(self.cause.as_deref().unwrap_or("none"), 2048)
                .replace('\r', " ")
                .replace('\n', " "),
        );
    }
}

// Reuse the existing StateMutex acquisition measurements. Only accumulate
// durations while held; formatting and reporting happen after guard release.
#[derive(Default)]
struct TestRunRegistrationLockTimings {
    waited: Duration,
    held: Duration,
    observations: Vec<(&'static str, Duration, Duration)>,
}

struct TestRunRegistrationGuard<'a> {
    guard: StateMutexGuard<'a, StateInner>,
    timings: Arc<Mutex<TestRunRegistrationLockTimings>>,
    phase: &'static str,
}

impl std::ops::Deref for TestRunRegistrationGuard<'_> {
    type Target = StateInner;
    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl std::ops::DerefMut for TestRunRegistrationGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

impl Drop for TestRunRegistrationGuard<'_> {
    fn drop(&mut self) {
        let mut timings = self.timings.lock().expect("registration timing mutex poisoned");
        let waited = self.guard.waited;
        let held = self.guard.acquired_at.elapsed();
        timings.waited += waited;
        timings.held += held;
        if timings.observations.len() < 8 {
            timings.observations.push((self.phase, waited, held));
        }
    }
}

fn test_run_registration_identity(request: &Value) -> Result<String, ApiError> {
    let fields = [
        "runId",
        "root",
        "stages",
        "notifyTo",
        "full",
        "scope",
        "detached",
        "creatorPid",
        "owner",
        "started",
        "requiredBinaryEnv",
        "prerequisiteFiles",
        "needsCargo",
        "liveEngram",
    ];
    let immutable: BTreeMap<_, _> = fields
        .into_iter()
        .map(|name| (name, request.get(name).cloned().unwrap_or(Value::Null)))
        .collect();
    let bytes = serde_json::to_vec(&immutable)
        .map_err(|_| ApiError::bad_request("invalid test run registration request"))?;
    Ok(sha256_hex(&bytes))
}

impl AppState {
    #[cfg(test)]
    fn register_test_run(
        &self,
        session_id: &str,
        request: RegisterTestRunRequest,
    ) -> Result<RegisterTestRunResponse, ApiError> {
        self.register_test_run_with_diagnostics(session_id, request, TestRunRegistrationDiagnostics::new(&self.server_instance_id))
    }

    fn register_test_run_with_diagnostics(
        &self,
        session_id: &str,
        request: RegisterTestRunRequest,
        mut diagnostics: TestRunRegistrationDiagnostics,
    ) -> Result<RegisterTestRunResponse, ApiError> {
        let run_id = request.run_id.clone();
        if diagnostics.trace.is_none() {
            diagnostics.trace = Some(self.test_run_phase_traces.begin("registration", Some(&run_id),
                &diagnostics.operation_id, diagnostics.started));
        }
        diagnostics.enter("validate_request");
        let mut result = self.register_test_run_inner(session_id, request, &mut diagnostics);
        if result.is_err() {
            diagnostics.failure_phase = Some((diagnostics.phase, diagnostics.phase_started.elapsed()));
            diagnostics.enter("failure_publication");
            let mut inner = diagnostics.lock(&self.inner);
            if !inner.test_runs.registered.contains(&run_id)
                && inner
                    .test_run_registrations
                    .get(&run_id)
                    .is_some_and(|row| row.initiating_session_id == session_id)
            {
                inner.test_runs.registration_failures.insert(run_id.clone());
                if self.commit_delta_locked(&mut inner).is_ok() {
                    self.publish_state_locked(&inner);
                }
            }
        }
        if let Err(error) = &mut result {
            if let Some(trace) = &diagnostics.trace { trace.mark("failed"); }
            diagnostics.report(&run_id, error);
        } else {
            diagnostics.enter("response_ready");
            eprintln!("[termal] test run registration confirmed: {}", diagnostics.fields(&run_id, "confirmed", 201));
        }
        result
    }

    fn register_test_run_inner(
        &self,
        session_id: &str,
        request: RegisterTestRunRequest,
        diagnostics: &mut TestRunRegistrationDiagnostics,
    ) -> Result<RegisterTestRunResponse, ApiError> {
        if !request.run_id.starts_with("test-")
            || request.run_id.len() > 128
            || !request
                .run_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || request.run_directory.len() > 4096
        {
            return Err(ApiError::bad_request(
                "invalid test run registration locator",
            ));
        }
        diagnostics.enter("resolve_project");
        let project_root = {
            let inner = diagnostics.lock(&self.inner);
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("test run initiating session is unavailable"))?;
            let project = inner.sessions[index]
                .session
                .project_id
                .as_deref()
                .and_then(|id| inner.projects.iter().find(|project| project.id == id))
                .filter(|project| project.remote_id == LOCAL_REMOTE_ID)
                .ok_or_else(|| ApiError::conflict("test run requires a local project"))?;
            project.root_path.clone()
        };
        // Canonicalization and artifact reads stay outside StateInner.
        diagnostics.enter("canonicalize_directory");
        let run_directory = fs::canonicalize(&request.run_directory)
            .map_err(|_| ApiError::bad_request("test run directory is unavailable"))?;
        diagnostics.enter("validate_target_directory");
        let common = test_runs_git_common_dir(FsPath::new(&project_root)).ok_or_else(|| {
            ApiError::bad_request("test run project Git directory is unavailable")
        })?;
        let allowed = test_run_registration_target(&common, &run_directory);
        if !allowed
            || run_directory.file_name().and_then(|name| name.to_str())
                != Some(request.run_id.as_str())
        {
            return Err(ApiError::bad_request(
                "test run directory is outside this project's run directories",
            ));
        }
        diagnostics.enter("parse_request");
        let TestRunJson::Parsed(value, _, _) =
            test_run_read_json(&run_directory.join("request.json"))
        else {
            return Err(ApiError::bad_request(
                "test run request is unavailable, malformed or oversized",
            ));
        };
        if value["runId"].as_str() != Some(request.run_id.as_str())
            || value["owner"].as_str() != Some(session_id)
        {
            return Err(ApiError::conflict(
                "test run identity or initiating session does not match",
            ));
        }
        diagnostics.enter("index_request");
        let disk = TestRunDisk::read(&run_directory, None)
            .ok_or_else(|| ApiError::bad_request("test run request cannot be indexed"))?;
        if disk.locator_identity.is_none() {
            return Err(ApiError::bad_request("test run native locator identity is unavailable"));
        }
        let request_identity = test_run_registration_identity(&value)?;
        diagnostics.request_digest = Some(request_identity.clone());
        if disk.request_identity.as_ref() != Some(&request_identity) {
            return Err(ApiError::conflict(
                "test run request changed during registration",
            ));
        }
        let request_common = test_runs_git_common_dir(FsPath::new(&disk.worktree))
            .and_then(|directory| fs::canonicalize(directory).ok());
        if request_common != fs::canonicalize(&common).ok() {
            return Err(ApiError::bad_request("test run worktree is outside the configured project"));
        }
        if let Some(trace) = &diagnostics.trace { trace.mark("validated"); }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        diagnostics.fence_deadline = Some(deadline);
        diagnostics.enter("capture_registration");
        let (expected, dispatch) = {
            let mut inner = diagnostics.lock(&self.inner);
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::conflict("test run initiating session changed"))?;
            let project = inner.sessions[index]
                .session
                .project_id
                .as_deref()
                .and_then(|id| inner.projects.iter().find(|project| project.id == id))
                .filter(|project| {
                    project.remote_id == LOCAL_REMOTE_ID && project.root_path == project_root
                })
                .ok_or_else(|| {
                    ApiError::conflict("test run project changed during registration")
                })?;
            let registration = TestRunRegistration {
                run_id: request.run_id.clone(),
                project_id: Some(project.id.clone()),
                run_directory: test_run_display_path(&run_directory),
                locator_identity: disk.locator_identity.clone(),
                worktree: disk.worktree.clone(),
                request_identity,
                initiating_session_id: session_id.to_owned(),
                owning_session_id: session_id.to_owned(),
                creator_pid: disk.creator_pid,
                registered_at: test_run_timestamp(),
                started_at: disk.started_at.clone(),
                preset: disk.preset,
                detached: disk.detached,
                stage_names: value
                    .get("stages")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .take(TEST_RUN_STAGES_MAX)
                    .filter_map(|stage| stage.get("name").and_then(Value::as_str))
                    .filter(|name| test_run_valid_stage_name(name))
                    .map(str::to_owned)
                    .collect(),
                work_binding: inner.sessions[index].engram.work_binding.clone(),
                authority_store: project
                    .engram
                    .as_ref()
                    .and_then(|settings| settings.authority_store_key.clone()),
                validated_unbound: project.engram.is_none(),
                monitoring_interrupted_at: None,
            };
            if let Some(compacted) = inner.test_run_compacted.get(&request.run_id) {
                if compacted
                    .request_identity
                    .as_ref()
                    .is_some_and(|identity| identity != &registration.request_identity)
                {
                    return Err(ApiError::conflict(
                        "retained run has different immutable request content",
                    ));
                }
                if compacted.compacted {
                    let earlier = compacted.registration.as_ref().ok_or_else(|| {
                        ApiError::conflict("retained run identity has no registration to replay")
                    })?;
                    if earlier.request_identity != registration.request_identity
                        || earlier.locator_identity != registration.locator_identity
                        || earlier.run_directory != registration.run_directory
                        || earlier.initiating_session_id != session_id
                    {
                        return Err(ApiError::conflict(
                            "retained run identity was registered with different immutable content",
                        ));
                    }
                    if test_run_compact_is_installed(&inner, compacted) {
                        diagnostics.durability_confirmed = true;
                        if let Some(trace) = &diagnostics.trace { trace.mark("durable"); }
                        diagnostics.published = true;
                        return Ok(RegisterTestRunResponse {
                            contract: "termal-durable-test-run-registration",
                            version: 1,
                            run_id: request.run_id,
                            registered: true,
                        });
                    }
                }
            }
            let expected = if let Some(earlier) = inner.test_run_registrations.get(&request.run_id)
            {
                if earlier.request_identity != registration.request_identity
                    || earlier.locator_identity != registration.locator_identity
                    || earlier.run_directory != registration.run_directory
                    || earlier.initiating_session_id != session_id
                {
                    return Err(ApiError::conflict(
                        "test run identity was already registered with different content",
                    ));
                }
                earlier.clone()
            } else {
                inner
                    .test_run_registrations
                    .insert(request.run_id.clone(), registration.clone());
                // The launcher fingerprints its input and runs its stages
                // only after this; a replay keeps the first registration's place.
                EngramHost::note_test_run_registered(&run_directory);
                registration
            };
            diagnostics.enter("persist_enqueue");
            let dispatch = self
                .persist_internal_locked_with_dispatch(&inner)
                .map_err(|error| {
                    diagnostics.category = Some("persist_write_failed");
                    diagnostics.cause = Some(format!("{error:#}"));
                    ApiError::internal("test run registration persistence failed")
                })?;
            (expected, dispatch)
        };
        diagnostics.writer_progress = Some(match dispatch {
            PersistDispatch::BackgroundQueued => "background Delta queued".to_owned(),
            PersistDispatch::Synchronous => "synchronous write confirmed".to_owned(),
        });
        if matches!(dispatch, PersistDispatch::BackgroundQueued) {
            diagnostics.enter("durability_fence_send");
            let (fence, waiter) = PersistFence::new(
                PersistFenceTarget::TestRunRegistration(expected.clone()),
                deadline,
            );
            self.persist_tx
                .send(PersistRequest::Fence(Box::new(fence)))
                .map_err(|_| {
                    diagnostics.category = Some("worker_unavailable");
                    ApiError::internal("test run registration persistence is unavailable")
                })?;
            #[cfg(test)]
            TEST_TEST_RUN_REGISTRATION_FENCE_SENT.with(|slot| {
                if let Some(step) = slot.borrow_mut().take() {
                    step();
                }
            });
            diagnostics.enter("durability_wait");
            match waiter.wait_until(deadline) {
                Some(Ok(())) => {
                    diagnostics.writer_progress = Some(waiter.progress().describe());
                }
                Some(Err(error)) => {
                    diagnostics.category = Some(match &error {
                        PersistFenceError::Deadline => "deadline",
                        PersistFenceError::WriteFailed(_) => "persist_write_failed",
                        PersistFenceError::Shutdown => "worker_shutdown",
                        PersistFenceError::WorkerStopped => "worker_stopped",
                        PersistFenceError::Superseded => "superseded",
                    });
                    diagnostics.cause = Some(format!("{error:?}"));
                    diagnostics.fence_error = Some(format!("{error:?}"));
                    diagnostics.writer_progress = Some(waiter.progress().describe());
                    if matches!(error, PersistFenceError::Deadline) {
                        return Err(ApiError::enqueued_durability_deadline(
                            "test run registration persistence is not yet acknowledged",
                            json!({
                                "runId": expected.run_id,
                                "requestDigest": expected.request_identity,
                                "ownerSessionId": expected.owning_session_id,
                            }),
                        ).at_durability_boundary(DurabilityPendingOperation::TestRunRegistration));
                    }
                    return Err(ApiError::internal(
                        "test run registration durability failed",
                    ));
                }
                None => {
                    diagnostics.category = Some("unresolved");
                    diagnostics.writer_progress = Some(waiter.progress().describe());
                    return Err(ApiError::internal(
                        "test run registration durability is unresolved",
                    ));
                }
            }
        }
        diagnostics.durability_confirmed = true;
        if let Some(trace) = &diagnostics.trace { trace.mark("durable"); }
        // Replace the rescan's former identity recheck with one bounded read
        // of this request. A mutation while its fence waited is not an ACK.
        diagnostics.enter("recheck_request");
        let TestRunJson::Parsed(current, _, _) = test_run_read_json(&run_directory.join("request.json")) else {
            return Err(ApiError::conflict("test run request became unavailable before acknowledgement"));
        };
        if test_run_registration_identity(&current)? != expected.request_identity {
            return Err(ApiError::conflict("test run request changed before acknowledgement"));
        }
        diagnostics.enter("publish_registration");
        {
            let mut inner = diagnostics.lock(&self.inner);
            if inner.test_run_registrations.get(&request.run_id) != Some(&expected) {
                return Err(ApiError::conflict(
                    "test run registration changed before acknowledgement",
                ));
            }
            inner.test_runs.registered.insert(request.run_id.clone());
            inner
                .test_runs
                .registration_failures
                .remove(&request.run_id);
            diagnostics.published = true;
        }
        // Durability, not an unrelated global rescan, is the ACK boundary.
        // The background index remains its only writer; exact GET can use
        // the durable registration until that scanner catches up.
        diagnostics.enter("query_confirmation");
        let inner = diagnostics.lock(&self.inner);
        if !inner.test_runs.registered.contains(&request.run_id)
            || inner.test_run_registrations.get(&request.run_id) != Some(&expected) {
            diagnostics.category = Some("query_unavailable");
            return Err(ApiError::internal(
                "durable test run registration is not available in the host query",
            ));
        }
        Ok(RegisterTestRunResponse {
            contract: "termal-durable-test-run-registration",
            version: 1,
            run_id: request.run_id,
            registered: true,
        })
    }
}

// The disk index versions file bytes as `sha256:<hash>`, while carried checks
// retain the raw SHA-256. Compare both exact identities in the raw form here.
fn test_run_terminal_digest_matches(version: Option<&str>, raw_digest: &str) -> bool {
    version.and_then(|version| version.strip_prefix("sha256:")) == Some(raw_digest)
}

fn test_run_registration_final_observation<'a>(
    registration: &TestRunRegistration,
    observations: &'a BTreeMap<String, TestRunCommandObservation>,
    durable: Option<&HashSet<String>>,
) -> Option<&'a TestRunCommandObservation> {
    test_run_registration_final_observation_from_candidates(registration, observations.values(), durable)
}

fn test_run_registration_final_observation_from_candidates<'a>(
    registration: &TestRunRegistration,
    observations: impl IntoIterator<Item = &'a TestRunCommandObservation>,
    durable: Option<&HashSet<String>>,
) -> Option<&'a TestRunCommandObservation> {
    if !registration.validated_unbound || registration.work_binding.is_some() {
        return None;
    }
    let candidates: Vec<_> = observations
        .into_iter()
        .inspect(|_| {
            #[cfg(test)]
            TEST_TEST_RUN_ASSOCIATION_VISITS.with(|count| count.set(count.get() + 1));
        })
        .filter(|row| {
            row.summary.owner_session_id.as_deref() == Some(&registration.initiating_session_id)
                && row.summary.project_id == registration.project_id
                && test_run_path_key(&row.summary.worktree)
                    == test_run_path_key(&registration.worktree)
                && row.artifact.as_ref().is_some_and(|artifact| {
                    artifact.run_id == registration.run_id
                        && test_run_path_key(&artifact.run_directory)
                            == test_run_path_key(&registration.run_directory)
                        && artifact.request_identity.as_ref()
                            == Some(&registration.request_identity)
                })
        })
        .collect();
    candidates
        .iter()
        .all(|row| {
            test_run_final_unbound_valid(row)
                && durable.is_none_or(|ids| ids.contains(&row.execution_id))
        })
        .then(|| candidates.first().copied())
        .flatten()
}

fn test_run_registration_view(inner: &StateInner, disk: &TestRunDisk) -> TestRunRegistrationView {
    test_run_registration_view_from_observations(inner, disk, inner.test_run_observations.values())
}

// Shared exact locator/content match. Project attribution needs this match
// and the durable registration marker, not the unrelated restart-risk scan.
fn test_run_matching_registration<'a>(
    inner: &'a impl TestRunProjectionSource,
    disk: &TestRunDisk,
) -> Option<&'a TestRunRegistration> {
    inner.projection_registrations().get(&disk.run_id).filter(|registration| {
        registration.locator_identity.is_some()
            && registration.locator_identity == disk.locator_identity
            && test_run_path_key(&registration.run_directory)
                == test_run_path_key(&test_run_display_path(&disk.run_dir))
            && disk.owner.as_deref() == Some(&registration.initiating_session_id)
            && disk.request_identity.as_ref() == Some(&registration.request_identity)
    })
}

fn test_run_registration_view_from_observations<'a>(
    inner: &impl TestRunProjectionSource,
    disk: &TestRunDisk,
    observations: impl IntoIterator<Item = &'a TestRunCommandObservation>,
) -> TestRunRegistrationView {
    let registration = test_run_matching_registration(inner, disk);
    let durable = registration.is_some() && inner.projection_index().registered.contains(&disk.run_id);
    let mut view = TestRunRegistrationView {
        registration_state: Some(
            if durable {
                "registered"
            } else if registration.is_some() {
                "registrationPending"
            } else {
                "discovered"
            }
            .to_owned(),
        ),
        tracking_provenances: vec![
            if durable {
                "launcherRegistered"
            } else {
                if registration.is_some() {
                    "registrationAttempt"
                } else {
                    "discovered"
                }
            }
            .to_owned(),
        ],
        tracking_durability: Some(
            if durable {
                "durable"
            } else if inner.projection_index().registration_failures.contains(&disk.run_id) {
                "failed"
            } else {
                "unconfirmed"
            }
            .to_owned(),
        ),
        initiating_session_id: registration.map(|row| row.initiating_session_id.clone()),
        bound_work_ids: registration
            .into_iter()
            .flat_map(|row| {
                row.work_binding
                    .iter()
                    .map(|binding| binding.work_id.clone())
            })
            .collect(),
        creator_pid: disk.creator_pid,
        start_time_origin: Some("launcherRequest".to_owned()),
        planned_stage_names: registration
            .map(|row| row.stage_names.clone())
            .unwrap_or_default(),
        runner_pid: disk.responsible_pid().map(|(pid, _)| pid),
        monitoring_interrupted_at: registration
            .and_then(|row| row.monitoring_interrupted_at.clone()),
        restart_would_void_evidence: None,
        restart_evidence_reason:
            "Artifact status does not establish exact verification settlement or work completion."
                .to_owned(),
        ..TestRunRegistrationView::default()
    };
    if registration.is_some() && inner.projection_index().registration_failures.contains(&disk.run_id) {
        view.restart_evidence_reason = "Registration persistence or query confirmation failed; no durable acknowledgement was issued.".to_owned();
    }
    if durable
        && registration.is_some_and(|row| {
            row.validated_unbound
                && row.work_binding.is_none()
                && if disk.is_terminal() {
                    test_run_registration_final_observation_from_candidates(
                        row,
                        observations,
                        Some(&inner.projection_index().observations_durable),
                    )
                    .is_some()
                } else {
                    row.monitoring_interrupted_at.is_none()
                }
        })
    {
        view.restart_would_void_evidence = Some(false);
        view.restart_evidence_reason =
            "Validated launch without a work binding; no work verification is pending.".to_owned();
    }
    // The existing carried-check path has already correlated the exact
    // artifact locator, first terminal digest, root, owner and claim. Its
    // positive hazard outlives artifact PASS and dominates weaker guesses.
    let carried = test_run_projection_carried_matches(
        inner, &disk.run_dir, disk.results.as_ref().map(|row| row.digest.as_str()), true,
    );
    if carried {
        view.restart_would_void_evidence = Some(true);
        view.restart_evidence_reason = "Exact carried run is awaiting host verification settlement; restarting would lose its pending credit.".to_owned();
    }
    view
}

fn test_run_registered_fallback(inner: &StateInner, row: &TestRunRegistration) -> TestRunSummary {
    test_run_registered_fallback_from_observations(inner, row, inner.test_run_observations.values())
}

fn test_run_registered_fallback_from_observations<'a>(
    inner: &StateInner,
    row: &TestRunRegistration,
    observations: impl IntoIterator<Item = &'a TestRunCommandObservation>,
) -> TestRunSummary {
    let durable = inner.test_runs.registered.contains(&row.run_id);
    let failed = inner.test_runs.registration_failures.contains(&row.run_id);
    let terminal = test_run_registration_final_observation_from_candidates(
        row,
        observations,
        Some(&inner.test_runs.observations_durable),
    );
    let unbound = durable && terminal.is_some();
    TestRunSummary {
        registration: TestRunRegistrationView {
            registration_state: Some(if durable { "registered" } else { "registrationPending" }.to_owned()),
            tracking_provenances: vec![if durable { "launcherRegistered" } else { "registrationAttempt" }.to_owned()],
            tracking_durability: Some(if durable { "durable" } else if failed { "failed" } else { "unconfirmed" }.to_owned()),
            initiating_session_id: Some(row.initiating_session_id.clone()),
            bound_work_ids: row.work_binding.iter().map(|binding| binding.work_id.clone()).collect(),
            creator_pid: row.creator_pid, monitoring_interrupted_at: row.monitoring_interrupted_at.clone(),
            start_time_origin: Some("launcherRequest".to_owned()),
            planned_stage_names: row.stage_names.clone(),
            restart_would_void_evidence: unbound.then_some(false),
            restart_evidence_reason: if failed { "Registration persistence or query confirmation failed; no durable acknowledgement was issued." }
                else if unbound { "Validated launch without a work binding." }
                else { "Registered launch remains visible; current artifacts and exact verification correlation are unavailable." }.to_owned(),
            ..TestRunRegistrationView::default()
        },
        run_id: row.run_id.clone(), project_id: row.project_id.clone(), worktree: row.worktree.clone(),
        run_dir: row.run_directory.clone(), preset: row.preset, command: None, command_truncated: false,
        detached: row.detached, state: terminal.map_or(TestRunState::Unknown, |observation| observation.summary.state),
        unknown_reason: terminal.is_none().then_some(TestRunUnknownReason::ResultsUnreadable),
        interrupted: terminal.is_none() && row.monitoring_interrupted_at.is_some(), current_stage: None,
        stages: Vec::new(), owner_session_id: Some(row.owning_session_id.clone()), notify_to: None, notify_session_id: None,
        started_at: row.started_at.clone(), ended_at: terminal.and_then(|observation| observation.summary.ended_at.clone()),
        exit_code: terminal.and_then(|observation| observation.summary.exit_code),
        error: terminal.is_none().then(|| "Current execution state is not confirmed by readable artifacts.".to_owned()), detail_version: None,
    }
}

async fn register_session_test_run(
    AxumPath(session_id): AxumPath<String>,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    request: Result<Json<RegisterTestRunRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<RegisterTestRunResponse>), ApiError> {
    let Json(request) =
        request.map_err(|error| api_json_rejection("test run registration", error))?;
    // This span begins at handler entry, after Axum's body extraction. It is
    // deliberately not labelled as network arrival or request-body time.
    let dispatched_at = std::time::Instant::now();
    let mut diagnostics = TestRunRegistrationDiagnostics::new(&state.server_instance_id);
    diagnostics.started = dispatched_at;
    diagnostics.phase_started = dispatched_at;
    diagnostics.phase = "blocking_dispatch";
    diagnostics.correlate(&headers);
    diagnostics.trace = Some(state.test_run_phase_traces.begin("registration", Some(&request.run_id),
        &diagnostics.operation_id, dispatched_at));
    let response = run_blocking_api(move || {
        if let Some(trace) = &diagnostics.trace { trace.mark("blocking_dispatched"); }
        state.register_test_run_with_diagnostics(&session_id, request, diagnostics)
    }).await?;
    Ok((StatusCode::CREATED, Json(response)))
}
