// The host-private named-root protocol: durable transition intents, wire
// receipts, authoritative lifecycle reconciliation and capture provenance.
// Path validation and the agent's naming request live in engram_source_roots.rs.

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramNamedRootKind {
    Bound,
    Ended,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramNamedRootEndReason {
    ExplicitClear,
    SessionGoneAtRestore,
    RootInvalid,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramSourceRootState {
    Named,
    Ended,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum EngramNamedRootState {
    None,
    Bound {
        workspace_id: String,
        generation: i64,
        named_at: String,
    },
    UnboundByRelease {
        last_generation: i64,
        released_at_position: i64,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramNamedRootReceipt {
    event: String,
    position: EngramNamedRootReadPosition,
    workspace_id: String,
    generation: i64,
    kind: EngramNamedRootKind,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramNamedRootReadResponse {
    project_id: String,
    work_id: String,
    root_execution_id: String,
    run_id: String,
    claim_id: String,
    run: EngramNamedRootReadRun,
    named_root: EngramNamedRootState,
    latest_event: Option<EngramNamedRootReadEvent>,
    read_cut: EngramNamedRootReadPosition,
    /// The read claim's own state (engram_named_root_capacity.rs). Older
    /// Engram builds do not send it; its absence proves nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claim: Option<EngramNamedRootReadClaim>,
}

/// The claim a named-root read is for, as Engram holds it: `state` is
/// `active`, `released` or `completed`; an active claim past `expires_at`
/// has lapsed; `fence` advances when a lapsed claim is recovered.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramNamedRootReadClaim {
    state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    holder: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revision: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fence: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramNamedRootReadRun {
    state: String,
    generation: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramNamedRootReadPosition {
    feed: EngramNamedRootReadFeed,
    position: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramNamedRootReadFeed {
    kind: String,
    id: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct EngramNamedRootReadEvent {
    event: String,
    position: EngramNamedRootReadPosition,
    generation: i64,
    kind: EngramNamedRootKind,
    workspace_id: String,
    named_at: String,
}

impl EngramNamedRootReadResponse {
    /// No-event absence on an open run cannot retire a potentially executable
    /// intent. Terminal lifecycle, the exact committed binding, or a strictly
    /// newer binding makes this immutable replay harmless or refused.
    fn settles_intent(&self, intent: &EngramNamedRootEvent) -> bool {
        if matches!(self.run.state.as_str(), "completed" | "cancelled")
            || matches!(
                self.named_root,
                EngramNamedRootState::UnboundByRelease { .. }
            )
        {
            return true;
        }
        self.latest_event.as_ref().is_some_and(|event| {
            event.generation > intent.root.generation as i64
                || event.generation == intent.root.generation as i64
                    && event.workspace_id == intent.root.root
                    && engram_named_at_matches(&event.named_at, &intent.root.named_at)
                    && (event.kind == intent.kind || event.kind == EngramNamedRootKind::Ended)
        })
    }
    /// A read never supplies a receipt. It must cover the real receipt already
    /// held and associate both positions with this exact canonical run.
    fn covers(&self, journal: &EngramNamedRootJournal) -> bool {
        let Some(binding) = &journal.read_binding else {
            return false;
        };
        let Some((event, receipt)) = &journal.confirmed else {
            return false;
        };
        let Some(latest) = &self.latest_event else {
            return false;
        };
        let feed_matches = |position: &EngramNamedRootReadPosition| {
            position.feed.kind == "run_execution" && position.feed.id == binding.run_id
        };
        self.project_id == journal.store.project_id
            && self.work_id == binding.work_id
            && self.work_id == event.root.work_id
            && self.run_id == binding.run_id
            && self.claim_id == binding.claim_id
            && self.claim_id == journal.claim_id
            && self.root_execution_id == binding.root_execution_id
            && feed_matches(&self.read_cut)
            && feed_matches(&latest.position)
            && feed_matches(&receipt.position)
            && receipt.position.position > 0
            && latest.position.position >= receipt.position.position
            && self.read_cut.position >= latest.position.position
            && (latest.position.position != receipt.position.position
                || latest.event == receipt.event && latest.kind == receipt.kind)
            && latest.generation >= receipt.generation
            && (latest.generation != receipt.generation
                || latest.workspace_id == receipt.workspace_id
                    && engram_named_at_matches(&latest.named_at, &event.root.named_at))
            && !latest.event.is_empty()
    }

    fn definitively_unbound(&self) -> bool {
        let Some(latest) = &self.latest_event else {
            return false;
        };
        match &self.named_root {
            EngramNamedRootState::None => {
                latest.kind == EngramNamedRootKind::Ended
                    || matches!(self.run.state.as_str(), "completed" | "cancelled")
            }
            EngramNamedRootState::UnboundByRelease {
                last_generation,
                released_at_position,
            } => {
                *last_generation >= latest.generation
                    && *released_at_position > latest.position.position
                    && *released_at_position <= self.read_cut.position
            }
            _ => false,
        }
    }
}

/// The entire immutable retry intent, including the reporting session and
/// original fence. Routing tokens may change; the event's content may not.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramNamedRootEvent {
    root: EngramWorkSourceRoot,
    reporter: String,
    kind: EngramNamedRootKind,
    end_reason: Option<EngramNamedRootEndReason>,
}

impl EngramNamedRootEvent {
    fn request(&self, routing_token: &str) -> Result<EngramControlRequest, EngramTransportError> {
        let generation = i64::try_from(self.root.generation)
            .ok()
            .filter(|generation| *generation > 0)
            .ok_or_else(|| {
                EngramTransportError::local_state("named-root generation is out of range")
            })?;
        let kind = match self.kind {
            EngramNamedRootKind::Bound => "bound",
            EngramNamedRootKind::Ended => "ended",
        };
        Ok(EngramControlRequest::NamedRootBind {
            routing_token: routing_token.to_owned(),
            claim_id: self.root.claim_id.clone(),
            claim_fence: self.root.claim_fence,
            workspace_id: self.root.root.clone(),
            generation,
            named_at: self.root.named_at.clone(),
            kind: self.kind,
            end_reason: self.end_reason,
            idempotency_key: format!(
                "termal-named-root:{}:{generation}:{kind}",
                self.root.claim_id
            ),
        })
    }

    fn matches_receipt(&self, receipt: &EngramNamedRootReceipt) -> bool {
        !receipt.event.is_empty()
            && receipt.position.position > 0
            && receipt.position.feed.kind == "run_execution"
            && !receipt.position.feed.id.is_empty()
            && receipt.workspace_id == self.root.root
            && u64::try_from(receipt.generation).ok() == Some(self.root.generation)
            && receipt.kind == self.kind
    }
}

/// One claim's last acknowledged transition and any request whose outcome
/// still needs settling. Kept after a clear so ordinary-workdir sightings
/// can state the ended generation. Pending content is saved before sending.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramNamedRootJournal {
    store: EngramAuthorityStoreKey,
    claim_id: String,
    /// Exact retained run identity for lifecycle reads after focus/removal.
    #[serde(default)]
    read_binding: Option<EngramControlWorkBinding>,
    confirmed: Option<(EngramNamedRootEvent, EngramNamedRootReceipt)>,
    pending: Option<EngramNamedRootEvent>,
    /// Fresh lifecycle readback superseded the acknowledged generation.
    /// This is not an event receipt and supplies no ended provenance.
    #[serde(default)]
    obsolete: bool,
    #[serde(default)]
    retirement: Option<EngramRootRetirement>,
    #[serde(default)]
    reconciliation: Option<EngramRootReconciliation>,
}

/// Retirement revokes new capture authority, not the historical receipt.
/// Only definitive unbinding and replacement by another current claim can
/// discard unused history. Unresolved authority keeps its refusal durable.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramRootRetirement {
    Unbound,
    DifferentActive,
    Displaced,
    NamingSessionRemoved,
    AssociationUnconfirmed,
}

/// Validated association for a recoverable intent. Fresh held claims also
/// validate their renewable fence; retained cleanup uses the original run.
struct EngramRootIntentAssociation {
    store: EngramAuthorityStoreKey,
    binding: EngramControlWorkBinding,
}

impl EngramRootIntentAssociation {
    fn validate(
        store: &EngramAuthorityStoreKey,
        work_id: &str,
        claim_id: &str,
        expected_fence: Option<i64>,
        binding: Option<&EngramControlWorkBinding>,
    ) -> Result<Self, ApiError> {
        let binding = binding.filter(|binding| {
            !store.project_id.is_empty()
                && !work_id.is_empty()
                && !claim_id.is_empty()
                && binding.work_id == work_id
                && binding.claim_id == claim_id
                && !binding.run_id.is_empty()
                && !binding.root_execution_id.is_empty()
                && binding.work_revision > 0
                && binding.claim_fence > 0
                && expected_fence.is_none_or(|fence| binding.claim_fence == fence)
        }).ok_or_else(|| ApiError::conflict(
            "naming requires this held claim's exact canonical run association; refresh the claim before naming; any earlier uncertain intent is retained",
        ))?;
        Ok(Self {
            store: store.clone(),
            binding: binding.clone(),
        })
    }

    fn for_event(
        event: &EngramNamedRootEvent,
        binding: Option<&EngramControlWorkBinding>,
    ) -> Result<Self, ApiError> {
        Self::validate(
            &event.root.store,
            &event.root.work_id,
            &event.root.claim_id,
            None,
            binding,
        )
    }
}

impl EngramNamedRootJournal {
    fn requires_reconciliation(&self) -> bool {
        self.pending.is_some()
            || self
                .reconciliation
                .as_ref()
                .is_some_and(|read| !read.settled)
            || (self.obsolete
                && !matches!(
                    self.retirement,
                    Some(EngramRootRetirement::Unbound | EngramRootRetirement::Displaced)
                ))
    }

    fn retire(&mut self, retirement: EngramRootRetirement) {
        self.obsolete = true;
        self.retirement = Some(retirement);
    }
}

fn engram_root_retirement_for_state(state: &EngramNamedRootState) -> EngramRootRetirement {
    match state {
        EngramNamedRootState::None | EngramNamedRootState::UnboundByRelease { .. } => {
            EngramRootRetirement::Unbound
        }
        _ => EngramRootRetirement::DifferentActive,
    }
}

/// Snapshot the exact local claim transitions before off-lock lifecycle I/O.
/// An end can reuse a generation, so a counter alone cannot fence its reply.
#[derive(Clone)]
struct EngramRootReadFence {
    generation: u64,
    roots: Vec<EngramWorkSourceRoot>,
    journals: Vec<EngramNamedRootJournal>,
    stores: Vec<(String, Option<EngramAuthorityStoreKey>)>,
}

// Test-only construction counts for the authority snapshots a scan may build
// under the global lock: a full binding target, and the root read fence each
// target captures (which clones every root, journal and project store).
#[cfg(test)]
thread_local! {
    static ENGRAM_ROOT_READ_FENCE_CAPTURES: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    static ENGRAM_BINDING_TARGET_BUILDS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn reset_engram_authority_build_counts() {
    ENGRAM_ROOT_READ_FENCE_CAPTURES.with(|count| count.set(0));
    ENGRAM_BINDING_TARGET_BUILDS.with(|count| count.set(0));
}

/// (root read fences captured, binding targets built) on this thread since
/// the last reset.
#[cfg(test)]
fn engram_authority_build_counts() -> (usize, usize) {
    (
        ENGRAM_ROOT_READ_FENCE_CAPTURES.with(std::cell::Cell::get),
        ENGRAM_BINDING_TARGET_BUILDS.with(std::cell::Cell::get),
    )
}

#[cfg(test)]
fn count_engram_binding_target_build() {
    ENGRAM_BINDING_TARGET_BUILDS.with(|count| count.set(count.get() + 1));
}

impl EngramRootReadFence {
    fn capture(inner: &StateInner) -> Self {
        #[cfg(test)]
        ENGRAM_ROOT_READ_FENCE_CAPTURES.with(|count| count.set(count.get() + 1));
        Self {
            generation: inner.engram_source_root_generation,
            roots: inner.engram_work_source_roots.clone(),
            journals: inner.engram_named_root_journal.clone(),
            stores: inner
                .projects
                .iter()
                .map(|project| {
                    (
                        project.id.clone(),
                        project
                            .engram
                            .as_ref()
                            .and_then(|settings| settings.authority_store_key.clone()),
                    )
                })
                .collect(),
        }
    }

    fn same_store(&self, inner: &StateInner, project_id: &str) -> bool {
        self.stores
            .iter()
            .find(|(id, _)| id == project_id)
            .map(|(_, store)| store)
            == inner
                .find_project(project_id)
                .map(|project| &project.engram)
                .map(|settings| {
                    settings
                        .as_ref()
                        .and_then(|settings| settings.authority_store_key.clone())
                })
                .as_ref()
    }

    fn still_current(
        &self,
        inner: &StateInner,
        store: &EngramAuthorityStoreKey,
        binding: &EngramControlWorkBinding,
    ) -> bool {
        engram_root_journal(&self.journals, store, &binding.claim_id)
            == engram_root_journal(&inner.engram_named_root_journal, store, &binding.claim_id)
            && engram_work_source_root_for_work(&self.roots, store, &binding.work_id)
                == engram_work_source_root_for_work(
                    &inner.engram_work_source_roots,
                    store,
                    &binding.work_id,
                )
    }
}

/// Readback retires an orphan's retry obligation without manufacturing an
/// event receipt. Keep the immutable intent alongside the state actually read.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramRootReconciliation {
    intent: EngramNamedRootEvent,
    state: EngramNamedRootState,
    observed_by: String,
    /// Host receipt time for this read, not an Engram event time or read cut.
    #[serde(default)]
    observed_at: String,
    /// Only covering lifecycle proof can retire an uncertain write. The
    /// original intent stays here as provenance, never as an event receipt.
    #[serde(default)]
    settled: bool,
}

const ENGRAM_NAMED_ROOT_JOURNAL_LIMIT: usize = 256;

/// History belongs to a work, rather than the disposable receipts of its
/// successive claims. The revision orders publication, not allocation.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramWorkNamingHistory {
    store: EngramAuthorityStoreKey,
    work_id: String,
    revision: u64,
    known_generation: u64,
    /// Numeric-only legacy records are never a canonical history frontier.
    #[serde(default)]
    epoch: u64,
    #[serde(default)]
    frontier: Option<EngramNamingFrontier>,
    #[serde(default)]
    owner_version: u64,
    #[serde(default)]
    transition: Option<EngramAuthorityTransition>,
    #[serde(default)]
    unresolved_runs: Vec<EngramControlWorkBinding>,
    /// Canonical no-event answers are knowledge too, scoped to the exact run.
    #[serde(default)]
    proofs: Vec<EngramAuthorityProof>,
    #[serde(default)]
    recovery_reason: Option<String>,
    /// Unpublished retirements still reserve capacity. The exact authority
    /// image owns their durable notice, independently of session focus.
    #[serde(default)]
    retirements: Vec<EngramNamedRootRetirementNotice>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramWorkNamingToken {
    work_id: String,
    revision: u64,
    #[serde(default)]
    epoch: u64,
    /// The binding the requested claim named when the token was taken, with
    /// the canonical run it was acknowledged in. A canonical read after the
    /// naming may learn that very naming as the history's frontier and move
    /// the revision once, which describes no change (see
    /// `engram_work_naming_change`). Absent for a workdir evaluation and from
    /// a token persisted before it existed: such a token compares the bare
    /// revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    named_binding: Option<EngramWorkNamingBound>,
}

/// A named binding as the canonical history records it: the claim, the
/// workspace, its claim-local root generation and named_at, and the
/// canonical run (identity and ordinal) its naming belongs to.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramWorkNamingBound {
    claim_id: String,
    workspace: String,
    generation: u64,
    named_at: String,
    run_id: String,
    run_generation: i64,
}

impl EngramWorkNamingBound {
    /// The binding `entry` names, in the canonical run of the latest proof
    /// `history` acknowledged for the entry's claim. None without such a
    /// proof: a binding whose run is unknown cannot be proved later.
    fn of_entry(entry: &EngramWorkSourceRoot, history: &EngramWorkNamingHistory) -> Option<Self> {
        let proof = history
            .proofs
            .iter()
            .filter(|proof| proof.binding.claim_id == entry.claim_id)
            .max_by_key(|proof| proof.read.run.generation)?;
        Some(Self {
            claim_id: entry.claim_id.clone(),
            workspace: entry.root.clone(),
            generation: entry.generation,
            named_at: entry.named_at.clone(),
            run_id: proof.read.run_id.clone(),
            run_generation: proof.read.run.generation,
        })
    }

    /// Whether the canonical `frontier` is the Bound event of this binding,
    /// in the same canonical run.
    fn is_frontier(&self, frontier: &EngramNamingFrontier) -> bool {
        frontier.kind == EngramNamedRootKind::Bound
            && frontier.claim_id == self.claim_id
            && frontier.workspace_id == self.workspace
            && u64::try_from(frontier.root_generation).ok() == Some(self.generation)
            && engram_named_at_matches(&frontier.named_at, &self.named_at)
            && frontier.run_id == self.run_id
            && frontier.run_generation == self.run_generation
    }
}

fn engram_work_naming_token(
    inner: &StateInner,
    store: &EngramAuthorityStoreKey,
    work_id: &str,
) -> EngramWorkNamingToken {
    let history = inner
        .engram_work_naming_history
        .iter()
        .find(|history| &history.store == store && history.work_id == work_id);
    EngramWorkNamingToken {
        work_id: work_id.to_owned(),
        revision: history.map_or(0, |history| history.revision),
        epoch: history.map_or(0, |history| history.epoch),
        named_binding: None,
    }
}

fn engram_record_naming_history(
    inner: &mut StateInner,
    store: &EngramAuthorityStoreKey,
    work_id: &str,
    generation: u64,
) {
    if generation == 0 {
        return;
    }
    if let Some(history) = inner
        .engram_work_naming_history
        .iter_mut()
        .find(|history| &history.store == store && history.work_id == work_id)
    {
        if history.epoch != ENGRAM_NAMING_HISTORY_EPOCH && generation > history.known_generation {
            history.revision = history
                .revision
                .checked_add(1)
                .expect("naming history revision exhausted");
            history.known_generation = generation;
        }
    } else {
        inner
            .engram_work_naming_history
            .push(EngramWorkNamingHistory {
                store: store.clone(),
                work_id: work_id.to_owned(),
                revision: 1,
                known_generation: generation,
                epoch: 0,
                frontier: None,
                owner_version: 0,
                transition: None,
                unresolved_runs: Vec::new(),
                proofs: Vec::new(),
                retirements: Vec::new(),
                recovery_reason: Some(
                    "legacy naming origin requires canonical recovery or explicit repair"
                        .to_owned(),
                ),
            });
    }
}

/// Why a naming token taken for an acceptance evaluation no longer fences the
/// tree it was taken on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngramWorkNamingChange {
    /// The work's naming history moved past the token in a way not proved to
    /// describe the same binding, or belongs to another authority epoch, or
    /// is gone.
    Moved,
    /// A naming of the work is in flight: an intent awaiting its receipt, a
    /// run awaiting recovery, or a binding awaiting reconciliation.
    Pending,
    /// Only an authority transition of the token's own binding is
    /// unpublished (a reclamation, recovery or admission pass between its
    /// preparation and its publication), with no naming intent, unresolved
    /// run or reconciliation behind it: it will publish the same binding or a
    /// retirement, so the caller tries again once it has, and that
    /// publication decides. Another claim's prepared naming is not this.
    InFlight,
}

/// Whether the only unresolved thing about the work is an authority
/// transition of `bound` itself in flight: Prepared or Candidate, owned by
/// the binding the token was taken on (the same claim, in the same canonical
/// run), every retained run that owner's, acknowledged proofs in the current
/// epoch, and no journal naming the work with an intent or a reconciliation
/// outstanding. A transition another claim or run prepared is a naming of
/// the work by someone else, never this binding's maintenance, so it is not
/// in flight here; nor is any transition for a token with no named binding.
fn engram_work_authority_transition_in_flight(
    inner: &StateInner,
    store: &EngramAuthorityStoreKey,
    work_id: &str,
    bound: Option<&EngramWorkNamingBound>,
) -> bool {
    let Some(bound) = bound else {
        return false;
    };
    let Some(history) = inner
        .engram_work_naming_history
        .iter()
        .find(|history| &history.store == store && history.work_id == work_id)
    else {
        return false;
    };
    history.epoch == ENGRAM_NAMING_HISTORY_EPOCH
        && !history.proofs.is_empty()
        && history.transition.as_ref().is_some_and(|transition| {
            transition.phase != EngramAuthorityPhase::Published
                && transition.binding.claim_id == bound.claim_id
                && transition.binding.run_id == bound.run_id
                && history
                    .unresolved_runs
                    .iter()
                    .all(|run| engram_same_recovery_run(run, &transition.binding))
        })
        && !inner.engram_named_root_journal.iter().any(|journal| {
            &journal.store == store
                && engram_journal_names_work(journal, work_id)
                && (journal.pending.is_some() || journal.requires_reconciliation())
        })
}

/// Whether `token` still fences the work's naming. The history revision
/// decides, strictly, with one proved exception: a token taken on a binding
/// its claim had named (`named_binding`) stays current across exactly one
/// revision step that left that binding's own Bound event, in the same
/// canonical run, as the history's frontier. That event is unique (a
/// generation is never named twice, a same-generation re-name sends no
/// event) and the frontier is the latest event learned, so the step learned
/// no later clear, re-name or other claim's naming, whatever the frontier
/// was before. Root generations are claim-local, so no generation number
/// alone can stand in for the binding. A publication whose durable
/// acknowledgement is outstanding cannot supply a stable history fence,
/// including to a request for an unfocused claim.
fn engram_work_naming_change(
    inner: &StateInner,
    store: &EngramAuthorityStoreKey,
    token: &EngramWorkNamingToken,
) -> Option<EngramWorkNamingChange> {
    let history = inner
        .engram_work_naming_history
        .iter()
        .find(|history| &history.store == store && history.work_id == token.work_id);
    let (revision, epoch) = history.map_or((0, 0), |history| (history.revision, history.epoch));
    let learned_own_binding = || {
        token.named_binding.as_ref().is_some_and(|bound| {
            token.revision.checked_add(1) == Some(revision)
                && history
                    .and_then(|history| history.frontier.as_ref())
                    .is_some_and(|frontier| bound.is_frontier(frontier))
        })
    };
    if epoch != token.epoch || token.epoch != ENGRAM_NAMING_HISTORY_EPOCH {
        return Some(EngramWorkNamingChange::Moved);
    }
    // Before the revision: a pass over this token's own binding between its
    // preparation and its publication is answered as retryable, and the
    // published state decides the retry.
    if engram_work_authority_transition_in_flight(
        inner,
        store,
        &token.work_id,
        token.named_binding.as_ref(),
    ) {
        return Some(EngramWorkNamingChange::InFlight);
    }
    if revision != token.revision && !learned_own_binding() {
        return Some(EngramWorkNamingChange::Moved);
    }
    let pending = engram_authority_work_unresolved(inner, store, &token.work_id)
        || inner.engram_named_root_journal.iter().any(|journal| {
            &journal.store == store
                && journal
                    .pending
                    .as_ref()
                    .is_some_and(|event| event.root.work_id == token.work_id)
        });
    pending.then_some(EngramWorkNamingChange::Pending)
}

#[cfg(test)]
fn engram_work_naming_is_current(
    inner: &StateInner,
    store: &EngramAuthorityStoreKey,
    token: &EngramWorkNamingToken,
) -> bool {
    engram_work_naming_change(inner, store, token).is_none()
}

/// Only settled, unused history is disposable. In particular a capture
/// worker still resolving its basis conservatively protects history until
/// that basis can be inspected without waiting under the state lock.
fn engram_compact_root_journal(inner: &mut StateInner) {
    let retained = inner.engram_named_root_journal.iter().filter(|journal| {
        if inner.engram_work_naming_history.iter().flat_map(|history| &history.retirements)
            // An unacknowledged publication still needs this journal for replay.
            // Published notices carry their own proof and delivery identity.
            .any(|notice| !notice.published && notice.selection.store == journal.store
                && notice.selection.claim_id == journal.claim_id) {
            return true;
        }
        if journal.requires_reconciliation() {
            return true;
        }
        if inner.delegations.iter().any(|delegation| {
            delegation.acceptance_evaluation.as_ref().is_some_and(|target| {
                target.store.as_ref() == Some(&journal.store)
                    && (target.source_claim.as_ref().is_some_and(|claim| claim.claim_id == journal.claim_id
                            || !journal.obsolete && journal.confirmed.as_ref().is_some_and(|(event, _)|
                                event.root.work_id == claim.work_id && event.root.generation > claim.named_generation_at_request.unwrap_or(0)))
                        || target.source_root.as_ref().is_some_and(|root| root.claim_id == journal.claim_id))
                    && (matches!(delegation.status, DelegationStatus::Queued | DelegationStatus::Running)
                        || matches!(target.submission, AcceptanceEvaluationSubmission::Pending { .. }
                            | AcceptanceEvaluationSubmission::Unconfirmed { .. }))
            })
        }) { return true; }
        if journal.confirmed.as_ref().is_some_and(|(event, _)|
            event.kind == EngramNamedRootKind::Bound && !journal.obsolete) {
            return true;
        }
        if inner.engram_work_source_roots.iter().any(|root|
            root.store == journal.store && root.claim_id == journal.claim_id) {
            return true;
        }
        let event = journal.confirmed.as_ref().map(|(event, _)| event);
        let references_basis = |basis: &EngramExecutionSourceBasis| event.is_some_and(|event|
            basis.source_root_generation == i64::try_from(event.root.generation).ok());
        let references_capture = |capture: &EngramBasisCapture| {
            capture.result.try_lock().map_or(true, |result|
                result.as_ref().is_none_or(|basis| basis.as_ref().is_some_and(&references_basis)))
        };
        inner.sessions.iter().any(|record| {
            let runtime = &record.engram;
            runtime.work_binding.as_ref().is_some_and(|binding| binding.claim_id == journal.claim_id)
                || runtime.active_turn_source_root.as_ref().is_some_and(|root|
                    root.claim_id == journal.claim_id)
                || matches!(&runtime.active_turn_root_capture,
                    Some(EngramRootCapture::Recorded { generation, .. })
                    if event.is_some_and(|event| i64::try_from(event.root.generation).ok() == Some(*generation)))
                || runtime.active_turn_start_basis.as_ref().is_some_and(&references_basis)
                || [&runtime.active_turn_report, &runtime.active_turn_report_fallback]
                    .into_iter().flatten().any(|(_, report)| report.observations.iter()
                        .any(|observation| observation.source_basis.as_ref().is_some_and(&references_basis)))
                || runtime.active_turn_checks.iter().any(|check|
                    references_capture(&check.start_basis)
                    || check.end.as_ref().is_some_and(|end| references_capture(&end.end_basis)))
        })
    }).cloned().collect();
    inner.engram_named_root_journal = retained;
}

/// Wire serializers may omit zero fractions or use a different UTC offset.
/// Compare instants without changing any bytes of a durable retry intent.
fn engram_named_at_matches(left: &str, right: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(left),
        chrono::DateTime::parse_from_rfc3339(right),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn engram_root_event_matches_state(
    event: &EngramNamedRootEvent,
    state: Option<&EngramNamedRootState>,
) -> bool {
    matches!(state, Some(EngramNamedRootState::Bound {workspace_id, generation, named_at})
        if *workspace_id == event.root.root && u64::try_from(*generation).ok() == Some(event.root.generation)
            && engram_named_at_matches(named_at, &event.root.named_at))
}

/// Cleanup keeps the original bound identity and audit time. The reporter is
/// selected durably when a live connection to this store becomes available.
fn engram_queue_root_cleanup(
    journals: &mut [EngramNamedRootJournal],
    root: &EngramWorkSourceRoot,
    reason: EngramNamedRootEndReason,
) {
    let Some(journal) = journals
        .iter_mut()
        .find(|journal| journal.store == root.store && journal.claim_id == root.claim_id)
    else {
        return;
    };
    if journal.pending.is_some() {
        return;
    }
    let Some((event, _)) = &journal.confirmed else {
        return;
    };
    if event.kind != EngramNamedRootKind::Bound || event.root.generation != root.generation {
        return;
    }
    if EngramRootIntentAssociation::for_event(event, journal.read_binding.as_ref()).is_err() {
        // A legacy receipt without its run association is recovery state,
        // not permission to create an Ended intent that nobody can settle.
        journal.retire(EngramRootRetirement::AssociationUnconfirmed);
        return;
    }
    journal.pending = Some(EngramNamedRootEvent {
        root: event.root.clone(),
        reporter: String::new(),
        kind: EngramNamedRootKind::Ended,
        end_reason: Some(reason),
    });
}

fn engram_root_journal<'a>(
    journals: &'a [EngramNamedRootJournal],
    store: &EngramAuthorityStoreKey,
    claim_id: &str,
) -> Option<&'a EngramNamedRootJournal> {
    journals
        .iter()
        .find(|journal| &journal.store == store && journal.claim_id == claim_id)
}

fn engram_named_root_read_obsoletes(
    entry: &EngramWorkSourceRoot,
    state: Option<&EngramNamedRootState>,
    read_generation: u64,
) -> bool {
    if entry.generation > read_generation {
        return false;
    }
    match state {
        Some(EngramNamedRootState::None) => true,
        Some(EngramNamedRootState::UnboundByRelease {
            last_generation, ..
        }) => {
            u64::try_from(*last_generation).is_ok_and(|generation| generation >= entry.generation)
        }
        Some(EngramNamedRootState::Bound {
            workspace_id,
            generation,
            named_at,
        }) => {
            u64::try_from(*generation).is_ok_and(|generation| generation >= entry.generation)
                && (*workspace_id != entry.root
                    || u64::try_from(*generation).ok() != Some(entry.generation)
                    || !engram_named_at_matches(named_at, &entry.named_at))
        }
        _ => false,
    }
}

/// A capture's provenance is decided before its filesystem read, and checked
/// again afterward. Pending transport never creates creditable evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
enum EngramRootCapture {
    Unnamed,
    Recorded {
        generation: i64,
        state: EngramSourceRootState,
        workspace_id: String,
    },
    Unconfirmed,
}

impl EngramRootCapture {
    fn stamp(
        &self,
        basis: Option<EngramExecutionSourceBasis>,
    ) -> Option<EngramExecutionSourceBasis> {
        let mut basis = basis?;
        match self {
            Self::Unnamed => {}
            Self::Recorded {
                generation,
                state,
                workspace_id,
            } => {
                if *state == EngramSourceRootState::Named && basis.workspace_id != *workspace_id {
                    return None;
                }
                basis.source_root_generation = Some(*generation);
                basis.source_root_state = Some(*state);
            }
            Self::Unconfirmed => return None,
        }
        Some(basis)
    }
}

fn engram_root_capture_locked(inner: &StateInner, session_id: &str) -> EngramRootCapture {
    let Some(index) = inner.find_session_index(session_id) else {
        return EngramRootCapture::Unconfirmed;
    };
    let record = &inner.sessions[index];
    let Some(binding) = record.engram.work_binding.as_ref() else {
        return EngramRootCapture::Unnamed;
    };
    let Some(store) = engram_project_for_session_locked(inner, session_id)
        .and_then(|p| p.engram.as_ref())
        .and_then(|s| s.authority_store_key.as_ref())
    else {
        return EngramRootCapture::Unconfirmed;
    };
    if engram_authority_work_unresolved(inner, store, &binding.work_id) {
        return EngramRootCapture::Unconfirmed;
    }
    if !inner.engram_work_naming_history.iter().any(|history| {
        &history.store == store
            && history.work_id == binding.work_id
            && history
                .proofs
                .iter()
                .any(|proof| engram_same_recovery_run(&proof.binding, binding))
    }) {
        return EngramRootCapture::Unconfirmed;
    }
    if matches!(
        record.engram.named_root,
        Some(EngramNamedRootState::Unknown)
    ) {
        return EngramRootCapture::Unconfirmed;
    }
    let journal = engram_root_journal(&inner.engram_named_root_journal, store, &binding.claim_id);
    if journal.is_some_and(EngramNamedRootJournal::requires_reconciliation) {
        return EngramRootCapture::Unconfirmed;
    }
    if let Some(EngramNamedRootState::Bound {
        generation,
        workspace_id,
        named_at,
    }) = &record.engram.named_root
    {
        let selected = engram_work_source_root_for_claim(
            &inner.engram_work_source_roots,
            store,
            &binding.work_id,
            &binding.claim_id,
        );
        return if *generation > 0
            && selected.is_some_and(|root| {
                root.root == *workspace_id
                    && root.generation == *generation as u64
                    && engram_named_at_matches(&root.named_at, named_at)
            }) {
            EngramRootCapture::Recorded {
                generation: *generation,
                state: EngramSourceRootState::Named,
                workspace_id: workspace_id.clone(),
            }
        } else {
            EngramRootCapture::Unconfirmed
        };
    }
    if let Some(journal) = journal {
        if let Some((event, _)) = &journal.confirmed {
            if event.kind == EngramNamedRootKind::Bound
                && matches!(
                    record.engram.named_root,
                    Some(
                        EngramNamedRootState::None | EngramNamedRootState::UnboundByRelease { .. }
                    )
                )
                && engram_work_source_root_for_claim(
                    &inner.engram_work_source_roots,
                    store,
                    &binding.work_id,
                    &binding.claim_id,
                )
                .is_none()
            {
                // A lifecycle end not reported by this host has no host
                // ended event to cite. It is an ordinary unbound capture.
                return EngramRootCapture::Unnamed;
            }
            if journal.obsolete && event.kind == EngramNamedRootKind::Bound {
                return EngramRootCapture::Unconfirmed;
            }
            return EngramRootCapture::Recorded {
                generation: event.root.generation as i64,
                workspace_id: event.root.root.clone(),
                state: match event.kind {
                    EngramNamedRootKind::Bound => EngramSourceRootState::Named,
                    EngramNamedRootKind::Ended => EngramSourceRootState::Ended,
                },
            };
        }
    }
    // An older host's local-only entry has no event to cite. Its path is
    // retained for repair, but it cannot mint evidence as a confirmed root.
    match &record.engram.named_root {
        Some(EngramNamedRootState::Unknown) => EngramRootCapture::Unconfirmed,
        _ if engram_work_source_root_for_claim(
            &inner.engram_work_source_roots,
            store,
            &binding.work_id,
            &binding.claim_id,
        )
        .is_some() =>
        {
            EngramRootCapture::Unconfirmed
        }
        _ => EngramRootCapture::Unnamed,
    }
}

/// An admitted turn keeps the generation and workspace it began with. A
/// naming request changes the next turn; it must not relabel earlier checks
/// or the closing sighting. Pending/unknown authority still withholds credit.
fn engram_turn_root_capture_locked(inner: &StateInner, session_id: &str) -> EngramRootCapture {
    let current = engram_root_capture_locked(inner, session_id);
    let Some(index) = inner.find_session_index(session_id) else {
        return current;
    };
    let record = &inner.sessions[index].engram;
    if record.active_grant_id.is_some()
        && record.active_turn_root_capture.is_some()
        && record.active_turn_naming_identity.is_none()
    {
        return EngramRootCapture::Unconfirmed;
    }
    // Immutable admitted provenance has its own store/work identity. A focus
    // change must not let an unsettled earlier work release checkpoint credit.
    if let Some((store, work)) = &record.active_turn_naming_identity {
        if engram_authority_work_unresolved(inner, store, work) {
            return EngramRootCapture::Unconfirmed;
        }
    }
    if current == EngramRootCapture::Unconfirmed {
        // Retirement denies future admission, not an already admitted
        // confirmed snapshot. Pending or unknown authority still withholds it.
        let store = engram_project_for_session_locked(inner, session_id)
            .and_then(|project| project.engram.as_ref())
            .and_then(|settings| settings.authority_store_key.as_ref());
        let known_retirement = record
            .active_turn_source_root
            .as_ref()
            .zip(store)
            .is_some_and(|(root, store)| {
                engram_root_journal(&inner.engram_named_root_journal, store, &root.claim_id)
                    .is_some_and(|journal| {
                        journal.obsolete
                            && journal.retirement.is_some()
                            && journal.pending.is_none()
                            && journal.reconciliation.is_none()
                    })
            });
        if !known_retirement || record.named_root == Some(EngramNamedRootState::Unknown) {
            return current;
        }
    }
    if record.active_grant_id.is_none() {
        return current;
    }
    if record.active_turn_root_capture == Some(EngramRootCapture::Unconfirmed) {
        return EngramRootCapture::Unconfirmed;
    }
    if let Some(root) = &record.active_turn_source_root {
        return EngramRootCapture::Recorded {
            generation: root.generation as i64,
            state: EngramSourceRootState::Named,
            workspace_id: root.root.clone(),
        };
    }
    if let Some(provenance) = &record.active_turn_root_capture {
        return provenance.clone();
    }
    match record.active_turn_start_basis.as_ref() {
        Some(basis) => match (basis.source_root_generation, basis.source_root_state) {
            (Some(generation), Some(state)) => EngramRootCapture::Recorded {
                generation,
                state,
                workspace_id: basis.workspace_id.clone(),
            },
            _ => EngramRootCapture::Unnamed,
        },
        None => EngramRootCapture::Unconfirmed,
    }
}

fn engram_removed_root_read_now(clock: &EngramBudgetClock) -> std::time::Instant {
    #[cfg(test)]
    if matches!(clock, EngramBudgetClock::Scripted(_)) {
        return clock.now();
    }
    #[cfg(test)]
    if let Some(now) = TEST_ENGRAM_REMOVED_ROOT_READ_CLOCK
        .with(|clock| clock.borrow_mut().as_mut().map(|clock| clock()))
    {
        return now;
    }
    clock.now()
}

impl AppState {
    /// Bounded maintenance for locally removed naming sessions. Unsupported
    /// readers and unresolved remote intents retain their refusal guards.
    fn resolve_removed_engram_roots(
        &self,
        session_id: &str,
        claim: Option<&str>,
        budget: Duration,
    ) {
        let clock = self.engram_budget_clock();
        let deadline = engram_removed_root_read_now(&clock) + budget.min(Duration::from_secs(2));
        if claim.is_none() {
            self.recover_engram_authority_runs(
                session_id,
                deadline.saturating_duration_since(engram_removed_root_read_now(&clock)),
            );
        }
        let (target, journals) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(target) =
                Self::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
                    .ok()
                    .flatten()
            else {
                return;
            };
            let Some(store) = target.settings.authority_store_key.as_ref() else {
                return;
            };
            if target.routing_token.is_none() {
                return;
            }
            let mut journals = inner
                .engram_named_root_journal
                .iter()
                .filter(|journal| {
                    &journal.store == store
                        && journal.retirement == Some(EngramRootRetirement::NamingSessionRemoved)
                        && journal.pending.is_none()
                        && journal.reconciliation.is_none()
                        && journal.read_binding.is_some()
                        && journal.confirmed.is_some()
                        && claim.is_none_or(|claim| journal.claim_id == claim)
                })
                .map(|journal| {
                    (
                        journal.clone(),
                        engram_work_source_root_for_work(
                            &inner.engram_work_source_roots,
                            store,
                            &journal
                                .confirmed
                                .as_ref()
                                .expect("retained history")
                                .0
                                .root
                                .work_id,
                        )
                        .cloned(),
                    )
                })
                .collect::<Vec<_>>();
            if !journals.is_empty() && claim.is_none() {
                journals.sort_by(|a, b| a.0.claim_id.cmp(&b.0.claim_id));
                let offset = inner.engram_root_read_cursor.get(store).map_or(0, |last| {
                    journals.partition_point(|(journal, _)| journal.claim_id <= *last)
                        % journals.len()
                });
                journals.rotate_left(offset);
            }
            journals.truncate(8);
            (target, journals)
        };
        for (before, selected_before) in journals {
            let budget = deadline.saturating_duration_since(engram_removed_root_read_now(&clock));
            if budget.is_zero() {
                break;
            }
            // Advance only for an attempted read, even when it uses the whole
            // budget. Stable claim identity survives removal and filtering;
            // requested-claim reads cannot reset the store's sweep position.
            if claim.is_none() {
                self.inner
                    .lock()
                    .expect("state mutex poisoned")
                    .engram_root_read_cursor
                    .insert(before.store.clone(), before.claim_id.clone());
            }
            let binding = before
                .read_binding
                .as_ref()
                .expect("selected read identity");
            let Ok(owner) = self.prepare_engram_authority_until(&before.store, binding, deadline)
            else {
                continue;
            };
            let budget = deadline.saturating_duration_since(clock.now());
            if budget.is_zero() {
                break;
            }
            let response = target
                .adapter
                .request(
                    &target.connection,
                    &EngramControlRequest::NamedRootRead {
                        routing_token: target.routing_token.clone().expect("bound reader"),
                        run_id: binding.run_id.clone(),
                        claim_id: binding.claim_id.clone(),
                    },
                    budget.min(target.settings.call_timeout()),
                )
                .and_then(parse_engram_result::<EngramNamedRootReadResponse>);
            let Ok(response) = response else {
                continue;
            };
            if response.validate_authority(&before.store, binding).is_err()
                || !response.covers(&before)
            {
                continue;
            }
            #[cfg(test)]
            if let Some(during) =
                TEST_ENGRAM_DURING_ROOT_LIFECYCLE_READ.with(|hook| hook.borrow_mut().take())
            {
                during();
            }
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let current =
                Self::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
                    .ok()
                    .flatten();
            if !current.is_some_and(|current| {
                current.connection == target.connection
                    && current.routing_token == target.routing_token
                    && current.settings.authority_store_key.as_ref() == Some(&before.store)
            }) {
                continue;
            }
            let Some(index) = inner
                .engram_named_root_journal
                .iter()
                .position(|journal| journal == &before)
            else {
                continue;
            };
            if engram_work_source_root_for_work(
                &inner.engram_work_source_roots,
                &before.store,
                &binding.work_id,
            ) != selected_before.as_ref()
            {
                continue;
            }
            if Self::learn_engram_authority_locked(&mut inner, &before.store, &owner, &response)
                .is_err()
            {
                continue;
            }
            if response.definitively_unbound() {
                inner.engram_named_root_journal[index].retire(EngramRootRetirement::Unbound);
            }
            engram_compact_root_journal(&mut inner);
            drop(inner);
            if self
                .publish_engram_authority_until(&before.store, &owner, deadline)
                .is_err()
            {
                continue;
            }
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if response.definitively_unbound() {
                let same_store_sessions = inner
                    .sessions
                    .iter()
                    .filter_map(|record| {
                        Self::engram_binding_target_for_session_shape_locked(
                            &inner,
                            &record.session.id,
                            true,
                        )
                        .ok()
                        .flatten()
                        .filter(|target| {
                            target.settings.authority_store_key.as_ref() == Some(&before.store)
                        })
                        .map(|_| record.session.id.clone())
                    })
                    .collect::<Vec<_>>();
                for record in &mut inner.sessions {
                    if record.engram.work_binding.as_ref().is_some_and(|bound| {
                        bound.claim_id == response.claim_id && bound.run_id == response.run_id
                    }) && same_store_sessions.contains(&record.session.id)
                    {
                        record.engram.named_root = Some(response.named_root.clone());
                    }
                }
            }
        }
    }
    // Reads the narrow reader view (engram_session_authority.rs): this runs
    // under the global lock for every pending journal intent, and a full
    // target would clone every root, journal and project store.
    fn engram_root_reporter_available(inner: &StateInner, event: &EngramNamedRootEvent) -> bool {
        engram_session_reader_view_locked(inner, &event.reporter).is_some_and(|view| {
            view.store == Some(&event.root.store) && view.routing_token.is_some()
        })
    }

    fn engram_orphaned_root_intent(
        &self,
        store: &EngramAuthorityStoreKey,
        claim_id: &str,
    ) -> Option<EngramNamedRootEvent> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        engram_root_journal(&inner.engram_named_root_journal, store, claim_id)
            .and_then(|journal| journal.pending.as_ref())
            .filter(|event| {
                !event.reporter.is_empty() && !Self::engram_root_reporter_available(&inner, event)
            })
            .cloned()
    }

    /// A queued delta is not durable. This fence names one claim's exact
    /// journal and selection, so unrelated sessions cannot prevent its ack.
    /// Never wait while holding StateInner: the writer needs that lock.
    /// Begin receipts are replayed as originally stored. When one disagrees
    /// with a local selection, a fresh status read decides; the receipt must
    /// not erase a name made after that begin was first issued.
    #[cfg(test)]
    fn reconcile_engram_begin_root(
        &self,
        target: &EngramBindingTarget,
        routing_token: &str,
        binding: Option<&EngramControlWorkBinding>,
        state: Option<EngramNamedRootState>,
        budget: Duration,
    ) -> Result<(), EngramTransportError> {
        let clock = self.engram_budget_clock();
        self.reconcile_engram_begin_root_until(
            target,
            routing_token,
            binding,
            state,
            clock.now() + budget,
            None,
            None,
        )
        .map(|_| ())
    }

    fn reconcile_engram_begin_root_until(
        &self,
        target: &EngramBindingTarget,
        routing_token: &str,
        binding: Option<&EngramControlWorkBinding>,
        mut state: Option<EngramNamedRootState>,
        deadline: std::time::Instant,
        mut guard: Option<EngramAcknowledgedRootGuard>,
        admission_owner: Option<&EngramQueuedAdmissionOwner>,
    ) -> Result<EngramRootReconcileOutcome, EngramTransportError> {
        let clock = self.engram_budget_clock();
        let (read_generation, needs_status) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let entry = target
                .settings
                .authority_store_key
                .as_ref()
                .zip(binding)
                .and_then(|(store, binding)| {
                    engram_work_source_root_for_claim(
                        &inner.engram_work_source_roots,
                        store,
                        &binding.work_id,
                        &binding.claim_id,
                    )
                });
            let needs_status = entry.is_some_and(|entry| !matches!(&state,
                Some(EngramNamedRootState::Bound {workspace_id, generation, named_at})
                if *workspace_id == entry.root && u64::try_from(*generation).ok() == Some(entry.generation) && engram_named_at_matches(named_at, &entry.named_at)));
            (EngramRootReadFence::capture(&inner), needs_status)
        };
        if needs_status {
            if clock.now() >= deadline {
                state = Some(EngramNamedRootState::Unknown);
            } else {
                if guard.as_ref().is_none_or(|guard| {
                    !guard.still_withholding(&self.inner.lock().expect("state mutex poisoned"))
                }) {
                    guard = self.guard_engram_root_read_until(target, binding, deadline)?;
                }
                state = match target
                    .adapter
                    .request(
                        &target.connection,
                        &EngramControlRequest::SessionStatus {
                            routing_token: routing_token.to_owned(),
                        },
                        target.rpc_timeout_until(deadline)?,
                    )
                    .and_then(parse_engram_result::<EngramSessionStatusResponse>)
                {
                    Ok(status) => status.named_root,
                    Err(error) if engram_status_error_requires_fresh_bind(&error) => {
                        return Err(error);
                    }
                    Err(_) => Some(EngramNamedRootState::Unknown),
                };
            }
        }
        self.reconcile_engram_root_for_admission_until(
            target,
            routing_token,
            binding,
            state,
            &read_generation,
            guard,
            admission_owner,
            deadline,
        )
    }

    fn retire_engram_root_replay(
        &self,
        event: &EngramNamedRootEvent,
        receipt: &EngramNamedRootReceipt,
        cleanup: Option<EngramNamedRootEndReason>,
    ) -> Result<(), ApiError> {
        self.retire_engram_root_replay_checked(
            event,
            receipt,
            cleanup,
            EngramRootRetirement::Unbound,
            None,
        )
    }

    fn retire_engram_root_replay_checked(
        &self,
        event: &EngramNamedRootEvent,
        receipt: &EngramNamedRootReceipt,
        cleanup: Option<EngramNamedRootEndReason>,
        retirement: EngramRootRetirement,
        read: Option<(
            &EngramBindingTarget,
            &EngramRootReadFence,
            &EngramNamedRootState,
        )>,
    ) -> Result<(), ApiError> {
        let clock = self.engram_budget_clock();
        let (target, binding) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let binding = engram_root_journal(
                &inner.engram_named_root_journal,
                &event.root.store,
                &event.root.claim_id,
            )
            .and_then(|journal| journal.read_binding.clone())
            .ok_or_else(|| {
                ApiError::conflict("replay retirement has no canonical run association")
            })?;
            let target = read
                .map(|(target, _, _)| target.clone())
                .or_else(|| {
                    Self::engram_binding_target_for_session_shape_locked(
                        &inner,
                        &event.reporter,
                        true,
                    )
                    .ok()
                    .flatten()
                })
                .ok_or_else(|| ApiError::conflict("replay retirement has no reader connection"))?;
            (target, binding)
        };
        let deadline = clock.now() + target.settings.call_timeout();
        let owner = self.prepare_engram_authority_until(&event.root.store, &binding, deadline)?;
        let proof = self.read_engram_authority_fact_until(&target, &binding, deadline)?;
        if proof.read_cut.position < receipt.position.position {
            return Err(ApiError::conflict(
                "replay retirement is not covered by canonical history",
            ));
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if let Some((target, fence, _)) = read {
            let current = Self::engram_binding_target_for_session_shape_locked(
                &inner,
                &target.connection.session_id,
                true,
            )
            .ok()
            .flatten();
            if !fence.same_store(&inner, &target.project_id)
                || current.as_ref().is_none_or(|current| {
                    current.connection != target.connection
                        || current.routing_token != target.routing_token
                        || current.work_binding != target.work_binding
                        || current.settings.authority_store_key
                            != target.settings.authority_store_key
                })
                || target.work_binding.as_ref().is_none_or(|binding| {
                    binding.claim_id != event.root.claim_id
                        || !fence.still_current(&inner, &event.root.store, binding)
                })
            {
                return Err(ApiError::conflict(
                    "named-root authority changed during replay readback",
                ));
            }
        }
        let Some(index) = inner
            .engram_named_root_journal
            .iter()
            .position(|journal| journal.pending.as_ref() == Some(event))
        else {
            return Err(ApiError::conflict(
                "pending root changed during lifecycle readback",
            ));
        };
        Self::learn_engram_authority_locked(&mut inner, &event.root.store, &owner, &proof)?;
        inner.engram_named_root_journal[index].confirmed = Some((event.clone(), receipt.clone()));
        inner.engram_named_root_journal[index].pending = None;
        inner.engram_named_root_journal[index].retire(retirement);
        if let Some((target, _, state)) = read {
            if let Some(session) = inner.find_session_index(&target.connection.session_id) {
                inner.sessions[session].engram.named_root = Some(state.clone());
            }
            let generation = match state {
                EngramNamedRootState::Bound { generation, .. } => *generation,
                EngramNamedRootState::UnboundByRelease {
                    last_generation, ..
                } => *last_generation,
                _ => 0,
            };
            inner.engram_source_root_generation = inner
                .engram_source_root_generation
                .max(u64::try_from(generation).unwrap_or(0));
        }
        inner
            .engram_work_source_roots
            .retain(|root| root != &event.root);
        if let Some(reason) = cleanup {
            engram_queue_root_cleanup(&mut inner.engram_named_root_journal, &event.root, reason);
        }
        drop(inner);
        self.publish_engram_authority_until(&event.root.store, &owner, deadline)
    }
    /// At most one cleanup per admitted turn, sharing the source-capture
    /// budget. It never grows admission's bind path or holds the state lock
    /// across Engram I/O. A failed send remains in the durable journal.
    fn flush_engram_root_cleanup(&self, session_id: &str, budget: Duration) {
        let clock = self.engram_budget_clock();
        let deadline = clock.now() + budget;
        let prepared = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(target) =
                Self::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
                    .ok()
                    .flatten()
            else {
                return;
            };
            let Some(store) = &target.settings.authority_store_key else {
                return;
            };
            let Some(index) = inner.engram_named_root_journal.iter().position(|journal| {
                &journal.store == store
                    && journal.pending.as_ref().is_some_and(|event| {
                        event.kind == EngramNamedRootKind::Ended
                            && event.end_reason != Some(EngramNamedRootEndReason::ExplicitClear)
                            && (event.reporter.is_empty() || event.reporter == session_id)
                    })
            }) else {
                return;
            };
            let event = inner.engram_named_root_journal[index]
                .pending
                .as_mut()
                .unwrap();
            event.reporter = session_id.to_owned();
            let event = event.clone();
            // Reporter assignment is part of the subsequent Prepared image.
            // No synchronous writer fallback is allowed under StateInner.
            (target, event)
        };
        let (target, event) = prepared;
        let Ok(reply) = self.send_engram_root_event_until(&target, &event, deadline) else {
            return;
        };
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner
            .engram_named_root_journal
            .iter()
            .position(|journal| journal.pending.as_ref() == Some(&event))
        else {
            return;
        };
        if Self::learn_engram_authority_locked(
            &mut inner,
            &event.root.store,
            &reply.owner,
            &reply.proof,
        )
        .is_err()
        {
            return;
        }
        inner.engram_named_root_journal[index].confirmed = Some((event.clone(), reply.receipt));
        inner.engram_named_root_journal[index].pending = None;
        inner.engram_named_root_journal[index].obsolete = false;
        inner.engram_named_root_journal[index].retirement = None;
        inner
            .engram_work_source_roots
            .retain(|root| root != &event.root);
        drop(inner);
        if self
            .publish_engram_authority_until(&event.root.store, &reply.owner, deadline)
            .is_err()
        {
            return;
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if let Some(index) = inner.find_session_index(session_id) {
            if inner.sessions[index]
                .engram
                .work_binding
                .as_ref()
                .is_some_and(|binding| binding.claim_id == event.root.claim_id)
            {
                inner.sessions[index].engram.named_root = Some(EngramNamedRootState::None);
                inner.sessions[index].engram.set_pending_source_root_line(
                    "[TermAl] The previous named source root is unavailable and its binding has ended. Name a valid worktree before editing or testing there.".to_owned());
            }
        }
    }

    /// Stage only. The owner acknowledges this whole image off-lock before
    /// transport; failure retains the exact intent, never a new timestamp.
    fn stage_engram_root_event_locked(
        &self,
        inner: &mut StateInner,
        event: &EngramNamedRootEvent,
        read_binding: Option<&EngramControlWorkBinding>,
    ) -> Result<(), ApiError> {
        let retained = engram_root_journal(
            &inner.engram_named_root_journal,
            &event.root.store,
            &event.root.claim_id,
        )
        .and_then(|journal| journal.read_binding.as_ref());
        let association = EngramRootIntentAssociation::validate(
            &event.root.store,
            &event.root.work_id,
            &event.root.claim_id,
            retained.is_none().then_some(event.root.claim_fence),
            retained.or(read_binding),
        )?;
        if let Some(supplied) = read_binding {
            EngramRootIntentAssociation::for_event(event, Some(supplied))?;
            if !engram_same_recovery_run(&association.binding, supplied) {
                return Err(ApiError::conflict(
                    "naming cannot replace a retained intent's canonical run association",
                ));
            }
        }
        engram_compact_root_journal(inner);
        let existing = inner.engram_named_root_journal.iter().position(|journal| {
            journal.store == event.root.store && journal.claim_id == event.root.claim_id
        });
        let index = match existing {
            Some(index) => index,
            None => {
                if inner.engram_named_root_journal.len() >= ENGRAM_NAMED_ROOT_JOURNAL_LIMIT {
                    return Err(ApiError::conflict(
                        "named-root journal is full of pending, unresolved or referenced transitions; settle pending names or obtain authoritative lifecycle reads for retired claims; no protected transition was discarded",
                    ));
                }
                inner
                    .engram_named_root_journal
                    .push(EngramNamedRootJournal {
                        store: event.root.store.clone(),
                        claim_id: event.root.claim_id.clone(),
                        read_binding: None,
                        confirmed: None,
                        pending: None,
                        obsolete: false,
                        retirement: None,
                        reconciliation: None,
                    });
                inner.engram_named_root_journal.len() - 1
            }
        };
        let journal = &mut inner.engram_named_root_journal[index];
        if journal
            .pending
            .as_ref()
            .is_some_and(|pending| pending != event)
        {
            return Err(ApiError::conflict(
                "an earlier named-root transition has an unknown outcome; retry that naming request first",
            ));
        }
        if journal.read_binding.is_none() {
            journal.read_binding = Some(association.binding);
        }
        journal.pending = Some(event.clone());
        Ok(())
    }

    fn send_engram_root_event(
        &self,
        target: &EngramBindingTarget,
        event: &EngramNamedRootEvent,
        budget: Duration,
    ) -> Result<EngramRootEventReply, ApiError> {
        let clock = self.engram_budget_clock();
        self.send_engram_root_event_until(target, event, clock.now() + budget)
    }

    /// Prepare the authority image for sending `event`. Nothing has been sent
    /// yet, so while the claim's run association is still `binding` and
    /// `event` is still its pending intent, a supersession by another owner
    /// prepares a fresh image instead of refusing the naming
    /// (`prepare_engram_authority_retrying_supersession_until`).
    fn prepare_engram_root_event_authority_until(
        &self,
        event: &EngramNamedRootEvent,
        binding: &EngramControlWorkBinding,
        deadline: std::time::Instant,
    ) -> Result<EngramAuthorityTransition, ApiError> {
        self.prepare_engram_authority_retrying_supersession_until(
            &event.root.store,
            binding,
            deadline,
            |inner| {
                engram_root_journal(
                    &inner.engram_named_root_journal,
                    &event.root.store,
                    &event.root.claim_id,
                )
                .is_some_and(|journal| {
                    journal.read_binding.as_ref() == Some(binding)
                        && journal.pending.as_ref() == Some(event)
                })
            },
        )
    }

    fn send_engram_root_event_until(
        &self,
        target: &EngramBindingTarget,
        event: &EngramNamedRootEvent,
        deadline: std::time::Instant,
    ) -> Result<EngramRootEventReply, ApiError> {
        let clock = self.engram_budget_clock();
        let binding = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            engram_root_journal(
                &inner.engram_named_root_journal,
                &event.root.store,
                &event.root.claim_id,
            )
            .and_then(|journal| journal.read_binding.clone())
            .ok_or_else(|| {
                ApiError::conflict("naming requires the claim's canonical run association")
            })?
        };
        let owner = self.prepare_engram_root_event_authority_until(event, &binding, deadline)?;
        let original_reporter;
        let target = if target.connection.session_id == event.reporter {
            target
        } else {
            let inner = self.inner.lock().expect("state mutex poisoned");
            original_reporter = Self::engram_binding_target_for_session_shape_locked(&inner, &event.reporter, true)
                .ok().flatten().filter(|target| target.settings.authority_store_key.as_ref() == Some(&event.root.store))
                .ok_or_else(|| ApiError::conflict("the pending named-root event needs its original reporting session's connection"))?;
            &original_reporter
        };
        let token = target.routing_token.as_deref().ok_or_else(|| {
            ApiError::conflict("Engram session is not bound; retry naming after its next admission")
        })?;
        if clock.now() >= deadline {
            return Err(ApiError::bad_gateway(
                "named-root transition is pending; its naming budget expired before transport",
            ));
        }
        let request = event
            .request(token)
            .map_err(|e| ApiError::conflict(e.to_string()))?;
        let result = target
            .adapter
            .request(
                &target.connection,
                &request,
                target
                    .rpc_timeout_until(deadline)
                    .map_err(|error| ApiError::bad_gateway(error.to_string()))?,
            )
            .and_then(parse_engram_result::<EngramNamedRootReceipt>);
        let receipt = match result {
            Ok(receipt) => receipt,
            Err(error) => {
                // A definitive lifecycle refusal has no event to retry. A
                // timeout, malformed receipt or persistence failure does.
                if error.code.as_deref() == Some("named_root_binding_refused") {
                    let proof =
                        self.read_engram_authority_fact_until(target, &binding, deadline)?;
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    Self::learn_engram_authority_locked(
                        &mut inner,
                        &event.root.store,
                        &owner,
                        &proof,
                    )?;
                    if let Some(index) = inner
                        .engram_named_root_journal
                        .iter()
                        .position(|journal| journal.pending.as_ref() == Some(event))
                    {
                        inner.engram_named_root_journal[index].pending = None;
                        if proof.definitively_unbound() {
                            inner.engram_named_root_journal[index]
                                .retire(EngramRootRetirement::Unbound);
                            inner
                                .engram_work_source_roots
                                .retain(|root| root != &event.root);
                        }
                    }
                    drop(inner);
                    self.publish_engram_authority_until(&event.root.store, &owner, deadline)?;
                    return Err(ApiError::conflict(format!(
                        "named_root_binding_refused: {error}"
                    )));
                }
                return Err(ApiError::bad_gateway(format!(
                    "named_root_bind did not confirm the transition ({}): {error}. The exact intent is retained; retry the same naming request.",
                    error.code.as_deref().unwrap_or("unknown")
                )));
            }
        };
        let expected_run = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            engram_root_journal(
                &inner.engram_named_root_journal,
                &event.root.store,
                &event.root.claim_id,
            )
            .and_then(|journal| journal.read_binding.as_ref())
            .map(|binding| binding.run_id.clone())
        };
        if !event.matches_receipt(&receipt)
            || expected_run.is_some_and(|run| receipt.position.feed.id != run)
        {
            return Err(ApiError::bad_gateway(
                "named_root_bind returned a mismatched receipt; the transition remains pending",
            ));
        }
        let proof = self.read_engram_authority_fact(
            target,
            &binding,
            deadline.saturating_duration_since(clock.now()),
        )?;
        {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let mut covered = engram_root_journal(
                &inner.engram_named_root_journal,
                &event.root.store,
                &event.root.claim_id,
            )
            .cloned()
            .ok_or_else(|| {
                ApiError::conflict("named-root journal disappeared before canonical proof")
            })?;
            covered.confirmed = Some((event.clone(), receipt.clone()));
            if !proof.covers(&covered) {
                return Err(ApiError::bad_gateway(
                    "named-root receipt is not covered by the canonical read",
                ));
            }
        }
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            Self::learn_engram_authority_locked(&mut inner, &event.root.store, &owner, &proof)?;
        }
        let active = match event.kind {
            EngramNamedRootKind::Bound => {
                engram_root_event_matches_state(event, Some(&proof.named_root))
            }
            EngramNamedRootKind::Ended => proof.definitively_unbound(),
        };
        if !active {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let journal = inner
                .engram_named_root_journal
                .iter_mut()
                .find(|journal| {
                    journal.store == event.root.store
                        && journal.claim_id == event.root.claim_id
                        && journal.pending.as_ref() == Some(event)
                })
                .ok_or_else(|| {
                    ApiError::conflict("named-root intent changed before stale receipt settlement")
                })?;
            journal.confirmed = Some((event.clone(), receipt.clone()));
            journal.pending = None;
            journal.retire(engram_root_retirement_for_state(&proof.named_root));
            inner.engram_work_source_roots.retain(|root| {
                root.store != event.root.store || root.claim_id != event.root.claim_id
            });
            drop(inner);
            self.publish_engram_authority_until(&event.root.store, &owner, deadline)?;
            return Err(ApiError::conflict(
                "the receipt is covered but it is no longer an active named root; name a fresh generation",
            ));
        }
        Ok(EngramRootEventReply {
            receipt,
            owner,
            proof,
        })
    }

    #[cfg(test)]
    fn engram_root_capture(&self, session_id: &str) -> EngramRootCapture {
        engram_root_capture_locked(
            &self.inner.lock().expect("state mutex poisoned"),
            session_id,
        )
    }

    fn engram_turn_root_capture(&self, session_id: &str) -> EngramRootCapture {
        engram_turn_root_capture_locked(
            &self.inner.lock().expect("state mutex poisoned"),
            session_id,
        )
    }

    /// Reads already returned by Engram govern local selection. A fence or
    /// missing held-claims row never stands in for a release. Absence and a
    /// future variant carry no lifecycle conclusion.
    #[cfg(test)]
    fn reconcile_engram_named_root(
        &self,
        session_id: &str,
        expected_token: &str,
        binding: Option<&EngramControlWorkBinding>,
        state: Option<EngramNamedRootState>,
        read_generation: u64,
    ) -> Result<(), EngramTransportError> {
        let mut fence =
            EngramRootReadFence::capture(&self.inner.lock().expect("state mutex poisoned"));
        fence.generation = read_generation;
        self.reconcile_engram_named_root_with_fence(
            session_id,
            expected_token,
            binding,
            state,
            &fence,
            None,
        )
    }

    #[cfg(test)]
    fn reconcile_engram_named_root_with_fence(
        &self,
        session_id: &str,
        expected_token: &str,
        binding: Option<&EngramControlWorkBinding>,
        state: Option<EngramNamedRootState>,
        fence: &EngramRootReadFence,
        expected_connection: Option<&EngramConnectionConfig>,
    ) -> Result<(), EngramTransportError> {
        let budget = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            Self::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
                .ok()
                .flatten()
                .map(|target| {
                    target
                        .test_dispatch_budget
                        .unwrap_or_else(|| target.settings.call_timeout())
                })
                .unwrap_or(Duration::ZERO)
        };
        self.reconcile_engram_named_root_with_fence_within(
            session_id,
            expected_token,
            binding,
            state,
            fence,
            expected_connection,
            budget,
        )
    }

    #[cfg(test)]
    fn reconcile_engram_named_root_with_fence_within(
        &self,
        session_id: &str,
        expected_token: &str,
        binding: Option<&EngramControlWorkBinding>,
        state: Option<EngramNamedRootState>,
        fence: &EngramRootReadFence,
        expected_connection: Option<&EngramConnectionConfig>,
        budget: Duration,
    ) -> Result<(), EngramTransportError> {
        let clock = self.engram_budget_clock();
        self.reconcile_engram_named_root_with_fence_until(
            session_id,
            expected_token,
            binding,
            state,
            fence,
            expected_connection,
            clock.now() + budget,
        )
    }

    fn reconcile_engram_named_root_with_fence_until(
        &self,
        session_id: &str,
        expected_token: &str,
        binding: Option<&EngramControlWorkBinding>,
        state: Option<EngramNamedRootState>,
        fence: &EngramRootReadFence,
        expected_connection: Option<&EngramConnectionConfig>,
        deadline: std::time::Instant,
    ) -> Result<(), EngramTransportError> {
        self.reconcile_engram_named_root_with_fence_classified_until(
            session_id,
            expected_token,
            binding,
            state,
            fence,
            expected_connection,
            deadline,
        )
        .map_err(EngramRootReconcileFailure::into_transport)
    }

    fn reconcile_engram_named_root_with_fence_classified_until(
        &self,
        session_id: &str,
        expected_token: &str,
        binding: Option<&EngramControlWorkBinding>,
        state: Option<EngramNamedRootState>,
        fence: &EngramRootReadFence,
        expected_connection: Option<&EngramConnectionConfig>,
        deadline: std::time::Instant,
    ) -> Result<(), EngramRootReconcileFailure> {
        let outcome = self.reconcile_engram_named_root_classified_until(
            session_id,
            expected_token,
            binding,
            state,
            fence,
            expected_connection,
            deadline,
            None,
            None,
            None,
        )?;
        match outcome {
            EngramRootReconcileOutcome::Confirmed | EngramRootReconcileOutcome::NoClaim => Ok(()),
            EngramRootReconcileOutcome::EvidenceWithheld {
                guard,
                reason:
                    EngramRootEvidenceUncertainty::UnknownProjection
                    | EngramRootEvidenceUncertainty::UnverifiedStore,
            } => {
                let target = {
                    let inner = self.inner.lock().expect("state mutex poisoned");
                    Self::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
                        .map_err(EngramTransportError::local_state)?
                        .ok_or_else(|| {
                            EngramTransportError::local_state("source-root reader disappeared")
                        })?
                };
                self.apply_engram_root_withholding_classified_until(
                    &target,
                    expected_token,
                    binding,
                    guard,
                    None,
                    deadline,
                )
            }
            EngramRootReconcileOutcome::EvidenceWithheld {
                reason: EngramRootEvidenceUncertainty::OwnerChanged,
                ..
            } => Ok(()),
            EngramRootReconcileOutcome::EvidenceWithheld { reason, .. } => {
                Err(EngramRootReconcileFailure::EvidenceWithheld(reason))
            }
        }
    }

    fn reconcile_engram_named_root_classified_until(
        &self,
        session_id: &str,
        expected_token: &str,
        binding: Option<&EngramControlWorkBinding>,
        state: Option<EngramNamedRootState>,
        fence: &EngramRootReadFence,
        expected_connection: Option<&EngramConnectionConfig>,
        deadline: std::time::Instant,
        guard: Option<EngramAcknowledgedRootGuard>,
        admission_owner: Option<&EngramQueuedAdmissionOwner>,
        expected_runtime: Option<&Option<RuntimeToken>>,
    ) -> Result<EngramRootReconcileOutcome, EngramRootReconcileFailure> {
        let read_generation = fence.generation;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return Err(EngramTransportError::local_state("named-root session disappeared").into());
        };
        if expected_runtime.is_some_and(|runtime| {
            !Self::engram_root_runtime_is_current(&inner.sessions[index], runtime)
        }) || admission_owner.is_some_and(|owner| !owner.matches(&inner.sessions[index]))
        {
            return Err(EngramTransportError::local_state(
                "source-root reply belongs to an earlier runtime or queued owner",
            )
            .into());
        }
        if inner.sessions[index].engram.routing_token.as_deref() != Some(expected_token)
            || inner.sessions[index].engram.work_binding.as_ref() != binding
        {
            return Err(EngramTransportError::local_state(
                "named-root read belongs to an earlier binding",
            )
            .into());
        }
        if let Some(expected) = expected_connection {
            let current =
                Self::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
                    .ok()
                    .flatten();
            if current
                .as_ref()
                .is_none_or(|target| &target.connection != expected)
            {
                return Err(EngramTransportError::local_state(
                    "named-root read belongs to an earlier connection",
                )
                .into());
            }
        }
        if inner.sessions[index]
            .session
            .project_id
            .as_deref()
            .is_some_and(|project_id| !fence.same_store(&inner, project_id))
        {
            return Err(EngramTransportError::local_state(
                "named-root read belongs to an earlier store",
            )
            .into());
        }
        let store = engram_project_for_session_locked(&inner, session_id)
            .and_then(|p| p.engram.as_ref())
            .and_then(|s| s.authority_store_key.clone());
        if binding.is_some() && store.is_none() {
            return Ok(EngramRootReconcileOutcome::EvidenceWithheld {
                guard: None,
                reason: EngramRootEvidenceUncertainty::UnverifiedStore,
            });
        }
        // Check freshness before retiring any retry intent or changing the
        // journal. A newer local transition also protects its runtime state
        // when the older response is missing or unknown.
        if let (Some(store), Some(binding)) = (&store, binding) {
            if !fence.still_current(&inner, store, binding) {
                return Ok(EngramRootReconcileOutcome::EvidenceWithheld {
                    guard,
                    reason: EngramRootEvidenceUncertainty::OwnerChanged,
                });
            }
            let selected_generation = engram_work_source_root_for_claim(
                &inner.engram_work_source_roots,
                store,
                &binding.work_id,
                &binding.claim_id,
            )
            .map(|entry| entry.generation)
            .unwrap_or(0);
            let journal_generation =
                engram_root_journal(&inner.engram_named_root_journal, store, &binding.claim_id)
                    .map(|journal| {
                        journal
                            .confirmed
                            .iter()
                            .map(|(event, _)| event.root.generation)
                            .chain(journal.pending.iter().map(|event| event.root.generation))
                            .chain(
                                journal
                                    .reconciliation
                                    .iter()
                                    .map(|read| read.intent.root.generation),
                            )
                            .max()
                            .unwrap_or(0)
                    })
                    .unwrap_or(0);
            if selected_generation.max(journal_generation) > read_generation {
                return Ok(EngramRootReconcileOutcome::EvidenceWithheld {
                    guard,
                    reason: EngramRootEvidenceUncertainty::OwnerChanged,
                });
            }
        }
        let canonical = if let (Some(store), Some(binding)) = (&store, binding) {
            let target =
                Self::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
                    .ok()
                    .flatten()
                    .ok_or_else(|| {
                        EngramTransportError::local_state("named-root reader disappeared")
                    })?;
            if !Self::engram_root_admission_is_current_locked(
                &inner,
                &target,
                expected_token,
                Some(binding),
                guard.as_ref(),
                admission_owner,
            ) {
                return Err(EngramTransportError::local_state(
                    "source-root recovery no longer owns admission",
                )
                .into());
            }
            drop(inner);
            let guard = match guard.filter(|guard| {
                guard.store == *store
                    && guard.owner.binding == *binding
                    && guard.still_withholding(&self.inner.lock().expect("state mutex poisoned"))
            }) {
                Some(guard) => guard,
                None => self
                    .guard_engram_root_read_classified_until(&target, Some(binding), deadline)?
                    .ok_or_else(|| {
                        EngramRootReconcileFailure::Infrastructure(ApiError::internal(
                            "claimed source-root recovery has no durable guard",
                        ))
                    })?,
            };
            let owner = guard.owner.clone();
            if state == Some(EngramNamedRootState::Unknown) {
                return Ok(EngramRootReconcileOutcome::EvidenceWithheld {
                    guard: Some(guard),
                    reason: EngramRootEvidenceUncertainty::UnknownProjection,
                });
            }
            let proof = match self
                .read_engram_authority_fact_classified_until(&target, binding, deadline)
            {
                Ok(proof) => proof,
                Err(EngramAuthorityReadFailure::AdmissionInvalid(error)) => {
                    return Err(error.into());
                }
                Err(EngramAuthorityReadFailure::Source(error)) => {
                    return Ok(EngramRootReconcileOutcome::EvidenceWithheld {
                        guard: Some(guard),
                        reason: EngramRootEvidenceUncertainty::CanonicalRead(error),
                    });
                }
            };
            inner = self.inner.lock().expect("state mutex poisoned");
            if !Self::engram_root_admission_is_current_locked(
                &inner,
                &target,
                expected_token,
                Some(binding),
                Some(&guard),
                admission_owner,
            ) {
                return Err(EngramTransportError::local_state(
                    "canonical source reply belongs to invalid admission authority",
                )
                .into());
            }
            if !fence.still_current(&inner, store, binding) {
                return Ok(EngramRootReconcileOutcome::EvidenceWithheld {
                    guard: Some(guard),
                    reason: EngramRootEvidenceUncertainty::OwnerChanged,
                });
            }
            if let Err(error) =
                Self::learn_engram_authority_locked(&mut inner, store, &owner, &proof)
            {
                return Ok(EngramRootReconcileOutcome::EvidenceWithheld {
                    guard: Some(guard),
                    reason: EngramRootEvidenceUncertainty::CanonicalHistory(error),
                });
            }
            Some((guard, proof, deadline))
        } else {
            None
        };
        let state = canonical
            .as_ref()
            .map(|(_, proof, _)| Some(proof.named_root.clone()))
            .unwrap_or(state);
        let Some(index) = inner.find_session_index(session_id) else {
            return Err(EngramTransportError::local_state(
                "named-root session disappeared during read",
            )
            .into());
        };
        if let (Some(store), Some(binding), Some(state)) = (&store, binding, &state) {
            let orphan =
                engram_root_journal(&inner.engram_named_root_journal, store, &binding.claim_id)
                    .and_then(|journal| journal.pending.as_ref())
                    .filter(|event| {
                        !event.reporter.is_empty()
                            && event.root.generation <= read_generation
                            && !Self::engram_root_reporter_available(&inner, event)
                    })
                    .cloned();
            if let Some(event) = orphan {
                if matches!(
                    state,
                    EngramNamedRootState::None
                        | EngramNamedRootState::UnboundByRelease { .. }
                        | EngramNamedRootState::Bound { .. }
                ) {
                    let journal = inner
                        .engram_named_root_journal
                        .iter_mut()
                        .find(|journal| {
                            &journal.store == store && journal.claim_id == binding.claim_id
                        })
                        .unwrap();
                    journal.reconciliation = Some(EngramRootReconciliation {
                        intent: event.clone(),
                        state: state.clone(),
                        observed_by: session_id.to_owned(),
                        observed_at: chrono::Utc::now()
                            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                        settled: canonical
                            .as_ref()
                            .is_some_and(|(_, proof, _)| proof.settles_intent(&event)),
                    });
                    if journal
                        .reconciliation
                        .as_ref()
                        .is_some_and(|read| read.settled)
                    {
                        journal.pending = None;
                    }
                    journal.retire(engram_root_retirement_for_state(state));
                    let settled = journal
                        .reconciliation
                        .as_ref()
                        .is_some_and(|read| read.settled);
                    inner
                        .engram_work_source_roots
                        .retain(|root| &root.store != store || root.claim_id != binding.claim_id);
                    inner.sessions[index].engram.set_pending_source_root_line(if settled {
                        "[TermAl] The previous naming session is unavailable. Its pending intent was retired by covering authoritative readback, without an event receipt. Name a valid worktree to establish a fresh generation; evidence remains withheld until then."
                    } else {
                        "[TermAl] The previous naming session is unavailable. Current canonical absence does not settle its pending write; the original retry intent is retained and evidence remains withheld pending recovery."
                    }.to_owned());
                } else {
                    inner.sessions[index].engram.set_pending_source_root_line(
                        "[TermAl] The pending source-root intent cannot be replayed because its reporting session is unavailable. Focus this claim and retry naming after Engram can return its authoritative state; evidence remains withheld.".to_owned());
                }
            }
            if let Some(journal) = inner
                .engram_named_root_journal
                .iter_mut()
                .find(|journal| &journal.store == store && journal.claim_id == binding.claim_id)
                && journal.pending.is_none()
                && journal.confirmed.as_ref().is_some_and(|(event, _)| {
                    engram_named_root_read_obsoletes(&event.root, Some(state), read_generation)
                })
            {
                journal.retire(engram_root_retirement_for_state(state));
            }
            if let Some(entry) = engram_work_source_root_for_claim(
                &inner.engram_work_source_roots,
                store,
                &binding.work_id,
                &binding.claim_id,
            )
            .cloned()
            {
                if engram_named_root_read_obsoletes(&entry, Some(state), read_generation) {
                    inner.engram_work_source_roots.retain(|root| root != &entry);
                    inner.sessions[index].engram.queue_source_root_notice(
                        EngramSourceRootNoticeKind::SelectionLoss { selection: entry },
                    );
                }
            }
            if let EngramNamedRootState::Bound { generation, .. }
            | EngramNamedRootState::UnboundByRelease {
                last_generation: generation,
                ..
            } = state
            {
                if let Ok(generation) = u64::try_from(*generation) {
                    inner.engram_source_root_generation =
                        inner.engram_source_root_generation.max(generation);
                }
            }
        }
        inner.sessions[index].engram.named_root = state;
        engram_compact_root_journal(&mut inner);
        drop(inner);
        if let (Some(store), Some((guard, _, deadline))) = (&store, canonical) {
            if let Err(error) = self.publish_engram_authority_until(store, &guard.owner, deadline) {
                return Ok(EngramRootReconcileOutcome::EvidenceWithheld {
                    guard: Some(guard),
                    reason: EngramRootEvidenceUncertainty::Publication(error),
                });
            }
        }
        Ok(if binding.is_some() {
            EngramRootReconcileOutcome::Confirmed
        } else {
            EngramRootReconcileOutcome::NoClaim
        })
    }
}

#[cfg(test)]
thread_local! {
    // A deterministic monotone clock lets the reader's real deadline branch
    // be exercised without imposing a throughput requirement on the machine.
    static TEST_ENGRAM_REMOVED_ROOT_READ_CLOCK: std::cell::RefCell<Option<Box<dyn FnMut() -> std::time::Instant>>> =
        const { std::cell::RefCell::new(None) };
    /// A local transition after transport but before application must fence
    /// an otherwise authoritative historical response.
    static TEST_ENGRAM_DURING_ROOT_LIFECYCLE_READ: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}
