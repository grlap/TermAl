// Shared durable source measurements and observation intents. Measurements
// record what the host actually saw; a remote receipt never invents one.
// Admission and retry retain ownership of the prompt and begun grant.

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramSourceSightingScope {
    store: EngramAuthorityStoreKey,
    project_id: String,
    root_execution_id: String,
    work_id: String,
    run_id: String,
    claim_id: String,
    workspace_id: String,
    source_root_generation: Option<i64>,
    source_root_state: Option<EngramSourceRootState>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramSourceSighting {
    basis: EngramExecutionSourceBasis,
    observed_at: String,
}

// The observation's delivery barrier and original request share the same
// persisted owner. Releasing a continuation leaves its facts and request.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
enum EngramSourceObservationPhase {
    Captured,
    Invariant { reason: String },
    Prepared { request: Value },
    Recorded { request: Value, receipt: Value },
    RefusedPolicy { request: Value, reason: String },
}

#[derive(Debug)]
enum EngramSourceObservationCapture {
    NoEvidence,
    Gate,
    RetiredRecovery { reason: String },
    Superseded,
    SupersededNoObservation,
    SupersededRecovery { scope: EngramSourceSightingScope, observation_id: String },
    Invariant { reason: String },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramSourceObservationIntent {
    id: String,
    session_id: String,
    prompt_id: String,
    dispatch_generation: u64,
    active_turn_generation: u64,
    grant_id: String,
    binding: EngramControlWorkBinding,
    connection: EngramConnectionConfig,
    #[serde(default = "engram_observation_default_timeout")]
    call_timeout_ms: u64,
    #[serde(default)]
    routing_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observing_session: Option<String>,
    baseline: Option<EngramSourceSighting>,
    sighting: EngramSourceSighting,
    // The opening measured after an owed close this intent reports: once
    // this one is recorded, the change from the close to it is reported next.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    follow_up: Option<EngramSourceSighting>,
    root_basis: Value,
    phase: EngramSourceObservationPhase,
    continuation_released: bool,
    // Set only after the coupled owner/session finalization fence succeeds.
    // Compaction is optional once this responsibility has been proved.
    #[serde(default)]
    finalization_complete: bool,
    #[serde(default)]
    delivery_retired: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    grant_settlement: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    grant_settlement_request: Option<Value>,
}

fn source_observation_settlement_request(intent: &EngramSourceObservationIntent) -> Result<EngramControlRequest, EngramTransportError> {
    let expected = EngramControlRequest::TurnCheckpoint {
        routing_token: intent.routing_token.clone(), grant_id: intent.grant_id.clone(),
        next_intent: EngramNextIntent::Exit, report: EngramTurnReport::default(),
        idempotency_key: engram_checkpoint_idempotency_key(
            format!("termal-observation-retirement:{}:{}", intent.session_id, intent.grant_id),
            &EngramTurnReport::default()),
    };
    if intent.grant_settlement_request.as_ref().is_some_and(|saved|
        saved != &serde_json::to_value(&expected).expect("settlement request serializes")) {
        return Err(EngramTransportError::protocol("retained source settlement contradicts its captured grant"));
    }
    Ok(expected)
}

fn engram_observation_default_timeout() -> u64 { ENGRAM_DEFAULT_CALL_TIMEOUT_MS }

fn retained_source_observation_request(
    intent: &EngramSourceObservationIntent, value: &Value,
) -> Result<EngramControlRequest, EngramTransportError> {
    let request: EngramControlRequest = serde_json::from_value(value.clone())
        .map_err(|_| EngramTransportError::protocol("retained source observation request is invalid"))?;
    let EngramControlRequest::ExecutionObserve { routing_token, observation } = &request else {
        return Err(EngramTransportError::protocol("retained source observation has another operation"));
    };
    let expected = EngramInterTurnObservation::from_intent(intent, observation.policy_basis.clone())?;
    if routing_token != &intent.routing_token || observation != &expected {
        return Err(EngramTransportError::protocol("retained source observation request contradicts its original facts"));
    }
    Ok(request)
}

fn validate_retained_source_observation_receipt(
    intent: &EngramSourceObservationIntent, request: &Value, receipt: &Value,
) -> Result<EngramSourceObservationReceipt, EngramTransportError> {
    let request = retained_source_observation_request(intent, request)?;
    let EngramControlRequest::ExecutionObserve { observation, .. } = &request else { unreachable!(); };
    let observer = intent.observing_session.as_deref().ok_or_else(||
        EngramTransportError::protocol("retained source observation has no observing session"))?;
    EngramSourceObservationReceipt::from_recorded_result(receipt.clone(), observation, observer)
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramSourceSightingOwner {
    scope: EngramSourceSightingScope,
    // Every change advances this fence. An old receipt cannot acknowledge a
    // newer fact or unblock its provider continuation.
    version: u64,
    // Pending facts never become an accounted baseline. Old metadata with an
    // actual latest sighting still decodes as Some without manufacturing one.
    #[serde(default)]
    latest: Option<EngramSourceSighting>,
    // A closing measurement whose change nothing has accounted yet. It never
    // becomes the baseline by itself; the next measured opening reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_close: Option<EngramPendingClose>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    observations: Vec<EngramSourceObservationIntent>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramPendingClose {
    sighting: EngramSourceSighting,
    // The grant whose checkpoint report carried the turn's own observation of
    // this close: that checkpoint, once granted, accounts it. None when no
    // accepted report can account it any more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reported_by: Option<String>,
}

/// What a measured opening reports, in order. With a close still owed the
/// opening is never Equal: it first reports the owed change itself, from
/// the accounted baseline to that close, then (`follow_up`) any further
/// change from the close to the opening, so a change that was reverted
/// before the opening is still reported rather than lost. An owed close
/// that equals the accounted baseline owes nothing.
struct EngramSourceOpeningPlan {
    baseline: Option<EngramSourceSighting>,
    sighting: EngramSourceSighting,
    follow_up: Option<EngramSourceSighting>,
    equal: bool,
}

fn engram_source_opening_plan(
    owner: Option<&EngramSourceSightingOwner>, opening: &EngramSourceSighting,
) -> EngramSourceOpeningPlan {
    let latest = owner.and_then(|owner| owner.latest.clone());
    match owner.and_then(engram_owed_close) {
        None => EngramSourceOpeningPlan {
            equal: latest.as_ref().is_some_and(|latest| latest.basis == opening.basis),
            baseline: latest, sighting: opening.clone(), follow_up: None,
        },
        Some(pending) => {
            // Each interval this plan adds runs forward in measured time. A
            // baseline stamped after the owed close cannot start it, so the
            // owed close is reported as assumed changed. A differing opening
            // always follows it, reported from the owed close unless its
            // stamp is earlier (`chain_source_observation_follow_up`).
            let closed = engram_sighting_time(&pending.sighting);
            let baseline = latest.filter(|latest| engram_sighting_time(latest) <= closed);
            let follow_up = (pending.sighting.basis != opening.basis).then(|| opening.clone());
            EngramSourceOpeningPlan { baseline, sighting: pending.sighting.clone(), follow_up, equal: false }
        }
    }
}

fn engram_sighting_time(sighting: &EngramSourceSighting) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(&sighting.observed_at).ok()
}

/// Whether `incoming` takes an owed slot holding `owed`: by master's
/// promotion convention, any measurement not stamped strictly earlier,
/// including a later one at the same revision; an exact duplicate does not.
/// It replaces the slot's sighting and the grant that would account it, so a
/// late Grant for the older close can no longer settle the newer duty.
fn engram_close_supersedes(incoming: &EngramSourceSighting, owed: &EngramSourceSighting) -> bool {
    incoming != owed && match (engram_sighting_time(incoming), engram_sighting_time(owed)) {
        (Some(incoming), Some(owed)) => incoming >= owed,
        _ => false,
    }
}

/// The close `owner` still owes: one at the accounted baseline's own
/// revision owes nothing, and never stops a newer close from being owed.
fn engram_owed_close(owner: &EngramSourceSightingOwner) -> Option<&EngramPendingClose> {
    owner.pending_close.as_ref().filter(|pending|
        owner.latest.as_ref().is_none_or(|latest| latest.basis != pending.sighting.basis))
}

/// The baseline a follow-up opening is reported from: the close it follows,
/// unless the clocks put the opening before that close. Then none (assumed
/// changed), so no reported interval runs backward in measured time.
fn engram_follow_up_baseline(
    close: &EngramSourceSighting, opening: &EngramSourceSighting,
) -> Option<EngramSourceSighting> {
    (engram_sighting_time(close) <= engram_sighting_time(opening)).then(|| close.clone())
}

/// A recorded observation accounts every change up to its sighting: the owed
/// close it reports itself, or one measured no later than it. An opening's
/// observation recorded under its live gate (`by_revision`) also accounts an
/// owed close at the very revision it records, whatever the clock said in
/// between, since that opening reports the change to that revision. A
/// finalization after the fact cannot: by then a later turn may owe a new
/// close at that same revision.
fn settle_engram_pending_close_by_observation(
    owner: &mut EngramSourceSightingOwner, sighting: &EngramSourceSighting, by_revision: bool,
) -> bool {
    let Ok(through) = chrono::DateTime::parse_from_rfc3339(&sighting.observed_at) else { return false; };
    let covered = owner.pending_close.as_ref().is_some_and(|pending| pending.sighting == *sighting
        || (by_revision && pending.sighting.basis == sighting.basis)
        || chrono::DateTime::parse_from_rfc3339(&pending.sighting.observed_at).is_ok_and(|closed| closed <= through));
    if covered {
        owner.pending_close = None;
        owner.version = owner.version.saturating_add(1);
    }
    covered
}

/// A granted checkpoint accounts its turn's close only when its report
/// carried the turn's own observation; otherwise that close stays owed to
/// the next opening. Keyed by the grant, so it holds wherever that
/// checkpoint is granted: the turn's own close or a later recovery.
fn settle_engram_pending_close_by_checkpoint_locked(inner: &mut StateInner, grant_id: &str, observed: bool) -> bool {
    // An opening gate that is live on a scope fenced its owner's version; its
    // own recorded observation will account the close, so leave that owner.
    let live = inner.sessions.iter().filter_map(|record| record.engram.source_observation_gate.as_ref()
        .filter(|gate| !gate.retired).map(|gate| gate.scope.clone())).collect::<Vec<_>>();
    let mut settled = false;
    for owner in &mut inner.engram_source_sightings {
        if owner.pending_close.as_ref().and_then(|pending| pending.reported_by.as_deref()) != Some(grant_id)
            || live.contains(&owner.scope) {
            continue;
        }
        let pending = owner.pending_close.take().expect("checked pending close");
        if observed {
            // Engram accepted the change, so the duty is settled even when a
            // later measurement keeps the close from becoming the baseline.
            promote_engram_source_sighting(owner, &pending.sighting);
        } else {
            owner.pending_close = Some(EngramPendingClose { reported_by: None, ..pending });
        }
        owner.version = owner.version.saturating_add(1);
        settled = true;
    }
    settled
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum EngramSourceOpeningDisposition {
    #[default]
    Pending,
    NoScope,
    Unmeasured { reason: String },
    Equal,
    Accounted { observation_id: String },
    HistoricalRootMove { observation_id: String },
}

impl EngramSourceOpeningDisposition {
    fn allows_delivery(&self) -> bool { !matches!(self, Self::Pending) }
    fn measured(&self) -> bool { matches!(self, Self::Equal | Self::Accounted { .. }) }
}

fn promote_engram_source_sighting(owner: &mut EngramSourceSightingOwner, sighting: &EngramSourceSighting) -> bool {
    let Ok(measured) = chrono::DateTime::parse_from_rfc3339(&sighting.observed_at) else { return false; };
    if let Some(previous) = &owner.latest {
        let Ok(previous_time) = chrono::DateTime::parse_from_rfc3339(&previous.observed_at) else { return false; };
        if measured < previous_time || previous == sighting { return false; }
    }
    owner.latest = Some(sighting.clone());
    owner.version = owner.version.saturating_add(1);
    true
}

/// Saved with the queued admission. The runtime payload and its local ACK
/// are deliberately absent after restart; an old receipt cannot replay it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramSourceObservationGate {
    scope: EngramSourceSightingScope,
    observation_id: String,
    owner_version: u64,
    prompt_id: String,
    dispatch_generation: u64,
    active_turn_generation: u64,
    grant_id: String,
    attempts: u32,
    policy_refreshed: bool,
    retry_at: String,
    reason: String,
    retired: bool,
}

#[derive(Clone, Debug)]
struct EngramSourceObservationContinuation {
    payload: DeferredEngramHandoff,
    runtime: RuntimeToken,
}

impl EngramSourceObservationGate {
    fn owns(&self, record: &SessionRecord) -> bool {
        !self.retired && !record.engram.source_observation_delete_requested
            && record.engram.dispatch_generation == self.dispatch_generation
            && record.active_turn_generation == self.active_turn_generation
            && record.engram.active_grant_id.as_ref() == Some(&self.grant_id)
            && record.queued_prompts.front().is_some_and(|head| head.pending_prompt.id == self.prompt_id)
    }
}

impl EngramSourceSightingOwner {
    fn matches_metadata(&self, metadata: &PersistedState) -> bool {
        metadata.engram_source_sightings.iter().any(|owner| owner == self)
    }
}

fn engram_source_observation_can_deliver(record: &SessionRecord) -> bool {
    !record.engram.source_observation_delete_requested
        && record.engram.source_opening_disposition.allows_delivery()
        && record.engram.active_grant_id.is_some()
        && record.engram.active_grant_id == record.engram.source_observation_delivery_grant
        && record.engram.source_observation_gate.as_ref().is_none_or(|gate| gate.owns(record))
}

// A fresh admission cannot replace recovery of a previously begun grant.
// Session ownership survives settings changes; a shared exact association
// also fences another session admitting the same work in the same store.
fn engram_source_observation_holds_admission(inner: &StateInner, index: usize) -> bool {
    if inner.sessions[index].engram.source_observation_delete_requested { return true; }
    let session_id = &inner.sessions[index].session.id;
    let target = AppState::engram_binding_target_for_session_shape_locked(inner, session_id, true)
        .ok().flatten();
    inner.engram_source_sightings.iter().any(|owner| owner.observations.iter().any(|intent| {
        source_observation_unresolved(intent) && (intent.session_id == *session_id || target.as_ref().is_some_and(|target|
            target.settings.authority_store_key.as_ref() == Some(&owner.scope.store)
                && target.work_binding.as_ref().is_some_and(|binding|
                    engram_same_recovery_run(binding, &intent.binding))))
    }))
}

fn hold_source_observation_admission(record: &mut SessionRecord) -> bool {
    let preview = if record.engram.source_observation_delete_requested {
        SOURCE_OBSERVATION_DELETE_RETAINED
    } else { "Engram: retained source observation requires explicit Resume before another admission." };
    let changed = !record.orchestrator_auto_dispatch_blocked || record.session.preview != preview;
    record.set_auto_dispatch_blocked(true);
    record.session.preview = preview.to_owned();
    changed
}

fn finish_source_observation_handoff_locked(inner: &mut StateInner, index: usize) {
    let gate = inner.sessions[index].engram.source_observation_gate.take();
    if let Some(gate) = gate {
        if let Some(owner) = inner.engram_source_sightings.iter_mut().find(|owner| owner.scope == gate.scope) {
            if let Some(intent) = owner.observations.iter_mut().find(|intent| intent.id == gate.observation_id) {
                intent.continuation_released = true;
                owner.version = owner.version.saturating_add(1);
            }
        }
    }
    inner.sessions[index].engram.source_observation_continuation = None;
}

/// Retirement does not settle a grant. Its original connection, routing and
/// facts stay in the durable outbox even if a successor owns the session now.
fn retire_source_observation_locked(inner: &mut StateInner, index: usize) -> bool {
    let Some(gate) = inner.sessions[index].engram.source_observation_gate.clone() else { return false; };
    if gate.retired { return false; }
    if let Some(owner) = inner.engram_source_sightings.iter_mut().find(|owner| owner.scope == gate.scope) {
        if let Some(intent) = owner.observations.iter_mut().find(|intent| intent.id == gate.observation_id) {
            intent.delivery_retired = true;
            intent.continuation_released = true;
            owner.version = owner.version.saturating_add(1);
        }
    }
    let record = &mut inner.sessions[index];
    // Never overwrite another recovery grant. The durable intent above remains
    // the complete owner even when the session's single mirror is occupied.
    let owned = gate.owns(record);
    if owned && record.engram.uncertain_grant_id.is_none() {
        record.engram.uncertain_grant_id = Some(gate.grant_id.clone());
        record.engram.active_grant_id = None;
    }
    if let Some(current) = &mut record.engram.source_observation_gate { current.retired = true; }
    let old_continuation = record.engram.source_observation_continuation.as_ref().is_some_and(|continuation|
        continuation.payload.0.lock().expect("source observation continuation mutex poisoned")
            .as_ref().is_some_and(|dispatch| dispatch.engram_dispatch_generation() == Some(gate.dispatch_generation)
                && dispatch.active_turn_generation() == gate.active_turn_generation));
    if old_continuation { record.engram.source_observation_continuation = None; }
    if owned || record.engram.source_observation_delivery_grant.as_ref() == Some(&gate.grant_id) {
        record.engram.source_opening_disposition = EngramSourceOpeningDisposition::Pending;
        record.engram.source_observation_delivery_grant = None;
    }
    if owned {
        record.session.status = SessionStatus::Idle;
        record.session.live_activity = None;
        record.set_auto_dispatch_blocked(true);
        record.engram.rebind_required = true;
    }
    inner.stamp_session_at_index(index);
    true
}

fn engram_source_sighting_scope_locked(
    inner: &StateInner,
    index: usize,
    basis: &EngramExecutionSourceBasis,
) -> Option<EngramSourceSightingScope> {
    if engram_turn_root_capture_locked(inner, &inner.sessions[index].session.id) == EngramRootCapture::Unconfirmed {
        return None;
    }
    engram_source_sighting_opening_scope_locked(inner, index, basis)
}

// The admitted measurement retains its association even if current authority
// becomes unavailable. This historical identity grants no delivery authority.
fn engram_source_sighting_opening_scope_locked(
    inner: &StateInner, index: usize, basis: &EngramExecutionSourceBasis,
) -> Option<EngramSourceSightingScope> {
    let record = &inner.sessions[index];
    let (store, work_id) = record.engram.active_turn_naming_identity.as_ref()?;
    let binding = record.engram.active_turn_source_binding.as_ref()?;
    if &binding.work_id != work_id || record.engram.work_binding.as_ref() != Some(binding) {
        return None;
    }
    Some(EngramSourceSightingScope {
        store: store.clone(), project_id: store.project_id.clone(),
        root_execution_id: binding.root_execution_id.clone(), work_id: binding.work_id.clone(),
        run_id: binding.run_id.clone(), claim_id: binding.claim_id.clone(),
        workspace_id: basis.workspace_id.clone(), source_root_generation: basis.source_root_generation,
        source_root_state: basis.source_root_state,
    })
}

/// A real closing measurement advances only its compatible scope. A receipt
/// does not make a measurement, and retained outbox facts are never replaced.
/// A close equal to the accounted baseline accounts itself; any other close
/// is kept as owed (`reported_by` names the grant whose report carried the
/// turn's own observation of it) until a granted checkpoint or a recorded
/// observation accounts it.
/// Returns the owner when the close became owed, so the caller can make it
/// durable before the checkpoint that may or may not account it.
fn retain_engram_closing_sighting_locked(
    inner: &mut StateInner,
    index: usize,
    basis: &EngramExecutionSourceBasis,
    observed_at: &str,
    reported_by: Option<&str>,
) -> Option<EngramSourceSightingOwner> {
    let record = &inner.sessions[index];
    if record.engram.active_turn_start_basis.is_none()
        || !record.engram.source_opening_disposition.measured() {
        return None;
    }
    let Some(scope) = engram_source_sighting_scope_locked(inner, index, basis) else { return None; };
    let sighting = EngramSourceSighting { basis: basis.clone(), observed_at: observed_at.to_owned() };
    let position = match inner.engram_source_sightings.iter().position(|owner| owner.scope == scope) {
        Some(position) => position,
        None => {
            inner.engram_source_sightings.push(EngramSourceSightingOwner {
                scope, version: 1, latest: None, pending_close: None, observations: Vec::new(),
            });
            inner.engram_source_sightings.len() - 1
        }
    };
    let owner = &mut inner.engram_source_sightings[position];
    // A close left owed at the baseline's own revision owes nothing, so it
    // never holds back the close measured now.
    if owner.pending_close.is_some() && engram_owed_close(owner).is_none() {
        owner.pending_close = None;
        owner.version = owner.version.saturating_add(1);
    }
    if owner.latest.as_ref().is_some_and(|latest| latest.basis == sighting.basis) {
        // While a close is owed the baseline keeps its time, so the owed
        // interval still starts before it ends.
        if owner.pending_close.is_none() {
            promote_engram_source_sighting(owner, &sighting);
        }
        return None;
    }
    // A differing close is owed even behind a baseline stamped later: the
    // close is never the baseline by itself, and the next opening reports it
    // without that baseline. With a close already owed, the newer measurement
    // keeps the slot, as master's promotion keeps the newer close.
    if chrono::DateTime::parse_from_rfc3339(&sighting.observed_at).is_err()
        || owner.pending_close.as_ref().is_some_and(|pending| !engram_close_supersedes(&sighting, &pending.sighting)) {
        return None;
    }
    owner.pending_close = Some(EngramPendingClose { sighting, reported_by: reported_by.map(str::to_owned) });
    owner.version = owner.version.saturating_add(1);
    Some(owner.clone())
}

impl AppState {
    /// Waits until `owner`, holding a newly owed close, is stored on the
    /// writer connection, so a crash while the following checkpoint is in
    /// flight still finds the owed close at restart.
    fn confirm_engram_owed_close_durable(
        &self, owner: &EngramSourceSightingOwner, timeout: Duration,
    ) -> Result<(), String> {
        let clock = self.engram_budget_clock();
        let deadline = clock.now() + timeout;
        let target = PersistFenceTarget::EngramSourceSightingHistory(Box::new(owner.clone()));
        let (fence, waiter) = PersistFence::new_with_clock(target.clone(), deadline, clock.clone());
        if self.persist_tx.send(PersistRequest::Fence(Box::new(fence))).is_ok() {
            return waiter.wait().map_err(|error| format!("{error:?}"));
        }
        if !engram_authority_manual_writer_allowed() {
            return Err("the persistence writer stopped".to_owned());
        }
        #[cfg(test)]
        {
            let delta = collect_persist_delta_from_shared_state(&self.inner, 0);
            let mut cache = SqlitePersistConnectionCache::new();
            persist_delta_via_cache(&mut cache, self.persistence_path.as_path(), &delta)
                .map_err(|error| format!("{error:#}"))?;
            if !cache.connection.as_ref().is_some_and(|connection| target.is_already_durable(connection).unwrap_or(false)) {
                return Err("the owed close was not committed".to_owned());
            }
        }
        Ok(())
    }
}
