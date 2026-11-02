// Observation-only facts for an ordinary or adopted runtime-started turn. Owns its
// stable identity, unbounded observed-check collection and immutable request
// preparation. Does not admit a turn, operate on grants or send a request.
// New fragment beside engram_unadmitted_observation.rs.

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramRuntimeObservedCommand {
    tool_key: String,
    simple: bool,
    // Keep the start's one-call and launcher interpretation through reload;
    // a completion label or exit alone cannot prove which command ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recognized_command: Option<EngramCheckCommand>,
    #[serde(default)]
    launch_only: bool,
    // A launcher invocation can end without proving that its background
    // test finished. Keep these two observed facts distinct.
    #[serde(default)]
    invocation_ended: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    declared: Option<EngramDeclaredBinding>,
    check: EngramObservedCheck,
}

// Build and qualified launcher completions require their existing owner;
// a mismatched command or runner-looking stdout cannot supply the outcome.
fn runtime_observed_command_needs_completion(command: &EngramCheckCommand) -> bool {
    command.argv.is_empty()
        || command.kind == EngramVerificationKind::Build
        || engram_is_focused_node_check(command)
        || (command.program == "node" && command.literal_pipe)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct EngramRuntimeObservedTurn {
    host_turn_ref: String,
    adopted_at: String,
    notice_message_id: String,
    generation: u64,
    commands: Vec<EngramRuntimeObservedCommand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observed_outcome: Option<EngramObservedTurnOutcome>,
    #[serde(default)]
    source_attribution_uncertain: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    opening_root: Option<EngramRuntimeOpeningRoot>,
    #[serde(skip)]
    opening_prepared: bool,
}

// Equality covers every retained observation fact, not process-local
// preparation progress that is deliberately absent from the stored image.
impl PartialEq for EngramRuntimeObservedTurn {
    fn eq(&self, other: &Self) -> bool {
        self.host_turn_ref == other.host_turn_ref
            && self.adopted_at == other.adopted_at
            && self.notice_message_id == other.notice_message_id
            && self.generation == other.generation
            && self.commands == other.commands
            && self.observed_outcome == other.observed_outcome
            && self.source_attribution_uncertain == other.source_attribution_uncertain
            && self.opening_root == other.opening_root
    }
}

impl Eq for EngramRuntimeObservedTurn {}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramRuntimeOpeningRoot {
    store: EngramAuthorityStoreKey,
    binding: EngramControlWorkBinding,
    provenance: EngramRootCapture,
    source_root: Option<EngramTurnSourceRoot>,
}

impl EngramRuntimeObservedTurn {
    fn adopted(notice_message_id: String, generation: u64) -> Self {
        Self::started(notice_message_id, generation)
    }

    // The existing anchor is an adoption notice for a runtime-started turn,
    // or the actual prompt message for an ordinary host-started turn.
    fn started(notice_message_id: String, generation: u64) -> Self {
        Self {
            host_turn_ref: format!("runtime-turn-{}", Uuid::new_v4()),
            adopted_at: unadmitted_observation_timestamp(),
            notice_message_id,
            generation,
            commands: Vec::new(),
            observed_outcome: None,
            source_attribution_uncertain: false,
            opening_root: None,
            opening_prepared: false,
        }
    }

    // This collection is independent of the local diagnostic ring. Neither
    // its number of commands nor the producer's inline-check limit truncates
    // it. Each command gets one immutable identity, including after reload.
    fn observe(&mut self, observation: &EngramRecorderObservation<'_>) {
        self.observe_with_facts(observation, None, None, None,
            matches!(observation, EngramRecorderObservation::CommandStarted { .. }));
    }

    fn observe_recognized(
        &mut self,
        observation: &EngramRecorderObservation<'_>,
        recognized: Option<&EngramCheckCommand>,
        declared: Option<&EngramDeclaredBinding>,
        fresh_start: bool,
    ) {
        self.observe_with_facts(observation, recognized, declared, None, fresh_start);
    }

    fn observe_with_facts(
        &mut self,
        observation: &EngramRecorderObservation<'_>,
        recognized: Option<&EngramCheckCommand>,
        declared: Option<&EngramDeclaredBinding>,
        artifact_outcome: Option<EngramExecutionOutcome>,
        fresh_start: bool,
    ) {
        let now = unadmitted_observation_timestamp();
        match observation {
            EngramRecorderObservation::CommandStarted { key, ran, .. }
            | EngramRecorderObservation::CommandDescribed { key, ran, .. } => {
                let Some(command) = recognized.cloned().or_else(|| ran.and_then(engram_check_command)) else {
                    return;
                };
                if let Some(previous) = self.commands.iter().rev().find(|entry| entry.tool_key == *key) {
                    // The existing recorder owns fresh starts. A repeated
                    // update cannot reopen a completed invocation or replace
                    // one that still awaits its terminal result.
                    if (!previous.invocation_ended && previous.check.finished_at.is_none()) || !fresh_start
                        || !matches!(observation, EngramRecorderObservation::CommandStarted { .. })
                    {
                        return;
                    }
                }
                let occurrence = self.commands.iter().filter(|entry| entry.tool_key == *key).count() as u64;
                self.commands.push(EngramRuntimeObservedCommand {
                    tool_key: (*key).to_owned(),
                    simple: command.simple,
                    recognized_command: Some(command.clone()),
                    // Reuse the launch owner's interpretation. A successful
                    // shell invocation can be only STARTED, not a test end.
                    launch_only: !matches!(
                        engram_launch_disposition(&command, EngramCommandExit::ReportedSuccess),
                        EngramLaunchDisposition::Ordinary
                    ),
                    invocation_ended: false,
                    declared: declared.cloned(),
                    check: EngramObservedCheck {
                        host_check_id: unadmitted_host_check_occurrence_id(&self.host_turn_ref, key, occurrence),
                        check_kind: command.kind,
                        observed_result: EngramObservedCheckResult::Indeterminate,
                        started_at: (fresh_start && matches!(
                            observation,
                            EngramRecorderObservation::CommandStarted { .. }
                        ))
                        .then(|| now.clone()),
                        finished_at: None,
                        observed_at: now,
                        source_basis: None,
                        host_evidence_ref: None,
                    },
                });
            }
            EngramRecorderObservation::CommandFinished {
                key,
                command,
                reported_command,
                output,
                exit,
                runtime_output_cut,
                ..
            } => {
                if !self.commands.iter().any(|entry| entry.tool_key == *key) {
                    self.observe(&EngramRecorderObservation::CommandDescribed {
                        key,
                        ran: Some(command),
                        cwd: None,
                    });
                }
                let Some(entry) = self
                    .commands
                    .iter_mut()
                    .rev()
                    .find(|entry| entry.tool_key == *key)
                else {
                    return;
                };
                if entry.invocation_ended || entry.check.finished_at.is_some() {
                    return;
                }
                // An explicit later command must name the start's exact
                // interpretation. An absent actual command is distinct from
                // the provider's display title and keeps the original owner.
                let changed_command = entry.declared.is_none()
                    && reported_command.is_some_and(|ran| {
                        entry.recognized_command.as_ref().is_none_or(|started|
                            engram_check_command_like(ran, started).as_ref() != Some(started))
                    });
                let outcome = if *runtime_output_cut || entry.launch_only || changed_command {
                    Some(EngramExecutionOutcome::Unknown)
                } else if entry.declared.is_some() {
                    if entry.declared.as_ref() == declared {
                        artifact_outcome.or(Some(EngramExecutionOutcome::Unknown))
                    } else {
                        Some(EngramExecutionOutcome::Unknown)
                    }
                } else if entry.recognized_command.as_ref()
                    .is_some_and(runtime_observed_command_needs_completion)
                {
                    artifact_outcome.or(Some(EngramExecutionOutcome::Unknown))
                } else {
                    entry.recognized_command.as_ref().and_then(|command| {
                        let lines = engram_check_result_lines(&command.program, output);
                        engram_check_command_outcome(
                            command, exit.unwrap_or(EngramCommandExit::Unknown), &lines,
                        )
                    })
                };
                entry.check.observed_result = match outcome {
                    Some(EngramExecutionOutcome::Succeeded) => EngramObservedCheckResult::Passed,
                    Some(EngramExecutionOutcome::Failed) => EngramObservedCheckResult::Failed,
                    Some(EngramExecutionOutcome::Unknown) | None => {
                        EngramObservedCheckResult::Indeterminate
                    }
                };
                // A background-launch result is not a terminal process result.
                if !matches!(exit, Some(EngramCommandExit::NotFinished)) {
                    entry.invocation_ended = true;
                    if !entry.launch_only {
                        entry.check.finished_at = Some(now.clone());
                    }
                }
                entry.check.observed_at = now;
            }
            EngramRecorderObservation::CommandAbandoned { key } => {
                // The recorder emits this for a proved denied/declined call,
                // not merely for a missing end. Launch classification does
                // not turn a proved denial into an observed invocation.
                if let Some(position) = self.commands.iter().rposition(|entry|
                    entry.tool_key == *key && !entry.invocation_ended && entry.check.finished_at.is_none())
                {
                    self.commands.remove(position);
                }
            }
            EngramRecorderObservation::WorkspaceEdit { .. } => {}
        }
    }
}

fn unadmitted_observation_timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn unadmitted_host_check_id(turn_ref: &str, tool_key: &str) -> String {
    let mut hash = Sha256::new();
    hash.update((turn_ref.len() as u64).to_be_bytes());
    hash.update(turn_ref.as_bytes());
    hash.update(tool_key.as_bytes());
    // The 128-bit digest identifies the tool occurrence within this UUID
    // turn; both identities also fit together in a standalone request key.
    let digest = format!("{:x}", hash.finalize());
    format!("check-{}", &digest[..32])
}

fn unadmitted_host_check_occurrence_id(turn_ref: &str, tool_key: &str, occurrence: u64) -> String {
    let first = unadmitted_host_check_id(turn_ref, tool_key);
    if occurrence == 0 {
        return first;
    }
    let mut hash = Sha256::new();
    hash.update(b"termal-observed-check-occurrence-v1\0");
    hash.update(first.as_bytes());
    hash.update(occurrence.to_be_bytes());
    let digest = format!("{:x}", hash.finalize());
    format!("check-{}", &digest[..32])
}

// A measured checkpoint's exact binding accompanies its original endpoint.
// A missing or incompatible baseline remains unknown; it is never invented
// from the current source or re-timestamped to fit the producer window.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramRuntimeCheckpointBasis {
    binding: EngramControlWorkBinding,
    endpoint: EngramMeasuredBaseline,
}

// Closing facts wait durably for off-thread measurement/preparation. The
// original checkpoint is captured before a successor turn can replace it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramRuntimeClosingJob {
    trace: EngramRuntimeObservedTurn,
    baseline: Option<EngramRuntimeCheckpointBasis>,
    scope: EngramRuntimeClosingScope,
    policy: Option<EngramObservationPolicyBasis>,
    attempts: u32,
    retry_at: String,
    last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal_failure: Option<String>,
}

impl EngramRuntimeClosingJob {
    fn new(
        trace: EngramRuntimeObservedTurn,
        baseline: Option<EngramRuntimeCheckpointBasis>,
        scope: EngramRuntimeClosingScope,
    ) -> Self {
        Self {
            trace,
            baseline,
            scope,
            policy: None,
            attempts: 0,
            retry_at: unadmitted_observation_timestamp(),
            last_error: None,
            terminal_failure: None,
        }
    }

    fn failed(&self, session_id: &str, error: &EngramTransportError) -> Self {
        let mut next = self.clone();
        next.attempts = next.attempts.saturating_add(1);
        next.retry_at = (chrono::Utc::now() + runtime_observation_retry_delay(session_id, next.attempts))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        next.last_error = Some(error.message.clone());
        next
    }
}

// Immutable work association captured under the terminal transition lock.
// The reporting policy is pinned separately, off-thread, before measurement.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramRuntimeClosingScope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    binding: Option<EngramControlWorkBinding>,
    store: EngramAuthorityStoreKey,
    connection: EngramConnectionConfig,
    // Preserve older closing images as data without treating their fields as
    // work-session credentials or current association requirements.
    #[serde(default, flatten)]
    historical_fields: BTreeMap<String, Value>,
    root: EngramNamedRootState,
    workdir: String,
}

impl EngramRuntimeClosingScope {
    fn capture(inner: &StateInner, session_id: &str) -> Result<Self, String> {
        Self::capture_with_binding(inner, session_id, None)
    }

    fn capture_with_binding(
        inner: &StateInner,
        session_id: &str,
        binding: Option<&EngramControlWorkBinding>,
    ) -> Result<Self, String> {
        let target =
            AppState::engram_source_root_target_locked(inner, session_id)?
                .ok_or_else(|| "no configured Engram connection".to_owned())?;
        let binding = binding.cloned().or(target.work_binding);
        let store = target
            .settings
            .authority_store_key
            .ok_or_else(|| "no verified store".to_owned())?;
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| "session disappeared".to_owned())?;
        let record = &inner.sessions[index];
        let root = if let Some(entry) = binding.as_ref().and_then(|binding| engram_work_source_root_for_claim(
            &inner.engram_work_source_roots,
            &store,
            &binding.work_id,
            &binding.claim_id,
        )) {
            EngramNamedRootState::Bound {
                workspace_id: entry.root.clone(),
                generation: i64::try_from(entry.generation)
                    .map_err(|_| "named root generation is out of range".to_owned())?,
                named_at: entry.named_at.clone(),
            }
        } else {
            record
                .engram
                .named_root
                .clone()
                .unwrap_or(EngramNamedRootState::None)
        };
        Ok(Self {
            binding,
            store,
            connection: target.connection,
            historical_fields: BTreeMap::new(),
            root,
            workdir: record.session.workdir.clone(),
        })
    }

    fn label(&self) -> String {
        format!(
            "binding={} root={:?} workdir={}",
            describe_engram_work_binding(self.binding.as_ref()),
            self.root,
            self.workdir
        )
    }

    fn require_current(
        &self,
        inner: &StateInner,
        session_id: &str,
    ) -> Result<(), EngramTransportError> {
        let restored_binding = inner.find_session_index(session_id)
            .filter(|&index| inner.sessions[index].engram.work_binding.is_none())
            .and(self.binding.as_ref());
        let current = Self::capture_with_binding(inner, session_id, restored_binding).map(|mut current| {
            if self.binding.is_none() {
                // No claim has been attributed yet. Only connection, store
                // and workdir are pinned until the worker retains a canonical
                // work association and its local root image.
                current.binding = None;
                current.root = self.root.clone();
            }
            current.historical_fields = self.historical_fields.clone();
            current
        });
        if current.as_ref() == Ok(self) {
            return Ok(());
        }
        let current = current
            .map(|scope| scope.label())
            .unwrap_or_else(|reason| reason);
        let mut error = EngramTransportError::local_state(format!(
            "not reported: scope changed; pinned [{}], current [{}] (binding, root, store or connection differs)",
            self.label(),
            current
        ));
        error.code = Some("runtime_observation_scope_changed".to_owned());
        Err(error)
    }
}

enum EngramRuntimeClosingOutcome {
    BindingRecoveryPending,
    Unclaimed,
    Binding(EngramRuntimeClosingScope),
    Policy(EngramObservationPolicyBasis),
    Report(EngramRuntimeObservationReport),
}

#[derive(Clone, Debug)]
struct EngramRuntimeClosingCapture {
    binding: EngramControlWorkBinding,
    root_basis: EngramObservationRootBasis,
    sighting: EngramMeasuredSighting,
    baseline: Option<EngramRuntimeCheckpointBasis>,
    policy: EngramObservationPolicyBasis,
}

impl EngramRuntimeClosingCapture {
    fn source_change(&self) -> Option<EngramObservedSourceChange> {
        let compatible = self.baseline.as_ref().filter(|basis| {
            basis.binding == self.binding
                && basis.endpoint.workspace_id == self.sighting.source_basis.workspace_id
        });
        match compatible {
            Some(basis)
                if basis.endpoint.source_revision == self.sighting.source_basis.source_revision =>
            {
                None
            }
            Some(basis) => Some(EngramObservedSourceChange::ContentComparison {
                workspace_id: self.sighting.source_basis.workspace_id.clone(),
                baseline: basis.endpoint.clone(),
                sighting: self.sighting.clone(),
            }),
            None => Some(EngramObservedSourceChange::AssumedMissingBaseline {
                workspace_id: self.sighting.source_basis.workspace_id.clone(),
                sighting: self.sighting.clone(),
            }),
        }
    }
}

// Request construction is separate from delivery. Once persisted, each
// serialized request is replayed exactly; a later policy, root, binding or
// source read never rewrites an uncertain earlier request.
fn prepare_runtime_observations(
    trace: &EngramRuntimeObservedTurn,
    capture: EngramRuntimeClosingCapture,
    built_at: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<Value>, EngramTransportError> {
    let source_change = (!trace.source_attribution_uncertain)
        .then(|| capture.source_change())
        .flatten();
    let adoption = engram_observation_instant(&trace.adopted_at, "actual adoption")
        .map_err(EngramTransportError::protocol)?;
    let mut from = adoption;
    if let Some(EngramObservedSourceChange::ContentComparison { baseline, .. }) = &source_change {
        let earlier =
            engram_observation_instant(&baseline.observed_at, "original checkpoint baseline")
                .map_err(EngramTransportError::protocol)?;
        from = from.min(earlier);
    }
    let through = engram_observation_instant(&capture.sighting.observed_at, "closing observation")
        .map_err(EngramTransportError::protocol)?;
    if through < adoption || through > built_at {
        return Err(EngramTransportError::protocol(
            "runtime observation has an invalid closing cut",
        ));
    }
    let basis = |key: String| EngramUnadmittedObservationBasis {
        idempotency_key: key,
        binding: capture.binding.clone(),
        root_basis: capture.root_basis.clone(),
        observed_interval: EngramObservationInterval {
            from: from.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            through: through.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        },
        policy_basis: capture.policy.clone(),
    };
    let checks = trace
        .commands
        .iter()
        .map(|entry| entry.check.clone())
        .collect::<Vec<_>>();
    let inline = checks.len().min(ENGRAM_OBSERVATION_MAX_CHECKS);
    let mut first = engram_unadmitted_turn_request(
        basis(format!("termal-unadmitted:{}", trace.host_turn_ref)),
        trace.host_turn_ref.clone(),
        source_change,
        checks[..inline].to_vec(),
        built_at,
    )?;
    if let EngramControlRequest::ExecutionObserve { observation, .. } = &mut first
        && let EngramInterTurnOccurrence::UnadmittedTurn { observed_outcome, .. } = &mut observation.occurrence
    {
        *observed_outcome = trace.observed_outcome;
    }
    let mut requests = vec![serde_json::to_value(first).map_err(|error| {
        EngramTransportError::protocol(format!("could not serialize runtime observation: {error}"))
    })?];
    for check in &checks[inline..] {
        let key = format!(
            "termal-observed:{}:{}",
            trace.host_turn_ref, check.host_check_id
        );
        let request = engram_observed_check_request(
            basis(key),
            trace.host_turn_ref.clone(),
            check.clone(),
            built_at,
        )?;
        requests.push(serde_json::to_value(request).map_err(|error| {
            EngramTransportError::protocol(format!("could not serialize observed check: {error}"))
        })?);
    }
    if !trace.source_attribution_uncertain {
        let request = engram_source_sighting_request(
            basis(format!("termal-source-sighting:{}", trace.host_turn_ref)),
            capture.sighting.clone(), built_at,
        )?;
        requests.push(serde_json::to_value(request).map_err(|error| {
            EngramTransportError::protocol(format!("could not serialize measured source sighting: {error}"))
        })?);
    }
    Ok(requests)
}
