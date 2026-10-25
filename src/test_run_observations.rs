// Host-observed test executions beside test_runs.rs. New file.
// Owns durable provider execution identity and visibility; command recognition,
// artifact validation, verification credit and tracker lifecycle stay with
// their existing adapters. A native exit never constitutes verification.

#[cfg(test)]
thread_local! {
    static TEST_TEST_RUN_ASSOCIATION_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct TestRunCommandObservation {
    execution_id: String,
    runtime_identity: Option<String>,
    tool_identity: String,
    grant_identity: Option<String>,
    command_fingerprint: String,
    turn_generation: u64,
    observed_at: String,
    summary: TestRunSummary,
    work_binding: Option<EngramControlWorkBinding>,
    source_root: Option<EngramWorkSourceRoot>,
    validated_unbound: bool,
    #[serde(default)]
    monitoring_interrupted_at: Option<String>,
    #[serde(default)]
    evidence: Vec<TestRunEvidenceAssociation>,
    #[serde(default)]
    artifact: Option<TestRunObservedArtifact>,
    #[serde(default)]
    declared_artifact: Option<String>,
    /// A successful command result acknowledged only a detached/background launch.
    #[serde(default)]
    launch_only: bool,
    // Minted only at the original execution's terminal provider boundary,
    // after positive host checks of both binding and reporting absence.
    #[serde(default)]
    final_unbound_proof: Option<TestRunFinalUnboundProof>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct TestRunFinalUnboundProof {
    version: u8,
    terminal_identity: String,
}

fn test_run_final_unbound_identity(row: &TestRunCommandObservation) -> Option<String> {
    if !row.validated_unbound
        || row.work_binding.is_some()
        || row.source_root.is_some()
        || row.grant_identity.is_some()
        || !row.evidence.is_empty()
        || row.launch_only
        || row.runtime_identity.is_none()
        || row.summary.ended_at.is_none()
        || !matches!(
            row.summary.state,
            TestRunState::Passed | TestRunState::Failed
        )
    {
        return None;
    }
    // These are exact original execution and terminal facts, not mutable
    // runtime handles, current project configuration or artifact discovery.
    serde_json::to_vec(&(
        &row.execution_id,
        &row.runtime_identity,
        &row.tool_identity,
        &row.command_fingerprint,
        row.turn_generation,
        &row.summary.owner_session_id,
        &row.summary.project_id,
        &row.summary.worktree,
        &row.summary.started_at,
        &row.summary.ended_at,
        row.summary.state,
        row.summary.exit_code,
    ))
    .ok()
    .map(|bytes| sha256_hex(&bytes))
}

fn test_run_final_unbound_valid(row: &TestRunCommandObservation) -> bool {
    row.final_unbound_proof.as_ref().is_some_and(|proof| {
        proof.version == 1
            && test_run_final_unbound_identity(row).as_ref() == Some(&proof.terminal_identity)
    })
}

fn test_run_can_finalize_unbound(
    inner: &StateInner,
    index: usize,
    row: &TestRunCommandObservation,
) -> bool {
    let record = &inner.sessions[index];
    let engram = &record.engram;
    row.validated_unbound
        && row.summary.owner_session_id.as_deref() == Some(&record.session.id)
        && row.summary.project_id == record.session.project_id
        && row.runtime_identity.is_some()
        && row.runtime_identity
            == test_run_runtime_identity(record, &EngramObservationProvenance::Ambient)
        && row.turn_generation == record.active_turn_generation
        && engram_project_for_session_locked(inner, &record.session.id)
            .is_some_and(|project| project.engram.is_none())
        && engram.work_binding.is_none()
        && engram.active_grant_id.is_none()
        && engram.uncertain_grant_id.is_none()
        && engram.routing_token.is_none()
        && engram.admission_attempt.is_none()
        && engram.admission_in_progress.is_none()
        && engram.source_observation_preparations == 0
        && engram.active_turn_checks.is_empty()
        && engram.carried_checks.is_empty()
        && engram.running_codex_tests.is_empty()
        && engram.carried_unmatched_launches.is_empty()
        && engram.active_turn_report.is_none()
        && engram.active_turn_report_fallback.is_none()
}

fn test_run_observation_boot(row: &mut TestRunCommandObservation) {
    // A known terminal execution is a durable fact. Only outstanding
    // execution/reporting monitoring acquires a new interruption marker.
    if row.summary.ended_at.is_none() {
        row.monitoring_interrupted_at
            .get_or_insert_with(test_run_timestamp);
        row.summary.state = TestRunState::Unknown;
        row.summary.interrupted = true;
    } else if row.evidence.iter().any(|association| {
        association.verification_record_id.is_none() || association.checkpoint_id.is_none()
    }) {
        row.monitoring_interrupted_at
            .get_or_insert_with(test_run_timestamp);
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct TestRunObservedArtifact {
    run_id: String,
    run_directory: String,
    request_identity: Option<String>,
    terminal_digest: Option<String>,
}

// Borrowed candidate selection for one locked projection, never cached proof.
// Every selected row still passes the exact owner/path/request-or-digest and
// current durability/lifecycle predicates. Rebuilding at each cut also sees
// unstamped in-memory changes; run-id equality alone never grants authority.
struct TestRunProjection<'a> {
    entries: HashMap<&'a str, &'a TestRunEntry>,
    observations: HashMap<&'a str, Vec<&'a TestRunCommandObservation>>,
}

impl<'a> TestRunProjection<'a> {
    fn new(inner: &'a StateInner) -> Self {
        let mut entries = HashMap::new();
        for entry in &inner.test_runs.entries {
            entries.entry(entry.summary.run_id.as_str()).or_insert(entry);
        }
        let mut observations: HashMap<&str, Vec<_>> = HashMap::new();
        for row in inner.test_run_observations.values() {
            if let Some(artifact) = &row.artifact {
                observations.entry(artifact.run_id.as_str()).or_default().push(row);
            }
        }
        Self { entries, observations }
    }

    fn observations(&self, run_id: &str) -> &[&'a TestRunCommandObservation] {
        self.observations.get(run_id).map(Vec::as_slice).unwrap_or(&[])
    }
}

// Waiters are process-local and never part of the persisted authority image.
#[derive(Clone, Default)]
struct TestRunObservationPersistence(
    Arc<Mutex<BTreeMap<String, (TestRunCommandObservation, PersistFenceWaiter)>>>,
);

#[derive(Clone, Debug)]
struct TestRunCommandOrigin {
    session_id: String,
    project_id: Option<String>,
    runtime_identity: Option<String>,
    tool_identity: String,
    turn_generation: u64,
    grant_identity: Option<String>,
    work_binding: Option<EngramControlWorkBinding>,
    source_root: Option<EngramWorkSourceRoot>,
    validated_unbound: bool,
}

// Only provider starts whose command text is not available yet. Process-local
// attribution, cleared at description/end/runtime loss; never credit authority.
#[derive(Clone, Debug)]
struct TestRunUnclassifiedCommand {
    origin: TestRunCommandOrigin,
    workdir: String,
    started_at: String,
    directories: Option<Vec<Option<String>>>,
}

impl TestRunCommandOrigin {
    fn execution_id(&self) -> String {
        format!(
            "observed-{}",
            sha256_hex(
                format!(
                    "{}:{:?}:{}:{}",
                    self.session_id,
                    self.runtime_identity,
                    self.turn_generation,
                    self.tool_identity
                )
                .as_bytes()
            )
        )
    }
    fn capture(
        inner: &StateInner,
        index: usize,
        provenance: &EngramObservationProvenance,
        key: &str,
    ) -> Self {
        let record = &inner.sessions[index];
        let project = engram_project_for_session_locked(inner, &record.session.id);
        let owns_current = claude_observation_disposition(record, provenance)
            == ClaudeObservationDisposition::Current;
        let work_binding = owns_current
            .then(|| record.engram.work_binding.clone())
            .flatten();
        Self {
            session_id: record.session.id.clone(),
            project_id: record.session.project_id.clone(),
            runtime_identity: test_run_runtime_identity(record, provenance),
            tool_identity: sha256_hex(key.as_bytes()),
            turn_generation: match provenance {
                EngramObservationProvenance::Claude(origin) => match &origin.origin {
                    ClaudeWorkOrigin::Attempt { turn_generation } => *turn_generation,
                    ClaudeWorkOrigin::Unattributed => record.active_turn_generation,
                },
                EngramObservationProvenance::Ambient => record.active_turn_generation,
            },
            grant_identity: owns_current
                .then(|| {
                    record
                        .engram
                        .active_grant_id
                        .as_ref()
                        .map(|grant| sha256_hex(grant.as_bytes()))
                })
                .flatten(),
            work_binding: work_binding.clone(),
            source_root: project
                .and_then(|project| project.engram.as_ref())
                .and_then(|settings| settings.authority_store_key.as_ref())
                .and_then(|store| {
                    work_binding.as_ref().and_then(|binding| {
                        inner
                            .engram_work_source_roots
                            .iter()
                            .find(|root| {
                                &root.store == store
                                    && root.work_id == binding.work_id
                                    && root.claim_id == binding.claim_id
                            })
                            .cloned()
                    })
                }),
            validated_unbound: owns_current
                && project.is_some_and(|project| project.engram.is_none())
                && record.engram.work_binding.is_none(),
        }
    }
}

impl std::fmt::Debug for TestRunObservationPersistence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TestRunObservationPersistence")
            .finish_non_exhaustive()
    }
}

fn test_run_runtime_identity(
    record: &SessionRecord,
    provenance: &EngramObservationProvenance,
) -> Option<String> {
    let runtime = match provenance {
        EngramObservationProvenance::Ambient => record.runtime.runtime_token(),
        EngramObservationProvenance::Claude(origin) => Some(origin.token.clone()),
    }?;
    // Runtime tokens and provider tool keys remain host-private. Only opaque
    // hashes participate in the durable identity and no token reaches the UI.
    Some(sha256_hex(format!("{runtime:?}").as_bytes()))
}

impl AppState {
    fn remember_unclassified_test_command(
        &self,
        origin: TestRunCommandOrigin,
        workdir: String,
        started_at: String,
        directories: Option<Vec<Option<String>>>,
    ) {
        if origin.runtime_identity.is_none() {
            return;
        }
        self.inner
            .lock()
            .expect("state mutex poisoned")
            .test_runs
            .unclassified_commands
            .entry(origin.execution_id())
            .or_insert(TestRunUnclassifiedCommand {
                origin,
                workdir,
                started_at,
                directories,
            });
    }

    fn observe_test_command_description(
        &self,
        session_id: &str,
        provenance: &EngramObservationProvenance,
        key: &str,
        ran: Option<&str>,
        cwd: Option<&str>,
    ) -> bool {
        let Some(ran) = ran else {
            return false;
        };
        let pending = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return false;
            };
            let runtime = test_run_runtime_identity(&inner.sessions[index], provenance);
            let tool = sha256_hex(key.as_bytes());
            let mut candidates = inner
                .test_runs
                .unclassified_commands
                .values()
                .filter(|pending| {
                    pending.origin.session_id == session_id
                        && pending.origin.runtime_identity == runtime
                        && pending.origin.runtime_identity.is_some()
                        && pending.origin.tool_identity == tool
                        && match provenance {
                            EngramObservationProvenance::Claude(origin) => match &origin.origin {
                                ClaudeWorkOrigin::Attempt { turn_generation } => {
                                    pending.origin.turn_generation == *turn_generation
                                }
                                ClaudeWorkOrigin::Unattributed => false,
                            },
                            EngramObservationProvenance::Ambient => true,
                        }
                });
            let Some(pending) = candidates.next().cloned() else {
                // Further ACP pending/in-progress frames may repeat a start
                // after its original placeholder has already been consumed.
                return inner.test_run_observations.values().any(|row| {
                    row.summary.owner_session_id.as_deref() == Some(session_id)
                        && row.runtime_identity == runtime
                        && row.runtime_identity.is_some()
                        && row.tool_identity == tool
                        && row.summary.ended_at.is_none()
                        && match provenance {
                            EngramObservationProvenance::Claude(origin) => match &origin.origin {
                                ClaudeWorkOrigin::Attempt { turn_generation } => {
                                    row.turn_generation == *turn_generation
                                }
                                ClaudeWorkOrigin::Unattributed => false,
                            },
                            EngramObservationProvenance::Ambient => true,
                        }
                });
            };
            if candidates.next().is_some() {
                // Ambiguous original starts cannot acquire the current owner.
                return true;
            }
            pending
        };
        let places = cwd
            .map(|directory| vec![Some(directory.to_owned())])
            .or(pending.directories.clone());
        let recognised = EngramHost::recognized_check(ran);
        let declared = recognised
            .is_none()
            .then(|| {
                engram_declared_start(
                    Some(ran),
                    FsPath::new(&pending.workdir),
                    places.as_deref(),
                    None,
                )
                .0
            })
            .flatten();
        let Some(command) = recognised
            .as_ref()
            .or_else(|| declared.as_ref().map(|(command, _, _)| command))
        else {
            return true;
        };
        if command.kind != EngramVerificationKind::Test {
            return true;
        }
        let worktree = engram_check_worktree_from(
            command,
            FsPath::new(&pending.workdir),
            places.as_deref(),
            None,
        )
        .map(|target| test_run_display_path(&target.root))
        .unwrap_or_default();
        let id = pending.origin.execution_id();
        self.observe_test_command_start(
            pending.origin,
            command,
            &worktree,
            &pending.started_at,
            declared.as_ref().map(|(_, _, binding)| binding),
        );
        self.inner
            .lock()
            .expect("state mutex poisoned")
            .test_runs
            .unclassified_commands
            .remove(&id);
        true
    }

    fn observe_test_command_start(
        &self,
        origin: TestRunCommandOrigin,
        command: &EngramCheckCommand,
        worktree: &str,
        started_at: &str,
        declared: Option<&EngramDeclaredBinding>,
    ) {
        if command.kind != EngramVerificationKind::Test {
            return;
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let TestRunCommandOrigin {
            session_id,
            project_id,
            runtime_identity,
            tool_identity,
            turn_generation,
            grant_identity,
            work_binding,
            source_root,
            validated_unbound,
        } = origin;
        let execution_id = format!(
            "observed-{}",
            sha256_hex(
                format!("{session_id}:{runtime_identity:?}:{turn_generation}:{tool_identity}")
                    .as_bytes()
            )
        );
        if inner.test_run_observations.contains_key(&execution_id)
            || inner.test_run_compacted.values().any(|row| {
                row.observations
                    .iter()
                    .any(|observation| observation.execution_id == execution_id)
            })
        {
            return;
        }
        let observation = TestRunCommandObservation {
            execution_id: execution_id.clone(),
            runtime_identity,
            tool_identity,
            grant_identity,
            command_fingerprint: EngramHost::check_fingerprint(command),
            turn_generation,
            observed_at: test_run_timestamp(),
            work_binding,
            source_root,
            validated_unbound,
            monitoring_interrupted_at: None,
            final_unbound_proof: None,
            evidence: Vec::new(),
            artifact: None,
            declared_artifact: declared
                .map(|binding| test_run_display_path(FsPath::new(&binding.artifact_canonical))),
            launch_only: EngramHost::is_launch_only(command, EngramCommandExit::Code(0)),
            summary: TestRunSummary {
                registration: TestRunRegistrationView::default(),
                run_id: execution_id.clone(),
                project_id,
                worktree: worktree.to_owned(),
                run_dir: String::new(),
                preset: if EngramHost::is_full_launcher(command) {
                    TestRunPreset::Full
                } else {
                    TestRunPreset::Focused
                },
                // Recognition supplies a bounded runner name; arbitrary
                // shell text or credentials are not copied into this row.
                command: Some(vec![command.program.clone()]),
                command_truncated: false,
                detached: None,
                state: TestRunState::Running,
                unknown_reason: None,
                interrupted: false,
                current_stage: None,
                stages: Vec::new(),
                owner_session_id: Some(session_id.to_owned()),
                notify_to: None,
                notify_session_id: None,
                started_at: Some(started_at.to_owned()),
                ended_at: None,
                exit_code: None,
                error: None,
                detail_version: None,
            },
        };
        inner
            .test_run_observations
            .insert(execution_id, observation.clone());
        self.persist_test_command_observation_locked(&mut inner, observation);
    }

    fn observe_test_command_end(
        &self,
        session_id: &str,
        provenance: &EngramObservationProvenance,
        key: &str,
        exit: Option<EngramCommandExit>,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let runtime_identity = test_run_runtime_identity(&inner.sessions[index], provenance);
        let tool_identity = sha256_hex(key.as_bytes());
        if exit != Some(EngramCommandExit::NotFinished) {
            inner.test_runs.unclassified_commands.retain(|_, pending| {
                !(pending.origin.session_id == session_id
                    && pending.origin.runtime_identity == runtime_identity
                    && pending.origin.tool_identity == tool_identity)
            });
        }
        let turn = match provenance {
            EngramObservationProvenance::Claude(origin) => match &origin.origin {
                ClaudeWorkOrigin::Attempt { turn_generation } => Some(*turn_generation),
                ClaudeWorkOrigin::Unattributed => None,
            },
            EngramObservationProvenance::Ambient => None,
        };
        let matches: Vec<String> = inner
            .test_run_observations
            .values()
            .filter(|row| {
                row.summary.owner_session_id.as_deref() == Some(session_id)
                    && row.runtime_identity == runtime_identity
                    && row.tool_identity == tool_identity
                    && turn.is_none_or(|turn| row.turn_generation == turn)
                    && row.summary.ended_at.is_none()
            })
            .map(|row| row.execution_id.clone())
            .collect();
        for id in &matches {
            let can_finalize = inner
                .test_run_observations
                .get(id)
                .is_some_and(|row| test_run_can_finalize_unbound(&inner, index, row));
            let Some(row) = inner.test_run_observations.get_mut(id) else {
                continue;
            };
            if exit == Some(EngramCommandExit::NotFinished) {
                if row.summary.preset == TestRunPreset::Full {
                    row.launch_only = true;
                    test_run_observation_launch_unconfirmed(row);
                    let row = row.clone();
                    self.persist_test_command_observation_locked(&mut inner, row);
                }
                continue;
            }
            if row.launch_only {
                // The launcher returning (even with exit 0) is not the run's
                // terminal outcome. Only its exact artifact may supply that.
                test_run_observation_launch_unconfirmed(row);
                let row = row.clone();
                self.persist_test_command_observation_locked(&mut inner, row);
                continue;
            }
            row.summary.ended_at = Some(test_run_timestamp());
            row.summary.state = if matches.len() != 1 {
                TestRunState::Unknown
            } else {
                match exit {
                    Some(EngramCommandExit::Code(0) | EngramCommandExit::ReportedSuccess) => {
                        TestRunState::Passed
                    }
                    Some(EngramCommandExit::Code(_)) => TestRunState::Failed,
                    _ => TestRunState::Unknown,
                }
            };
            row.summary.exit_code = match exit {
                Some(EngramCommandExit::Code(code)) => Some(code),
                _ => None,
            };
            if matches.len() == 1 && can_finalize {
                row.final_unbound_proof =
                    test_run_final_unbound_identity(row).map(|terminal_identity| {
                        TestRunFinalUnboundProof {
                            version: 1,
                            terminal_identity,
                        }
                    });
            }
            let observation = row.clone();
            self.persist_test_command_observation_locked(&mut inner, observation);
        }
    }

    fn persist_test_command_observation_locked(
        &self,
        inner: &mut StateInner,
        observation: TestRunCommandObservation,
    ) {
        let id = observation.execution_id.clone();
        inner.test_runs.observations_durable.remove(&id);
        inner.test_runs.observations_failed.remove(&id);
        let result = self.commit_persisted_delta_locked_with_dispatch(inner);
        // The commit allocates its revision before attempting persistence.
        // Even a failed write publishes the factual tracking failure at that
        // revision, after its disposition is known, without a second commit
        // or a global snapshot that would make the delta redundant.
        let revision = inner.revision;
        match result {
            Ok((_, PersistDispatch::Synchronous)) => {
                inner.test_runs.observations_durable.insert(id);
            }
            Ok((_, PersistDispatch::BackgroundQueued)) => {
                let (fence, waiter) = PersistFence::new(
                    PersistFenceTarget::TestRunObservation(Box::new(observation.clone())),
                    std::time::Instant::now() + Duration::from_secs(5),
                );
                if self
                    .persist_tx
                    .send(PersistRequest::Fence(Box::new(fence)))
                    .is_ok()
                {
                    inner
                        .test_runs
                        .observation_persistence
                        .0
                        .lock()
                        .expect("test observation mutex poisoned")
                        .insert(id, (observation.clone(), waiter));
                } else {
                    inner.test_runs.observations_failed.insert(id);
                }
            }
            Err(_) => {
                inner.test_runs.observations_failed.insert(id);
            }
        }
        // Publication remains available when durability fails: its explicit
        // failed tracking state warns the operator instead of hiding the run.
        if let Some(row) = inner.test_run_observations.get(&observation.execution_id) {
            let run = test_run_public_observation(inner, row);
            self.publish_delta_locked(inner, DeltaEvent::TestRunChanged { revision, run });
        }
        self.compact_test_runs_locked(inner);
    }

    // Receipt settlement and lifecycle reads can update many retained rows at
    // once. Persist their final images together and project the public index
    // once, rather than repeating both history scans after every association.
    // Other single-row observation writers retain their existing commit path.
    fn persist_test_run_evidence_batch_locked(
        &self,
        inner: &mut StateInner,
        observations: Vec<TestRunCommandObservation>,
        compacted: Vec<TestRunCompacted>,
    ) {
        if observations.is_empty() && compacted.is_empty() {
            return;
        }
        for row in &observations {
            inner.test_runs.observations_durable.remove(&row.execution_id);
            inner.test_runs.observations_failed.remove(&row.execution_id);
        }
        for row in &compacted {
            inner
                .test_runs
                .compactions_revalidating
                .insert(row.summary.run_id.clone());
        }
        // Live observation saves have a visible revision, including on write
        // failure. A compacted-only proof save remains private until its delta.
        let mut first_revision = None;
        let dispatch = if observations.is_empty() {
            self.persist_internal_locked_with_dispatch(inner).ok()
        } else {
            let result = self.commit_persisted_delta_locked_with_dispatch(inner);
            first_revision = Some(inner.revision);
            result.ok().map(|(_, dispatch)| dispatch)
        };
        let mut ids = Vec::with_capacity(observations.len());
        for row in observations {
            let id = row.execution_id.clone();
            ids.push(id.clone());
            match dispatch {
                Some(PersistDispatch::Synchronous) => {
                    inner.test_runs.observations_durable.insert(id);
                }
                Some(PersistDispatch::BackgroundQueued) => {
                    let (fence, waiter) = PersistFence::new(
                        PersistFenceTarget::TestRunObservation(Box::new(row.clone())),
                        std::time::Instant::now() + Duration::from_secs(5),
                    );
                    if self
                        .persist_tx
                        .send(PersistRequest::Fence(Box::new(fence)))
                        .is_ok()
                    {
                        inner
                            .test_runs
                            .observation_persistence
                            .0
                            .lock()
                            .expect("test observation mutex poisoned")
                            .insert(id, (row, waiter));
                    } else {
                        inner.test_runs.observations_failed.insert(id);
                    }
                }
                None => {
                    inner.test_runs.observations_failed.insert(id);
                }
            }
        }
        for row in &compacted {
            match dispatch {
                Some(PersistDispatch::Synchronous) => {
                    inner
                        .test_runs
                        .compactions_durable
                        .insert(row.summary.run_id.clone());
                    inner
                        .test_runs
                        .compactions_revalidating
                        .remove(&row.summary.run_id);
                }
                Some(PersistDispatch::BackgroundQueued) => {
                    self.queue_test_run_compaction_fence(inner, row.clone());
                }
                None => {}
            }
        }
        if !ids.is_empty() {
            let runs = test_run_summaries_locked(inner);
            // Preserve first-match selection for both execution and artifact
            // identities without another full index search per changed row.
            let mut by_id = HashMap::new();
            let mut by_artifact = HashMap::new();
            for run in &runs {
                by_id.entry(run.run_id.as_str()).or_insert(run);
                by_artifact
                    .entry((run.run_id.as_str(), test_run_path_key(&run.run_dir)))
                    .or_insert(run);
            }
            for id in ids {
                let Some(row) = inner.test_run_observations.get(&id) else {
                    continue;
                };
                let run = by_id
                    .get(id.as_str())
                    .copied()
                    .or_else(|| {
                        row.artifact.as_ref().and_then(|artifact| {
                            by_artifact
                                .get(&(
                                    artifact.run_id.as_str(),
                                    test_run_path_key(&artifact.run_directory),
                                ))
                                .copied()
                        })
                    })
                    .cloned()
                    .unwrap_or_else(|| test_run_observation_summary(inner, row));
                // Global deltas need distinct revisions: same-revision rows
                // are discarded by clients. Only the first emits a persist
                // dispatch; later revisions use the existing delta allocator,
                // including its post-shutdown synchronous durability fallback.
                let revision = match first_revision.take() {
                    Some(revision) => Some(revision),
                    None => self.commit_delta_locked(inner).ok(),
                };
                if let Some(revision) = revision {
                    self.publish_delta_locked(inner, DeltaEvent::TestRunChanged { revision, run });
                }
            }
        }
        for row in compacted {
            self.publish_test_run_compact_risk_locked(inner, &row);
        }
        self.compact_test_runs_locked(inner);
    }

    fn step_test_command_observation_persistence(&self) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let obsolete: Vec<_> = inner
            .test_runs
            .unclassified_commands
            .iter()
            .filter(|(_, pending)| {
                inner
                    .find_session_index(&pending.origin.session_id)
                    .and_then(|index| {
                        test_run_runtime_identity(
                            &inner.sessions[index],
                            &EngramObservationProvenance::Ambient,
                        )
                    })
                    != pending.origin.runtime_identity
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in obsolete {
            inner.test_runs.unclassified_commands.remove(&id);
        }
        // A launch acknowledgement may already be Unknown while its provider
        // is still monitored. Record exact runtime loss once, without claiming
        // that the workload ended or acquired a terminal result.
        let stale: Vec<_> = inner
            .test_run_observations
            .values()
            .filter(|row| {
                (row.summary.state == TestRunState::Running
                    || (row.launch_only && row.summary.state == TestRunState::Unknown))
                    && row.summary.ended_at.is_none()
                    && !row.summary.interrupted
                    && inner
                        .find_session_index(row.summary.owner_session_id.as_deref().unwrap_or(""))
                        .and_then(|index| {
                            test_run_runtime_identity(
                                &inner.sessions[index],
                                &EngramObservationProvenance::Ambient,
                            )
                        })
                        != row.runtime_identity
            })
            .map(|row| row.execution_id.clone())
            .collect();
        for id in stale {
            let Some(row) = inner.test_run_observations.get_mut(&id) else {
                continue;
            };
            row.summary.state = TestRunState::Unknown;
            row.summary.interrupted = true;
            row.monitoring_interrupted_at = Some(test_run_timestamp());
            row.summary.error = Some("Provider runtime ended or changed before reporting this execution's terminal status.".to_owned());
            let row = row.clone();
            self.persist_test_command_observation_locked(&mut inner, row);
        }
        // Repair persisted launch acknowledgements from older hosts without
        // requiring their still-live provider runtime to be replaced first.
        let unconfirmed: Vec<_> = inner
            .test_run_observations
            .values()
            .filter(|row| {
                row.launch_only
                    && row.summary.ended_at.is_none()
                    && row.summary.state == TestRunState::Running
            })
            .map(|row| row.execution_id.clone())
            .collect();
        for id in unconfirmed {
            let Some(row) = inner.test_run_observations.get_mut(&id) else {
                continue;
            };
            test_run_observation_launch_unconfirmed(row);
            let row = row.clone();
            self.persist_test_command_observation_locked(&mut inner, row);
        }
        let pending = inner.test_runs.observation_persistence.clone();
        let mut pending = pending.0.lock().expect("test observation mutex poisoned");
        let completed: Vec<_> = pending
            .iter()
            .filter_map(|(id, (expected, waiter))| {
                waiter
                    .wait_until(std::time::Instant::now())
                    .map(|result| (id.clone(), expected.clone(), result))
            })
            .collect();
        for (id, expected, result) in completed {
            pending.remove(&id);
            if inner.test_run_observations.get(&id) != Some(&expected) {
                continue;
            }
            if result.is_ok() {
                inner.test_runs.observations_durable.insert(id.clone());
            } else {
                inner.test_runs.observations_failed.insert(id.clone());
            }
            if let Some(row) = inner.test_run_observations.get(&id) {
                let run = test_run_public_observation(&inner, row);
                if let Ok(revision) = self.commit_delta_locked(&mut inner) {
                    self.publish_delta_locked(&inner, DeltaEvent::TestRunChanged { revision, run });
                }
            }
        }
    }
}

fn test_run_public_observation(
    inner: &StateInner,
    row: &TestRunCommandObservation,
) -> TestRunSummary {
    let runs = test_run_summaries_locked(inner);
    runs.iter()
        .find(|run| run.run_id == row.execution_id)
        .cloned()
        .or_else(|| {
            row.artifact.as_ref().and_then(|artifact| {
                runs.into_iter().find(|run| {
                    run.run_id == artifact.run_id
                        && test_run_path_key(&run.run_dir)
                            == test_run_path_key(&artifact.run_directory)
                })
            })
        })
        .unwrap_or_else(|| test_run_observation_summary(inner, row))
}

fn test_run_observation_risk(
    inner: &StateInner,
    row: &TestRunCommandObservation,
    durable: bool,
) -> Option<bool> {
    let risk = if !row.evidence.is_empty() {
        test_run_evidence_impact(inner, row)
    } else {
        (test_run_final_unbound_valid(row)
            || (row.summary.ended_at.is_none()
                && !row.launch_only
                && row.artifact.is_none()
                && row.validated_unbound
                && row.runtime_identity.is_some()
                && row.monitoring_interrupted_at.is_none()))
        .then_some(false)
    };
    if risk == Some(false) && !durable {
        None
    } else {
        risk
    }
}

fn test_run_observation_summary(
    inner: &StateInner,
    row: &TestRunCommandObservation,
) -> TestRunSummary {
    let mut summary = row.summary.clone();
    summary.registration.registration_state = Some("hostObserved".to_owned());
    summary.registration.tracking_provenances = vec!["hostObserved".to_owned()];
    summary.registration.start_time_origin = Some("hostCommandStart".to_owned());
    summary.registration.initiating_session_id = summary.owner_session_id.clone();
    summary.registration.bound_work_ids = row
        .work_binding
        .iter()
        .map(|binding| binding.work_id.clone())
        .collect();
    summary.registration.monitoring_interrupted_at = row.monitoring_interrupted_at.clone();
    summary.registration.observed_at = Some(row.observed_at.clone());
    summary.registration.artifact_locations = row.declared_artifact.iter().cloned().collect();
    summary.registration.verification_record_ids = row
        .evidence
        .iter()
        .filter_map(|association| association.verification_record_id.clone())
        .collect();
    summary.registration.source_fingerprints = row
        .evidence
        .iter()
        .map(|association| association.source_basis.source_revision.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    summary.registration.tracking_durability = Some(
        if inner
            .test_runs
            .observations_failed
            .contains(&row.execution_id)
        {
            "failed"
        } else if inner
            .test_runs
            .observations_durable
            .contains(&row.execution_id)
        {
            "durable"
        } else {
            "pending"
        }
        .to_owned(),
    );
    summary.registration.restart_would_void_evidence = test_run_observation_risk(
        inner,
        row,
        inner
            .test_runs
            .observations_durable
            .contains(&row.execution_id),
    );
    summary.registration.restart_evidence_reason =
        "Provider execution observed; exact verification settlement is not yet correlated."
            .to_owned();
    if row.evidence.is_empty() && summary.registration.restart_would_void_evidence == Some(false) {
        summary.registration.restart_evidence_reason = if test_run_final_unbound_valid(row) {
            "Final host proof confirms no work binding or outstanding verification report."
        } else {
            "Current host observation validates an execution without work tracking."
        }
        .to_owned();
    }
    if !row.evidence.is_empty() {
        summary.registration.bound_work_ids = row
            .evidence
            .iter()
            .map(|association| association.binding.work_id.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        summary.registration.restart_evidence_reason = match summary.registration.restart_would_void_evidence {
            Some(true) => "Exact test verification is pending settlement or its associated work lifecycle has not completed.",
            Some(false) => "Every exact verification association is terminal and each passing lifecycle has a validated canonical completion.",
            None => "Evidence correlation or its durability must be revalidated.",
        }.to_owned();
    }
    summary
}

// Whether command observation `row` is exactly associated with launcher run
// `run`: same owner and worktree, an artifact naming this run and its
// directory, and a positive terminal-digest or immutable-request match. Also
// used by the handoff ledger to find a run's credited observations.
fn test_run_observation_matches_run(
    inner: &StateInner,
    run: &TestRunSummary,
    disk: Option<&TestRunDisk>,
    row: &TestRunCommandObservation,
) -> bool {
    #[cfg(test)]
    TEST_TEST_RUN_ASSOCIATION_VISITS.with(|count| count.set(count.get() + 1));
    row.artifact.as_ref().is_some_and(|artifact| {
        run.owner_session_id == row.summary.owner_session_id
            && test_run_path_key(&run.worktree) == test_run_path_key(&row.summary.worktree)
            && artifact.run_id == run.run_id
            && test_run_path_key(&artifact.run_directory) == test_run_path_key(&run.run_dir)
            && if let Some(digest) = &artifact.terminal_digest {
                test_run_terminal_digest_matches(run.detail_version.as_deref(), digest)
            } else {
                artifact.request_identity.as_ref().is_some_and(|identity| {
                    disk.is_some_and(|disk| disk.request_identity.as_ref() == Some(identity))
                        || (disk.is_none()
                            && inner.test_run_compacted.get(&run.run_id).is_some_and(
                                |retained| retained.request_identity.as_ref() == Some(identity),
                            ))
                })
            }
    })
}

// Shared by the scanner's SSE summary and fresh API/snapshot projection.
// Only positive immutable-request or terminal-digest matches suppress an
// observation row. All inputs here are already read; no filesystem I/O.
fn test_run_enrich_artifact_summary(
    inner: &StateInner,
    run: &mut TestRunSummary,
    disk: Option<&TestRunDisk>,
) -> HashSet<String> {
    test_run_enrich_artifact_summary_from_observations(
        inner, run, disk, inner.test_run_observations.values(),
    )
}

fn test_run_enrich_artifact_summary_from_observations<'a>(
    inner: &StateInner,
    run: &mut TestRunSummary,
    disk: Option<&TestRunDisk>,
    observations: impl IntoIterator<Item = &'a TestRunCommandObservation>,
) -> HashSet<String> {
    let mut merged = HashSet::new();
    let associations: Vec<_> = observations
        .into_iter()
        .filter(|row| test_run_observation_matches_run(inner, run, disk, row))
        .collect();
    if associations.is_empty() {
        return merged;
    }
    // A discovery-only artifact is superseded by exact report proof. A
    // registered launch additionally requires coverage of its captured
    // binding; another lifecycle cannot discharge that association.
    let base_covered = inner
        .test_run_registrations
        .get(&run.run_id)
        .is_none_or(|registration| {
            registration.validated_unbound
                && registration.work_binding.is_none()
                && (registration.monitoring_interrupted_at.is_none()
                    || associations
                        .iter()
                        .any(|row| test_run_final_unbound_valid(row)))
                || registration
                    .authority_store
                    .as_ref()
                    .zip(registration.work_binding.as_ref())
                    .is_some_and(|(store, binding)| {
                        associations
                            .iter()
                            .flat_map(|row| &row.evidence)
                            .any(|association| {
                                &association.store == store
                                    && association.binding.root_execution_id
                                        == binding.root_execution_id
                                    && association.binding.work_id == binding.work_id
                                    && association.binding.run_id == binding.run_id
                                    && association.binding.claim_id == binding.claim_id
                            })
                    })
        });
    let mut positive = run.registration.restart_would_void_evidence == Some(true);
    let mut unknown = !base_covered;
    for row in associations {
        merged.insert(row.execution_id.clone());
        let observed = test_run_observation_summary(inner, row);
        positive |= observed.registration.restart_would_void_evidence == Some(true);
        unknown |= observed.registration.restart_would_void_evidence.is_none();
        run.registration
            .tracking_provenances
            .extend(observed.registration.tracking_provenances);
        run.registration
            .artifact_locations
            .extend(observed.registration.artifact_locations);
        if run.registration.observed_at.is_none() {
            run.registration.observed_at = observed.registration.observed_at;
        }
        run.registration
            .bound_work_ids
            .extend(observed.registration.bound_work_ids);
        run.registration
            .verification_record_ids
            .extend(observed.registration.verification_record_ids);
        run.registration
            .source_fingerprints
            .extend(observed.registration.source_fingerprints);
    }
    run.registration.bound_work_ids.sort();
    run.registration.bound_work_ids.dedup();
    run.registration.tracking_provenances.sort();
    run.registration.tracking_provenances.dedup();
    run.registration.artifact_locations.sort();
    run.registration.artifact_locations.dedup();
    run.registration.verification_record_ids.sort();
    run.registration.verification_record_ids.dedup();
    run.registration.source_fingerprints.sort();
    run.registration.source_fingerprints.dedup();
    run.registration.superseded_run_ids = merged.iter().cloned().collect();
    run.registration.superseded_run_ids.sort();
    run.registration.restart_would_void_evidence = if positive {
        Some(true)
    } else if unknown {
        None
    } else {
        Some(false)
    };
    run.registration.restart_evidence_reason = match run.registration.restart_would_void_evidence {
            Some(true) => "At least one exact run/check association is still awaiting settlement or lifecycle completion.",
            Some(false) => "All exact run/check associations are settled and each passing work lifecycle is completed.",
            None => "At least one run/check association remains unknown or requires revalidation.",
        }.to_owned();
    merged
}

fn test_run_artifact_fallback(inner: &StateInner, run_id: &str) -> Option<TestRunSummary> {
    test_run_artifact_fallback_from_observations(inner, run_id, inner.test_run_observations.values())
}

fn test_run_artifact_fallback_from_observations<'a>(
    inner: &StateInner,
    run_id: &str,
    observations: impl IntoIterator<Item = &'a TestRunCommandObservation>,
) -> Option<TestRunSummary> {
    let run = if let Some(row) = inner.test_run_registrations.get(run_id) {
        test_run_registered_fallback_from_observations(inner, row, observations)
    } else {
        let row = inner.test_run_compacted.get(run_id)?;
        if (row.compacted && test_run_compact_is_installed(inner, row))
            || inner.test_run_observations.contains_key(run_id)
        {
            return None;
        }
        if row.compacted {
            row.summary.clone()
        } else {
            test_run_compacted_summary(inner, row)
        }
    };
    Some(test_run_unavailable_artifact(run))
}

// The same selection as the full history projection for one compaction
// identity. Completing one fence must not rebuild every unrelated summary.
// Candidate filtering is not proof: exact matching and risk are still fresh.
fn test_run_compaction_summary(inner: &StateInner, run_id: &str) -> Option<TestRunSummary> {
    let observations = || inner.test_run_observations.values().filter(|row| {
        row.artifact.as_ref().is_some_and(|artifact| artifact.run_id == run_id)
    });
    let artifact = if let Some(entry) = inner.test_runs.find(run_id) {
        let mut run = entry.summary.clone();
        run.registration = test_run_registration_view_from_observations(
            inner, &entry.disk, observations(),
        );
        run.project_id = test_run_effective_project_id(inner, entry);
        test_run_enrich_artifact_summary_from_observations(
            inner, &mut run, Some(&entry.disk), observations(),
        );
        Some(run)
    } else {
        test_run_artifact_fallback_from_observations(inner, run_id, observations())
            .map(|mut run| {
                test_run_enrich_artifact_summary_from_observations(inner, &mut run, None, observations());
                run
            })
    };
    if artifact.is_some() {
        return artifact;
    }
    let row = inner.test_run_observations.get(run_id)?;
    if let Some(artifact) = &row.artifact {
        // An observation merged into an artifact is absent from full summaries.
        // Artifact ids are distinct from execution ids; do not recurse through
        // an arbitrary user-supplied/corrupt observation association chain.
        let indexed = inner.test_runs.find(&artifact.run_id);
        let target = indexed.map(|entry| entry.summary.clone()).or_else(|| {
            test_run_artifact_fallback(inner, &artifact.run_id)
        });
        if target.as_ref().is_some_and(|run| {
            test_run_observation_matches_run(inner, run, indexed.map(|entry| entry.disk.as_ref()), row)
        }) {
            return None;
        }
    }
    Some(test_run_observation_summary(inner, row))
}

fn test_run_full_summaries_locked(inner: &StateInner) -> Vec<TestRunSummary> {
    let projection = TestRunProjection::new(inner);
    let mut runs = inner.test_runs.summaries();
    // Cached summaries cannot serve an old 'safe' value after lifecycle changes.
    for run in &mut runs {
        if let Some(entry) = projection.entries.get(run.run_id.as_str()) {
            run.registration = test_run_registration_view_from_observations(
                inner, &entry.disk, projection.observations(&run.run_id).iter().copied(),
            );
            run.project_id = test_run_effective_project_id(inner, entry);
        }
    }
    let retained: std::collections::BTreeSet<_> = inner
        .test_run_registrations
        .keys()
        .chain(inner.test_run_compacted.keys())
        .collect();
    runs.extend(retained.into_iter().filter_map(|id| {
        if !projection.entries.contains_key(id.as_str()) {
            test_run_artifact_fallback_from_observations(
                inner, id, projection.observations(id).iter().copied(),
            )
        } else {
            None
        }
    }));
    let mut merged = HashSet::new();
    for run in &mut runs {
        let disk = projection.entries
            .get(run.run_id.as_str())
            .map(|entry| entry.disk.as_ref());
        let observations = projection.observations(&run.run_id);
        merged.extend(test_run_enrich_artifact_summary_from_observations(
            inner, run, disk, observations.iter().copied(),
        ));
    }
    runs.extend(
        inner
            .test_run_observations
            .values()
            .filter(|row| !merged.contains(&row.execution_id))
            .map(|row| test_run_observation_summary(inner, row)),
    );
    runs.sort_by(|left, right| {
        right
            .started_at
            .cmp(&left.started_at)
            .then_with(|| left.run_id.cmp(&right.run_id))
    });
    runs
}

fn test_run_summaries_locked(inner: &StateInner) -> Vec<TestRunSummary> {
    let mut runs = test_run_full_summaries_locked(inner);
    let known: HashSet<_> = runs.iter().map(|run| run.run_id.clone()).collect();
    runs.extend(
        inner
            .test_run_compacted
            .values()
            .filter(|row| {
                test_run_compact_is_installed(inner, row) && !known.contains(&row.summary.run_id)
            })
            .map(|row| test_run_compacted_summary(inner, row))
            .filter(|run| run.registration.restart_would_void_evidence != Some(false)),
    );
    runs.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| a.run_id.cmp(&b.run_id))
    });
    runs
}
