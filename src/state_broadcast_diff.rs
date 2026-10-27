// Owns the broadcaster's `stateDiff` producer: the last cut it emitted, the
// keys forwarded deltas touched since that cut, and the diff wire payload. It
// runs only on the broadcaster thread and never reads StateInner or the
// projection cache; its inputs are owned cuts and forwarded deltas.
//
// Does not own: mailbox order, coalescing or loss (`state.rs`), the worker
// lifetime (`state_broadcast_worker.rs`), or which connection forwards which
// event kind (`api_sse.rs`).
//
// A diff is cumulative from the last cut emitted successfully. Without that
// base (worker start, a mailbox loss, a failed emission, a new server
// instance, or a pre-built snapshot), or when the cut was captured after an
// unanswered full-state request (its request generation is newer),
// the cut goes out as a full snapshot marked `stateDiffBase: true` that every
// connection forwards, and becomes the base. Clients build diffs only on a
// marked snapshot. A full snapshot of the same cut accompanies a diff only
// while legacy connections exist; it carries no mark.

/// The last cut the broadcaster emitted. Holding the session Arcs keeps their
/// identity comparable with later cuts: the projection cache mints a new Arc
/// for any captured difference and never mutates one in place.
struct EmittedStateCut {
    revision: u64,
    server_instance_id: String,
    sessions: Vec<Arc<StateSessionSummary>>,
    delegations: Vec<DelegationStateSummary>,
    test_runs: Vec<TestRunSummary>,
}

impl EmittedStateCut {
    fn capture(snapshot: &ImmutableStateSnapshot) -> Self {
        Self {
            revision: snapshot.header.revision,
            server_instance_id: snapshot.header.server_instance_id.clone(),
            sessions: snapshot.sessions.clone(),
            delegations: snapshot.header.delegations.clone(),
            test_runs: snapshot.header.test_runs.clone(),
        }
    }
}

/// Keys a forwarded delta changed on clients since the base cut. Their rows go
/// out absolutely even when equal to the base, because a client's copy may
/// still hold the delta's intermediate value.
#[derive(Default)]
struct StateDiffTouchedKeys {
    sessions: BTreeSet<String>,
    delegations: BTreeSet<String>,
    test_runs: BTreeSet<String>,
}

impl StateDiffTouchedKeys {
    fn record(&mut self, event: &DeltaEvent) {
        // Exhaustive on purpose: a new delta variant must declare its keys.
        match event {
            DeltaEvent::SessionCreated { session_id, .. }
            | DeltaEvent::MessageCreated { session_id, .. }
            | DeltaEvent::MessageUpdated { session_id, .. }
            | DeltaEvent::TextDelta { session_id, .. }
            | DeltaEvent::TextReplace { session_id, .. }
            | DeltaEvent::CommandUpdate { session_id, .. }
            | DeltaEvent::ParallelAgentsUpdate { session_id, .. }
            | DeltaEvent::TestRunCardUpdated { session_id, .. }
            | DeltaEvent::ConversationMarkerCreated { session_id, .. }
            | DeltaEvent::ConversationMarkerUpdated { session_id, .. }
            | DeltaEvent::ConversationMarkerDeleted { session_id, .. }
            | DeltaEvent::TestRunWaitConsumed { session_id, .. }
            | DeltaEvent::TestRunWaitResumeDispatchFailed { session_id, .. } => {
                self.sessions.insert(session_id.clone());
            }
            DeltaEvent::DelegationWaitConsumed {
                parent_session_id, ..
            }
            | DeltaEvent::DelegationWaitResumeDispatchFailed {
                parent_session_id, ..
            } => {
                self.sessions.insert(parent_session_id.clone());
            }
            DeltaEvent::DelegationWaitCreated { wait, .. } => {
                self.sessions.insert(wait.parent_session_id.clone());
            }
            DeltaEvent::TestRunWaitCreated { wait, .. } => {
                self.sessions.insert(wait.session_id.clone());
            }
            // Header slices go out absolutely in every diff.
            DeltaEvent::CodexUpdated { .. } => {}
            DeltaEvent::OrchestratorsUpdated { sessions, .. } => {
                self.sessions
                    .extend(sessions.iter().map(|session| session.id.clone()));
            }
            DeltaEvent::DelegationCreated { delegation, .. } => {
                self.delegations.insert(delegation.id.clone());
                self.sessions.insert(delegation.parent_session_id.clone());
                self.sessions.insert(delegation.child_session_id.clone());
            }
            DeltaEvent::DelegationUpdated { delegation_id, .. }
            | DeltaEvent::DelegationCompleted { delegation_id, .. }
            | DeltaEvent::DelegationFailed { delegation_id, .. }
            | DeltaEvent::DelegationCanceled { delegation_id, .. } => {
                self.delegations.insert(delegation_id.clone());
            }
            DeltaEvent::TestRunChanged { run, .. } => {
                self.test_runs.insert(run.run_id.clone());
            }
            DeltaEvent::TestRunRemoved { run_id, .. } => {
                self.test_runs.insert(run_id.clone());
            }
        }
    }
}

/// The `stateDiff` SSE payload. Header slices are absolute and keep the shapes
/// `StateResponse` gives them; keyed collections carry absolute changed rows
/// and removed keys.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StateDiffPayload<'a> {
    base_revision: u64,
    revision: u64,
    server_instance_id: &'a str,
    header: StateDiffHeader<'a>,
    sessions: StateDiffSessions<'a>,
    delegations: StateDiffRows<'a, DelegationStateSummary>,
    test_runs: StateDiffRows<'a, TestRunSummary>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StateDiffHeader<'a> {
    codex: &'a CodexState,
    agent_readiness: &'a [AgentReadiness],
    preferences: &'a AppPreferences,
    #[serde(serialize_with = "serialize_state_diff_projects")]
    projects: &'a [Project],
    orchestrators: &'a [OrchestratorInstance],
    workspaces: &'a [WorkspaceLayoutSummary],
    delegation_waits: &'a [DelegationWaitRecord],
    test_run_waits: &'a [TestRunWaitRecord],
    pending_engram_mcp_revocation_session_ids: &'a [String],
}

/// Projects use the same client projection as `StateResponse::projects`.
fn serialize_state_diff_projects<S>(projects: &&[Project], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serialize_client_projects_with_engram_status(projects, serializer)
}

#[derive(Serialize)]
struct StateDiffSessions<'a> {
    changed: Vec<&'a StateSessionSummary>,
    removed: Vec<&'a str>,
    /// The full id sequence, present only when membership or order changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    order: Option<Vec<&'a str>>,
}

#[derive(Serialize)]
struct StateDiffRows<'a, T> {
    changed: Vec<&'a T>,
    removed: Vec<&'a str>,
}

fn delegation_row_key(row: &DelegationStateSummary) -> &str {
    &row.id
}

fn test_run_row_key(row: &TestRunSummary) -> &str {
    &row.run_id
}

/// Base keys absent from the cut, then touched keys absent from both.
fn state_diff_removed_keys<'a>(
    base_keys: impl Iterator<Item = &'a str>,
    touched: &'a BTreeSet<String>,
    current: &HashSet<&'a str>,
) -> Vec<&'a str> {
    let mut removed: Vec<&'a str> = base_keys.filter(|key| !current.contains(key)).collect();
    let mut listed: HashSet<&'a str> = removed.iter().copied().collect();
    for key in touched {
        let key = key.as_str();
        if !current.contains(key) && listed.insert(key) {
            removed.push(key);
        }
    }
    removed
}

fn state_diff_rows<'a, T: PartialEq>(
    base: &'a [T],
    current: &'a [T],
    touched: &'a BTreeSet<String>,
    key: fn(&T) -> &str,
) -> StateDiffRows<'a, T> {
    let base_rows: HashMap<&'a str, &'a T> = base.iter().map(|row| (key(row), row)).collect();
    let changed = current
        .iter()
        .filter(|row| {
            touched.contains(key(row))
                || base_rows
                    .get(key(row))
                    .is_none_or(|base_row| *base_row != *row)
        })
        .collect();
    let current_keys: HashSet<&'a str> = current.iter().map(key).collect();
    StateDiffRows {
        changed,
        removed: state_diff_removed_keys(base.iter().map(key), touched, &current_keys),
    }
}

fn state_diff_payload<'a>(
    base: &'a EmittedStateCut,
    cut: &'a ImmutableStateSnapshot,
    touched: &'a StateDiffTouchedKeys,
) -> StateDiffPayload<'a> {
    // Exhaustive on purpose: a new `StateResponse` field must decide whether
    // it is a header slice, a keyed collection, or carried elsewhere.
    let StateResponse {
        revision,
        server_instance_id,
        codex,
        agent_readiness,
        preferences,
        projects,
        orchestrators,
        workspaces,
        // A cut's header lists no sessions; `cut.sessions` holds them.
        sessions: _,
        delegations,
        delegation_waits,
        test_runs,
        test_run_waits,
        pending_engram_mcp_revocation_session_ids,
    } = &cut.header;
    let base_sessions: HashMap<&'a str, &'a Arc<StateSessionSummary>> = base
        .sessions
        .iter()
        .map(|session| (session.id.as_str(), session))
        .collect();
    let changed = cut
        .sessions
        .iter()
        .filter(|session| {
            touched.sessions.contains(&session.id)
                || base_sessions
                    .get(session.id.as_str())
                    .is_none_or(|base_session| !Arc::ptr_eq(*base_session, *session))
        })
        .map(|session| &**session)
        .collect();
    let current_ids: HashSet<&'a str> = cut
        .sessions
        .iter()
        .map(|session| session.id.as_str())
        .collect();
    let same_order = base.sessions.len() == cut.sessions.len()
        && base
            .sessions
            .iter()
            .zip(&cut.sessions)
            .all(|(base_session, session)| base_session.id == session.id);
    StateDiffPayload {
        base_revision: base.revision,
        revision: *revision,
        server_instance_id,
        header: StateDiffHeader {
            codex,
            agent_readiness,
            preferences,
            projects,
            orchestrators,
            workspaces,
            delegation_waits,
            test_run_waits,
            pending_engram_mcp_revocation_session_ids,
        },
        sessions: StateDiffSessions {
            changed,
            removed: state_diff_removed_keys(
                base.sessions.iter().map(|session| session.id.as_str()),
                &touched.sessions,
                &current_ids,
            ),
            order: (!same_order).then(|| {
                cut.sessions
                    .iter()
                    .map(|session| session.id.as_str())
                    .collect()
            }),
        },
        delegations: state_diff_rows(
            &base.delegations,
            delegations,
            &touched.delegations,
            delegation_row_key,
        ),
        test_runs: state_diff_rows(
            &base.test_runs,
            test_runs,
            &touched.test_runs,
            test_run_row_key,
        ),
    }
}

/// A full state the broadcaster sends as a diff base: today's bytes plus a
/// trailing `"stateDiffBase":true`. Clients that do not diff ignore the field.
#[derive(Serialize)]
struct MarkedStateResponse<'a> {
    #[serde(flatten)]
    state: &'a StateResponse,
    #[serde(rename = "stateDiffBase")]
    state_diff_base: bool,
}

/// Serializes a cut exactly as the full `state` event has always carried it,
/// with the diff-base mark when `marked`.
fn serialize_full_state_cut(snapshot: ImmutableStateSnapshot, marked: bool) -> Option<String> {
    let revision = snapshot.header.revision;
    let state = snapshot.materialize();
    let payload = if marked {
        serde_json::to_string(&MarkedStateResponse {
            state: &state,
            state_diff_base: true,
        })
    } else {
        serde_json::to_string(&state)
    };
    match payload {
        Ok(payload) => Some(payload),
        Err(err) => {
            eprintln!("warning: failed to serialize SSE state snapshot at revision {revision}: {err}");
            None
        }
    }
}

/// Why the broadcaster sent a marked full state instead of a diff.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StateDiffRebase {
    /// Worker start, or a pre-built snapshot left no comparable base.
    NoBase,
    Lag,
    /// The previous emission failed to serialize.
    Failure,
    /// The previous emission reached no subscriber: nothing failed, but no
    /// client holds that cut either.
    NoReceivers,
    AlignmentRequest,
    NewInstance,
}

/// Host-wide broadcaster counters, read by the persistence diagnostics route.
#[derive(Default)]
struct StateDiffCounters {
    diffs_emitted: AtomicU64,
    marked_no_base: AtomicU64,
    marked_lag: AtomicU64,
    marked_failure: AtomicU64,
    marked_no_receivers: AtomicU64,
    marked_alignment_request: AtomicU64,
    marked_new_instance: AtomicU64,
    legacy_paired_full_states: AtomicU64,
    /// `POST /api/events/full-state` calls with `mode: "cut"`.
    cut_requests: AtomicU64,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct StateDiffCountersSnapshot {
    diffs_emitted: u64,
    marked_full_states: StateDiffMarkedCounts,
    legacy_paired_full_states: u64,
    cut_requests: u64,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct StateDiffMarkedCounts {
    no_base: u64,
    lag: u64,
    failure: u64,
    no_receivers: u64,
    alignment_request: u64,
    new_instance: u64,
}

impl StateDiffCounters {
    fn count_marked(&self, reason: StateDiffRebase) {
        let counter = match reason {
            StateDiffRebase::NoBase => &self.marked_no_base,
            StateDiffRebase::Lag => &self.marked_lag,
            StateDiffRebase::Failure => &self.marked_failure,
            StateDiffRebase::NoReceivers => &self.marked_no_receivers,
            StateDiffRebase::AlignmentRequest => &self.marked_alignment_request,
            StateDiffRebase::NewInstance => &self.marked_new_instance,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> StateDiffCountersSnapshot {
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        StateDiffCountersSnapshot {
            diffs_emitted: read(&self.diffs_emitted),
            marked_full_states: StateDiffMarkedCounts {
                no_base: read(&self.marked_no_base),
                lag: read(&self.marked_lag),
                failure: read(&self.marked_failure),
                no_receivers: read(&self.marked_no_receivers),
                alignment_request: read(&self.marked_alignment_request),
                new_instance: read(&self.marked_new_instance),
            },
            legacy_paired_full_states: read(&self.legacy_paired_full_states),
            cut_requests: read(&self.cut_requests),
        }
    }
}

/// Worker-local diff state. Created when the broadcaster thread starts and
/// dropped only after its final drain, so closing never discards a base that
/// late work still diffs from.
struct StateDiffProducer {
    base: Option<EmittedStateCut>,
    touched: StateDiffTouchedKeys,
    /// Why there is no base; the next marked full state is counted under it.
    rebase: StateDiffRebase,
    /// The newest full-state request generation a delivered marked full
    /// state has answered. A cut captured at a newer generation goes out
    /// marked; an older cut cannot answer a later request.
    satisfied_generation: u64,
}

impl Default for StateDiffProducer {
    fn default() -> Self {
        Self {
            base: None,
            touched: StateDiffTouchedKeys::default(),
            rebase: StateDiffRebase::NoBase,
            satisfied_generation: 0,
        }
    }
}

impl StateDiffProducer {
    fn forward(&mut self, work: StateBroadcastWork, senders: &StateBroadcastSenders) {
        match work {
            StateBroadcastWork::ImmutableSnapshot(snapshot) => self.forward_cut(snapshot, senders),
            StateBroadcastWork::Delta(event) => {
                self.touched.record(&event);
                forward_state_broadcast_work(StateBroadcastWork::Delta(event), senders);
            }
            // A pre-built snapshot goes to every connection unmarked and
            // leaves no comparable base; diffs restart from the next cut.
            work @ StateBroadcastWork::Snapshot(_) => {
                self.reset(StateDiffRebase::NoBase);
                forward_state_broadcast_work(work, senders);
            }
            // A loss means clients recover from a full state; the base and
            // touched keys are discarded before the marker goes out.
            StateBroadcastWork::Lagged => {
                self.reset(StateDiffRebase::Lag);
                forward_state_broadcast_work(StateBroadcastWork::Lagged, senders);
            }
            StateBroadcastWork::Drain => panic!("drain is handled by the owned broadcaster"),
        }
    }

    fn reset(&mut self, reason: StateDiffRebase) {
        self.base = None;
        self.touched = StateDiffTouchedKeys::default();
        self.rebase = reason;
    }

    /// Sends the cut as a marked full state to every connection. Only a
    /// delivered one becomes the base.
    fn emit_marked_cut(
        &mut self,
        snapshot: ImmutableStateSnapshot,
        reason: StateDiffRebase,
        senders: &StateBroadcastSenders,
    ) {
        let next_base = EmittedStateCut::capture(&snapshot);
        let request_generation = snapshot.request_generation;
        let Some(payload) = serialize_full_state_cut(snapshot, true) else {
            self.rebase = StateDiffRebase::Failure;
            return;
        };
        if senders.send_state(payload) {
            senders.diff_counters.count_marked(reason);
            self.base = Some(next_base);
            // Every request this cut was captured after is answered.
            self.satisfied_generation = self.satisfied_generation.max(request_generation);
        } else {
            self.rebase = StateDiffRebase::NoReceivers;
        }
    }

    /// Sends the unmarked full state legacy connections receive for a cut
    /// that clients with diffs get another way. `Err` names why it did not
    /// reach anyone.
    fn send_legacy_pair(
        snapshot: ImmutableStateSnapshot,
        senders: &StateBroadcastSenders,
    ) -> Result<(), StateDiffRebase> {
        let payload = serialize_full_state_cut(snapshot, false).ok_or(StateDiffRebase::Failure)?;
        if !senders.send_legacy_state(payload) {
            return Err(StateDiffRebase::NoReceivers);
        }
        senders
            .diff_counters
            .legacy_paired_full_states
            .fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn forward_cut(&mut self, snapshot: ImmutableStateSnapshot, senders: &StateBroadcastSenders) {
        // The same consumer hooks as the full-state path, which a diff skips.
        #[cfg(test)]
        let snapshot = {
            let mut snapshot = snapshot;
            TEST_BEFORE_STATE_SERIALIZE.with(|hook| {
                if let Some(hook) = hook.borrow_mut().take() {
                    hook();
                }
            });
            if let Some(hook) = snapshot.before_materialize.take() {
                hook();
            }
            snapshot
        };
        let touched = std::mem::take(&mut self.touched);
        // Only a cut captured after an unanswered request answers it; an
        // older cut still in flight carries an older generation.
        let alignment_requested = snapshot.request_generation > self.satisfied_generation;
        // Every path below leaves no base unless its emission succeeded.
        let base = match self.base.take() {
            None => Err(self.rebase),
            Some(base) if base.server_instance_id != snapshot.header.server_instance_id => {
                Err(StateDiffRebase::NewInstance)
            }
            Some(_) if alignment_requested => Err(StateDiffRebase::AlignmentRequest),
            Some(base) => Ok(base),
        };
        let base = match base {
            Ok(base) => base,
            Err(reason) => return self.emit_marked_cut(snapshot, reason, senders),
        };
        if snapshot.header.revision <= base.revision {
            // A publication without a revision bump: a client at the base's
            // revision would ignore a diff to this one, so its changes must
            // ride the next diff instead. Keep the old base and every touched
            // key; Arc identity against that base still finds this cut's
            // changes. Legacy connections get the full state as always.
            self.base = Some(base);
            self.touched = touched;
            if senders.legacy_state_demand() {
                // A failed legacy pair leaves the diff base intact: diff
                // clients never relied on it.
                let _ = Self::send_legacy_pair(snapshot, senders);
            }
            return;
        }
        // Ends the payload's borrow of `snapshot` before a failure needs it.
        let diff = serde_json::to_string(&state_diff_payload(&base, &snapshot, &touched));
        let diff = match diff {
            Ok(diff) => diff,
            Err(err) => {
                eprintln!(
                    "warning: failed to serialize SSE state diff at revision {}: {err}",
                    snapshot.header.revision
                );
                // No base: the next cut goes out marked, counted as a
                // failure. Legacy connections still get this cut in full.
                self.rebase = StateDiffRebase::Failure;
                if senders.legacy_state_demand() {
                    let _ = Self::send_legacy_pair(snapshot, senders);
                }
                return;
            }
        };
        let next_base = EmittedStateCut::capture(&snapshot);
        let mut outcome = if senders.send_state_diff(diff) {
            senders.diff_counters.diffs_emitted.fetch_add(1, Ordering::Relaxed);
            Ok(())
        } else {
            Err(StateDiffRebase::NoReceivers)
        };
        // Read after the diff is out: a legacy connection counts itself before
        // subscribing, so one that could have received this diff is counted.
        if senders.legacy_state_demand() {
            let paired = Self::send_legacy_pair(snapshot, senders);
            outcome = outcome.and(paired);
        }
        match outcome {
            Ok(()) => self.base = Some(next_base),
            Err(reason) => self.rebase = reason,
        }
    }
}
