// Registration boundary beside src/test_runs.rs and src/test_runs_api.rs.
// Owns durable launch identity and acknowledgement; does not execute tests,
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
    fn register_test_run(
        &self,
        session_id: &str,
        request: RegisterTestRunRequest,
    ) -> Result<RegisterTestRunResponse, ApiError> {
        let run_id = request.run_id.clone();
        let result = self.register_test_run_inner(session_id, request);
        if result.is_err() {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if !inner.test_runs.registered.contains(&run_id)
                && inner
                    .test_run_registrations
                    .get(&run_id)
                    .is_some_and(|row| row.initiating_session_id == session_id)
            {
                inner.test_runs.registration_failures.insert(run_id);
                if self.commit_delta_locked(&mut inner).is_ok() {
                    self.publish_state_locked(&inner);
                }
            }
        }
        result
    }

    fn register_test_run_inner(
        &self,
        session_id: &str,
        request: RegisterTestRunRequest,
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
        let project_root = {
            let inner = self.inner.lock().expect("state mutex poisoned");
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
        let run_directory = fs::canonicalize(&request.run_directory)
            .map_err(|_| ApiError::bad_request("test run directory is unavailable"))?;
        let common = test_runs_git_common_dir(FsPath::new(&project_root)).ok_or_else(|| {
            ApiError::bad_request("test run project Git directory is unavailable")
        })?;
        let mut stamps = Vec::new();
        let allowed = test_runs_directories(&common, &mut stamps)
            .into_iter()
            .filter_map(|directory| fs::canonicalize(directory).ok())
            .any(|directory| directory == run_directory);
        if !allowed
            || run_directory.file_name().and_then(|name| name.to_str())
                != Some(request.run_id.as_str())
        {
            return Err(ApiError::bad_request(
                "test run directory is outside this project's run directories",
            ));
        }
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
        let disk = TestRunDisk::read(&run_directory, None)
            .ok_or_else(|| ApiError::bad_request("test run request cannot be indexed"))?;
        let request_identity = test_run_registration_identity(&value)?;
        if disk.request_identity.as_ref() != Some(&request_identity) {
            return Err(ApiError::conflict(
                "test run request changed during registration",
            ));
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let (expected, dispatch) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
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
                        || earlier.run_directory != registration.run_directory
                        || earlier.initiating_session_id != session_id
                    {
                        return Err(ApiError::conflict(
                            "retained run identity was registered with different immutable content",
                        ));
                    }
                    if test_run_compact_is_installed(&inner, compacted) {
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
                registration
            };
            let dispatch = self
                .persist_internal_locked_with_dispatch(&inner)
                .map_err(|_| ApiError::internal("test run registration persistence failed"))?;
            (expected, dispatch)
        };
        if matches!(dispatch, PersistDispatch::BackgroundQueued) {
            let (fence, waiter) = PersistFence::new(
                PersistFenceTarget::TestRunRegistration(expected.clone()),
                deadline,
            );
            self.persist_tx
                .send(PersistRequest::Fence(Box::new(fence)))
                .map_err(|_| {
                    ApiError::internal("test run registration persistence is unavailable")
                })?;
            #[cfg(test)]
            TEST_TEST_RUN_REGISTRATION_FENCE_SENT.with(|slot| {
                if let Some(step) = slot.borrow_mut().take() {
                    step();
                }
            });
            waiter
                .wait_until(deadline)
                .ok_or_else(|| {
                    ApiError::internal("test run registration durability is unresolved")
                })?
                .map_err(|_| ApiError::internal("test run registration durability failed"))?;
        }
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
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
        }
        self.refresh_test_runs();
        let inner = self.inner.lock().expect("state mutex poisoned");
        if !inner.test_runs.find(&request.run_id).is_some_and(|entry| {
            test_run_path_key(&entry.summary.run_dir) == test_run_path_key(&expected.run_directory)
                && entry.disk.request_identity.as_ref() == Some(&expected.request_identity)
        }) {
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
    if !registration.validated_unbound || registration.work_binding.is_some() {
        return None;
    }
    let candidates: Vec<_> = observations
        .values()
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
    let registration = inner
        .test_run_registrations
        .get(&disk.run_id)
        .filter(|registration| {
            test_run_path_key(&registration.run_directory)
                == test_run_path_key(&test_run_display_path(&disk.run_dir))
                && disk.owner.as_deref() == Some(&registration.initiating_session_id)
                && disk.request_identity.as_ref() == Some(&registration.request_identity)
        });
    let durable = registration.is_some() && inner.test_runs.registered.contains(&disk.run_id);
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
            } else if inner.test_runs.registration_failures.contains(&disk.run_id) {
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
    if registration.is_some() && inner.test_runs.registration_failures.contains(&disk.run_id) {
        view.restart_evidence_reason = "Registration persistence or query confirmation failed; no durable acknowledgement was issued.".to_owned();
    }
    if durable
        && registration.is_some_and(|row| {
            row.validated_unbound
                && row.work_binding.is_none()
                && if disk.is_terminal() {
                    test_run_registration_final_observation(
                        row,
                        &inner.test_run_observations,
                        Some(&inner.test_runs.observations_durable),
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
    let carried = inner
        .sessions
        .iter()
        .flat_map(|record| &record.engram.carried_checks)
        .any(|carried| {
            carried.run_directory.as_ref().is_some_and(|path| {
                test_run_path_key(&test_run_display_path(path))
                    == test_run_path_key(&test_run_display_path(&disk.run_dir))
            }) && carried.terminal_digest.as_ref().is_some_and(|digest| {
                disk.results.as_ref().is_some_and(|results| {
                    test_run_terminal_digest_matches(Some(&results.digest), digest)
                })
            }) && !carried.terminal_conflict
        });
    if carried {
        view.restart_would_void_evidence = Some(true);
        view.restart_evidence_reason = "Exact carried run is awaiting host verification settlement; restarting would lose its pending credit.".to_owned();
    }
    view
}

fn test_run_registered_fallback(inner: &StateInner, row: &TestRunRegistration) -> TestRunSummary {
    let durable = inner.test_runs.registered.contains(&row.run_id);
    let failed = inner.test_runs.registration_failures.contains(&row.run_id);
    let terminal = test_run_registration_final_observation(
        row,
        &inner.test_run_observations,
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
    request: Result<Json<RegisterTestRunRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<RegisterTestRunResponse>), ApiError> {
    let Json(request) =
        request.map_err(|error| api_json_rejection("test run registration", error))?;
    let response = run_blocking_api(move || state.register_test_run(&session_id, request)).await?;
    Ok((StatusCode::CREATED, Json(response)))
}
