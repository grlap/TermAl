// Owns canonical naming history and the durable publication boundary for one
// work in one authority store. Protocol callers submit facts; captures and
// evaluations consume only acknowledged authority. Uses the existing writer.

const ENGRAM_NAMING_HISTORY_EPOCH: u64 = 1;

#[cfg(test)]
thread_local! {
    // Manual AppState fixtures explicitly lack the production persist worker.
    // Tests can disable this allowance to exercise the stopped-writer refusal.
    static TEST_ENGRAM_AUTHORITY_MANUAL_WRITER: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

fn engram_authority_manual_writer_allowed() -> bool {
    #[cfg(test)]
    {
        TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.get())
    }
    #[cfg(not(test))]
    {
        false
    }
}

// A renewed view changes revision and fence, but not the canonical run that
// still needs reconciliation. Keep the newest view for that exact association.
fn engram_same_recovery_run(
    left: &EngramControlWorkBinding,
    right: &EngramControlWorkBinding,
) -> bool {
    left.work_id == right.work_id
        && left.run_id == right.run_id
        && left.claim_id == right.claim_id
        && left.root_execution_id == right.root_execution_id
}

fn engram_retain_recovery_run(
    runs: &mut Vec<EngramControlWorkBinding>,
    binding: &EngramControlWorkBinding,
) {
    runs.retain(|existing| !engram_same_recovery_run(existing, binding));
    runs.push(binding.clone());
}

struct EngramRootEventReply {
    receipt: EngramNamedRootReceipt,
    owner: EngramAuthorityTransition,
    proof: EngramNamedRootReadResponse,
}

/// Issued only after the existing exact-image fence acknowledged Prepared.
/// The current owner and admission identities must still be checked at use.
#[derive(Clone)]
struct EngramAcknowledgedRootGuard {
    store: EngramAuthorityStoreKey,
    owner: EngramAuthorityTransition,
    runtime: Option<RuntimeToken>,
    connection: EngramConnectionConfig,
    settings: EngramProjectSettings,
}

impl EngramAcknowledgedRootGuard {
    fn still_withholding(&self, inner: &StateInner) -> bool {
        inner.engram_work_naming_history.iter().any(|history| {
            history.store == self.store
                && history.work_id == self.owner.binding.work_id
                && history.transition.as_ref().is_some_and(|current| {
                    current.id == self.owner.id
                        && current.version == self.owner.version
                        && current.binding == self.owner.binding
                        && current.phase != EngramAuthorityPhase::Published
                })
        }) && engram_authority_work_unresolved(inner, &self.store, &self.owner.binding.work_id)
    }
}

enum EngramRootEvidenceUncertainty {
    UnverifiedStore,
    UnknownProjection,
    OwnerChanged,
    CanonicalRead(ApiError),
    CanonicalHistory(ApiError),
    Publication(ApiError),
}

enum EngramRootReconcileOutcome {
    Confirmed,
    NoClaim,
    EvidenceWithheld {
        guard: Option<EngramAcknowledgedRootGuard>,
        reason: EngramRootEvidenceUncertainty,
    },
}

enum EngramRootReconcileFailure {
    AdmissionInvalid(EngramTransportError),
    Infrastructure(ApiError),
}

enum EngramAuthorityReadFailure {
    Source(ApiError),
    AdmissionInvalid(EngramTransportError),
}

impl EngramAuthorityReadFailure {
    fn into_api(self) -> ApiError {
        match self {
            Self::Source(error) => error,
            Self::AdmissionInvalid(error) => ApiError::conflict(error.to_string()),
        }
    }
}

impl EngramRootReconcileFailure {
    fn into_transport(self) -> EngramTransportError {
        match self {
            Self::AdmissionInvalid(error) => error,
            Self::Infrastructure(error) => EngramTransportError::transport(error.message),
        }
    }
}

impl From<EngramTransportError> for EngramRootReconcileFailure {
    fn from(error: EngramTransportError) -> Self {
        Self::AdmissionInvalid(error)
    }
}

impl EngramRootEvidenceUncertainty {
    fn into_transport(self) -> EngramTransportError {
        match self {
            Self::UnverifiedStore => {
                EngramTransportError::local_state("claimed source-root store is unverified")
            }
            Self::UnknownProjection => {
                EngramTransportError::protocol("source-root projection is unconfirmed")
            }
            Self::OwnerChanged => {
                EngramTransportError::local_state("source-root owner changed during recovery")
            }
            Self::CanonicalRead(error)
            | Self::CanonicalHistory(error)
            | Self::Publication(error) => EngramTransportError::protocol(error.message),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramAuthorityProof {
    binding: EngramControlWorkBinding,
    read: EngramNamedRootReadResponse,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramNamingFrontier {
    run_id: String,
    run_generation: i64,
    claim_id: String,
    event_id: String,
    position: EngramNamedRootReadPosition,
    workspace_id: String,
    root_generation: i64,
    named_at: String,
    kind: EngramNamedRootKind,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramAuthorityPhase {
    Prepared,
    Candidate,
    Published,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramAuthorityTransition {
    id: String,
    version: u64,
    binding: EngramControlWorkBinding,
    phase: EngramAuthorityPhase,
}

/// Exact materialized metadata for the entire work. In particular an absent
/// selection or journal is part of the expected content, not a missing check.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EngramAuthorityImage {
    store: EngramAuthorityStoreKey,
    work_id: String,
    history: EngramWorkNamingHistory,
    selection: Option<EngramWorkSourceRoot>,
    journals: Vec<EngramNamedRootJournal>,
    allocation_floor: u64,
}

fn engram_journal_names_work(journal: &EngramNamedRootJournal, work: &str) -> bool {
    journal
        .read_binding
        .as_ref()
        .is_some_and(|binding| binding.work_id == work)
        || journal
            .confirmed
            .as_ref()
            .is_some_and(|(event, _)| event.root.work_id == work)
        || journal
            .pending
            .as_ref()
            .is_some_and(|event| event.root.work_id == work)
        || journal
            .reconciliation
            .as_ref()
            .is_some_and(|read| read.intent.root.work_id == work)
}

fn engram_authority_work_unresolved(
    inner: &StateInner,
    store: &EngramAuthorityStoreKey,
    work: &str,
) -> bool {
    inner
        .engram_work_naming_history
        .iter()
        .find(|history| &history.store == store && history.work_id == work)
        .is_none_or(|history| {
            history.epoch != ENGRAM_NAMING_HISTORY_EPOCH
                || history.proofs.is_empty()
                || !history.unresolved_runs.is_empty()
                || history
                    .transition
                    .as_ref()
                    .is_none_or(|transition| transition.phase != EngramAuthorityPhase::Published)
        })
        || inner.engram_named_root_journal.iter().any(|journal| {
            &journal.store == store
                && engram_journal_names_work(journal, work)
                && journal.requires_reconciliation()
        })
}

impl EngramAuthorityImage {
    fn capture(
        inner: &StateInner,
        store: &EngramAuthorityStoreKey,
        work: &str,
    ) -> Result<Self, ApiError> {
        let history = inner
            .engram_work_naming_history
            .iter()
            .find(|history| &history.store == store && history.work_id == work)
            .cloned()
            .ok_or_else(|| ApiError::conflict("named-root authority owner is absent"))?;
        Ok(Self {
            store: store.clone(),
            work_id: work.to_owned(),
            history,
            selection: engram_work_source_root_for_work(
                &inner.engram_work_source_roots,
                store,
                work,
            )
            .cloned(),
            journals: inner
                .engram_named_root_journal
                .iter()
                .filter(|journal| {
                    &journal.store == store && engram_journal_names_work(journal, work)
                })
                .cloned()
                .collect(),
            allocation_floor: inner.engram_source_root_generation,
        })
    }

    fn matches_metadata(&self, metadata: &PersistedState) -> bool {
        metadata.engram_work_naming_history.iter()
            .find(|history| history.store == self.store && history.work_id == self.work_id) == Some(&self.history)
            && engram_work_source_root_for_work(&metadata.engram_work_source_roots, &self.store, &self.work_id) == self.selection.as_ref()
            && metadata.engram_named_root_journal.iter()
                .filter(|journal| journal.store == self.store && engram_journal_names_work(journal, &self.work_id))
                .eq(self.journals.iter())
            // Other work may allocate while this scoped image is in flight;
            // only lowering the watermark would fail to preserve our image.
            && metadata.engram_source_root_generation >= self.allocation_floor
    }

    fn still_owned(&self, inner: &StateInner) -> bool {
        Self::capture(inner, &self.store, &self.work_id).is_ok_and(|current| {
            current.history == self.history
                && current.selection == self.selection
                && current.journals == self.journals
                && current.allocation_floor >= self.allocation_floor
        })
    }
}

impl EngramNamedRootReadResponse {
    fn validate_authority(
        &self,
        store: &EngramAuthorityStoreKey,
        binding: &EngramControlWorkBinding,
    ) -> Result<(), ApiError> {
        let feed = |position: &EngramNamedRootReadPosition| {
            position.feed.kind == "run_execution" && position.feed.id == binding.run_id
        };
        if self.project_id != store.project_id
            || self.work_id != binding.work_id
            || self.run_id != binding.run_id
            || self.claim_id != binding.claim_id
            || self.root_execution_id != binding.root_execution_id
            || self.run.generation <= 0
            || !feed(&self.read_cut)
            || self.read_cut.position < 0
            || !matches!(self.run.state.as_str(), "open" | "completed" | "cancelled")
        {
            return Err(ApiError::bad_gateway(
                "named-root read has a contradictory canonical run association",
            ));
        }
        if let Some(event) = &self.latest_event {
            if event.event.is_empty()
                || !feed(&event.position)
                || event.position.position <= 0
                || event.position.position > self.read_cut.position
                || event.generation <= 0
                || event.workspace_id.is_empty()
                || chrono::DateTime::parse_from_rfc3339(&event.named_at).is_err()
            {
                return Err(ApiError::bad_gateway(
                    "named-root read has malformed canonical event history",
                ));
            }
        }
        let consistent = match &self.named_root {
            EngramNamedRootState::Unknown => false,
            EngramNamedRootState::Bound {
                workspace_id,
                generation,
                named_at,
            } => self.latest_event.as_ref().is_some_and(|event| {
                event.kind == EngramNamedRootKind::Bound
                    && event.workspace_id == *workspace_id
                    && event.generation == *generation
                    && engram_named_at_matches(&event.named_at, named_at)
            }),
            EngramNamedRootState::UnboundByRelease {
                last_generation,
                released_at_position,
            } => self.latest_event.as_ref().is_some_and(|event| {
                *last_generation == event.generation
                    && *released_at_position > event.position.position
                    && *released_at_position <= self.read_cut.position
            }),
            EngramNamedRootState::None => self.latest_event.as_ref().is_none_or(|event| {
                event.kind == EngramNamedRootKind::Ended
                    || matches!(self.run.state.as_str(), "completed" | "cancelled")
            }),
        };
        if !consistent {
            return Err(ApiError::bad_gateway(
                "named-root state contradicts its canonical event history",
            ));
        }
        Ok(())
    }

    fn frontier(&self) -> Option<EngramNamingFrontier> {
        self.latest_event
            .as_ref()
            .map(|event| EngramNamingFrontier {
                run_id: self.run_id.clone(),
                run_generation: self.run.generation,
                claim_id: self.claim_id.clone(),
                event_id: event.event.clone(),
                position: event.position.clone(),
                workspace_id: event.workspace_id.clone(),
                root_generation: event.generation,
                named_at: event.named_at.clone(),
                kind: event.kind,
            })
    }
}

/// The event position orders only this run; the run ordinal orders only this
/// work/store. Neither is the local historical invalidation revision.
fn engram_learn_canonical_history(
    history: &mut EngramWorkNamingHistory,
    read: &EngramNamedRootReadResponse,
) -> Result<(), ApiError> {
    let Some(next) = read.frontier() else {
        if history.epoch != ENGRAM_NAMING_HISTORY_EPOCH && history.known_generation > 0 {
            return Err(ApiError::conflict(
                "legacy naming origin is unexplained; recover its original run association or obtain explicit repair before a fresh evaluation",
            ));
        }
        if history.epoch != ENGRAM_NAMING_HISTORY_EPOCH {
            history.revision = history
                .revision
                .checked_add(1)
                .ok_or_else(|| ApiError::internal("named-root history revision exhausted"))?;
        }
        history.epoch = ENGRAM_NAMING_HISTORY_EPOCH;
        return Ok(());
    };
    let mut new_naming = true;
    if let Some(previous) = &history.frontier {
        if next.run_id == previous.run_id && next.run_generation != previous.run_generation {
            return Err(ApiError::bad_gateway(
                "canonical run ordinal changed for an existing run identity",
            ));
        }
        if next.run_generation < previous.run_generation {
            return Ok(());
        }
        if next.run_generation == previous.run_generation {
            if next.run_id != previous.run_id {
                return Err(ApiError::bad_gateway(
                    "equal run ordinals identify different canonical runs",
                ));
            }
            if next.position.position < previous.position.position {
                return Ok(());
            }
            if next.position.position == previous.position.position {
                if next.event_id != previous.event_id
                    || next.claim_id != previous.claim_id
                    || next.kind != previous.kind
                    || next.workspace_id != previous.workspace_id
                    || next.root_generation != previous.root_generation
                    || !engram_named_at_matches(&next.named_at, &previous.named_at)
                {
                    return Err(ApiError::bad_gateway(
                        "equal run-feed positions identify different naming events",
                    ));
                }
                return Ok(());
            }
            if next.claim_id != previous.claim_id {
                return Err(ApiError::bad_gateway(
                    "canonical run changed its naming claim",
                ));
            }
            if next.event_id == previous.event_id {
                return Err(ApiError::bad_gateway(
                    "one naming event has different canonical positions",
                ));
            }
            let same_binding = next.workspace_id == previous.workspace_id
                && next.root_generation == previous.root_generation
                && engram_named_at_matches(&next.named_at, &previous.named_at);
            if next.kind == EngramNamedRootKind::Ended && same_binding {
                new_naming = false;
            }
            if next.root_generation < previous.root_generation
                || next.root_generation == previous.root_generation
                    && (next.kind != EngramNamedRootKind::Ended || !same_binding)
            {
                return Err(ApiError::bad_gateway(
                    "later canonical event contradicts this claim's binding order",
                ));
            }
        }
    }
    if new_naming || history.epoch != ENGRAM_NAMING_HISTORY_EPOCH {
        history.revision = history
            .revision
            .checked_add(1)
            .ok_or_else(|| ApiError::internal("named-root history revision exhausted"))?;
    }
    history.known_generation = history
        .known_generation
        .max(u64::try_from(next.root_generation).unwrap_or(0));
    history.frontier = Some(next);
    history.epoch = ENGRAM_NAMING_HISTORY_EPOCH;
    Ok(())
}

impl AppState {
    // Stop may temporarily borrow the exact runtime while its outcome is
    // unknown. This is identity only: the existing Stop/admission handoff
    // barrier still decides whether delivery can proceed after it settles.
    fn engram_root_runtime_is_current(
        record: &SessionRecord,
        expected: &Option<RuntimeToken>,
    ) -> bool {
        record.runtime.runtime_token() == *expected
            || (expected.is_some()
                && record.runtime.runtime_token().is_none()
                && record.runtime_stop_in_progress
                && record.runtime_stop_owner.as_ref().is_some_and(|owner| {
                    owner.kind == RuntimeStopOwnerKind::UserStop
                        && owner.token == *expected
                        && owner.generation == record.runtime_stop_generation
                }))
    }

    fn engram_root_admission_is_current_locked(
        inner: &StateInner,
        target: &EngramBindingTarget,
        expected_token: &str,
        binding: Option<&EngramControlWorkBinding>,
        guard: Option<&EngramAcknowledgedRootGuard>,
        admission_owner: Option<&EngramQueuedAdmissionOwner>,
    ) -> bool {
        let Some(index) = inner.find_session_index(&target.connection.session_id) else {
            return false;
        };
        let record = &inner.sessions[index];
        let current = Self::engram_binding_target_for_session_shape_locked(
            inner,
            &target.connection.session_id,
            true,
        )
        .ok()
        .flatten();
        current.as_ref().is_some_and(|current| {
            current.connection == target.connection
                && current.settings.same_admission_settings(&target.settings)
        }) && record.engram.routing_token.as_deref() == Some(expected_token)
            && Self::engram_root_runtime_is_current(record, &target.runtime_snapshot)
            && record.engram.work_binding.as_ref() == binding
            && guard.is_none_or(|guard| {
                Self::engram_root_runtime_is_current(record, &guard.runtime)
                    && guard.connection == target.connection
                    && guard.settings.same_admission_settings(&target.settings)
            })
            && admission_owner.is_none_or(|owner| owner.matches(record))
    }

    fn apply_engram_root_withholding_until(
        &self,
        target: &EngramBindingTarget,
        expected_token: &str,
        binding: Option<&EngramControlWorkBinding>,
        mut guard: Option<EngramAcknowledgedRootGuard>,
        admission_owner: Option<&EngramQueuedAdmissionOwner>,
        deadline: std::time::Instant,
    ) -> Result<(), EngramTransportError> {
        let identity_current = |inner: &StateInner, guard: Option<&EngramAcknowledgedRootGuard>| {
            Self::engram_root_admission_is_current_locked(
                inner,
                target,
                expected_token,
                binding,
                guard,
                admission_owner,
            )
        };
        {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if !identity_current(&inner, guard.as_ref()) {
                return Err(EngramTransportError::local_state(
                    "source uncertainty belongs to an invalid admission owner",
                ));
            }
        }
        if target.settings.authority_store_key.is_some()
            && guard.as_ref().is_none_or(|guard| {
                !guard.still_withholding(&self.inner.lock().expect("state mutex poisoned"))
            })
        {
            // A successor's publication invalidates the old proof's lifetime.
            // Establish a new acknowledged obligation, never reuse its token
            // to overwrite a successor's admission or assume on-disk guard state.
            guard = self.guard_engram_root_read_until(target, binding, deadline)?;
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if !identity_current(&inner, guard.as_ref())
            || (target.settings.authority_store_key.is_some()
                && guard
                    .as_ref()
                    .is_none_or(|guard| !guard.still_withholding(&inner)))
        {
            return Err(EngramTransportError::local_state(
                "source uncertainty has no current acknowledged recovery guard",
            ));
        }
        let index = inner
            .find_session_index(&target.connection.session_id)
            .expect("validated source uncertainty session");
        inner.sessions[index].engram.named_root = Some(EngramNamedRootState::Unknown);

        // Prepared/Candidate plus its exact retained association is the
        // acknowledged restart guard. Restoration/capture readers withhold
        // it; the independent admission fence still gates provider handoff.
        Ok(())
    }

    fn reconcile_engram_root_for_admission_until(
        &self,
        target: &EngramBindingTarget,
        expected_token: &str,
        binding: Option<&EngramControlWorkBinding>,
        state: Option<EngramNamedRootState>,
        fence: &EngramRootReadFence,
        guard: Option<EngramAcknowledgedRootGuard>,
        admission_owner: Option<&EngramQueuedAdmissionOwner>,
        deadline: std::time::Instant,
    ) -> Result<EngramRootReconcileOutcome, EngramTransportError> {
        if let Some(owner) = admission_owner {
            self.require_queued_engram_owner(
                &target.connection.session_id,
                owner,
                "Source reconciliation",
            )?;
        }
        let original_guard = guard.clone();
        let outcome = self
            .reconcile_engram_named_root_classified_until(
                &target.connection.session_id,
                expected_token,
                binding,
                state,
                fence,
                Some(&target.connection),
                deadline,
                guard,
                admission_owner,
                Some(&target.runtime_snapshot),
            )
            .map_err(EngramRootReconcileFailure::into_transport)?;
        if let EngramRootReconcileOutcome::EvidenceWithheld { guard, .. } = &outcome {
            self.apply_engram_root_withholding_until(
                target,
                expected_token,
                binding,
                guard.clone(),
                admission_owner,
                deadline,
            )?;
        }
        if !Self::engram_root_admission_is_current_locked(
            &self.inner.lock().expect("state mutex poisoned"),
            target,
            expected_token,
            binding,
            original_guard.as_ref(),
            admission_owner,
        ) {
            return Err(EngramTransportError::local_state(
                "source reconciliation no longer owns admission",
            ));
        }
        Ok(outcome)
    }

    #[cfg(test)]
    fn guard_engram_root_read(
        &self,
        target: &EngramBindingTarget,
        binding: Option<&EngramControlWorkBinding>,
        budget: Duration,
    ) -> Result<(), EngramTransportError> {
        self.guard_engram_root_read_until(target, binding, std::time::Instant::now() + budget)
            .map(|_| ())
    }

    fn guard_engram_root_read_until(
        &self,
        target: &EngramBindingTarget,
        binding: Option<&EngramControlWorkBinding>,
        deadline: std::time::Instant,
    ) -> Result<Option<EngramAcknowledgedRootGuard>, EngramTransportError> {
        self.guard_engram_root_read_classified_until(target, binding, deadline)
            .map_err(EngramRootReconcileFailure::into_transport)
    }

    fn guard_engram_root_read_classified_until(
        &self,
        target: &EngramBindingTarget,
        binding: Option<&EngramControlWorkBinding>,
        deadline: std::time::Instant,
    ) -> Result<Option<EngramAcknowledgedRootGuard>, EngramRootReconcileFailure> {
        let Some((store, binding)) = target.settings.authority_store_key.as_ref().zip(binding)
        else {
            return Ok(None);
        };
        let runtime = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(&target.connection.session_id)
                .ok_or_else(|| {
                    EngramTransportError::local_state("source-root guard session disappeared")
                })?;
            if !Self::engram_root_runtime_is_current(
                &inner.sessions[index],
                &target.runtime_snapshot,
            ) {
                return Err(EngramTransportError::local_state(
                    "source-root guard belongs to an earlier runtime",
                )
                .into());
            }
            target.runtime_snapshot.clone()
        };
        let owner = self
            .prepare_engram_authority_until(store, binding, deadline)
            .map_err(EngramRootReconcileFailure::Infrastructure)?;
        let inner = self.inner.lock().expect("state mutex poisoned");
        let current = Self::engram_binding_target_for_session_shape_locked(
            &inner,
            &target.connection.session_id,
            true,
        )
        .ok()
        .flatten();
        let index = inner
            .find_session_index(&target.connection.session_id)
            .ok_or_else(|| {
                EngramTransportError::local_state("source-root guard session disappeared")
            })?;
        if current.as_ref().is_none_or(|current| {
            current.connection != target.connection
                || !current.settings.same_admission_settings(&target.settings)
        }) || !Self::engram_root_runtime_is_current(&inner.sessions[index], &runtime)
        {
            return Err(EngramTransportError::local_state(
                "source-root guard admission authority changed",
            )
            .into());
        }
        Ok(Some(EngramAcknowledgedRootGuard {
            store: store.clone(),
            owner,
            runtime,
            connection: target.connection.clone(),
            settings: target.settings.clone(),
        }))
    }

    fn learn_engram_authority_locked(
        inner: &mut StateInner,
        store: &EngramAuthorityStoreKey,
        owner: &EngramAuthorityTransition,
        proof: &EngramNamedRootReadResponse,
    ) -> Result<(), ApiError> {
        let history = inner
            .engram_work_naming_history
            .iter_mut()
            .find(|history| &history.store == store && history.work_id == owner.binding.work_id)
            .ok_or_else(|| ApiError::conflict("named-root authority owner disappeared"))?;
        if history
            .transition
            .as_ref()
            .is_none_or(|current| current.id != owner.id || current.version != owner.version)
        {
            return Err(ApiError::conflict(
                "named-root response belongs to a superseded work owner",
            ));
        }
        let binding = history
            .unresolved_runs
            .iter()
            .find(|binding| binding.run_id == proof.run_id && binding.claim_id == proof.claim_id)
            .or_else(|| {
                (owner.binding.run_id == proof.run_id && owner.binding.claim_id == proof.claim_id)
                    .then_some(&owner.binding)
            })
            .cloned()
            .ok_or_else(|| {
                ApiError::conflict("canonical proof has no retained recovery association")
            })?;
        proof.validate_authority(store, &binding)?;
        if let Some(previous) = history
            .proofs
            .iter()
            .find(|previous| engram_same_recovery_run(&previous.binding, &binding))
        {
            if proof.read_cut.position < previous.read.read_cut.position {
                return Err(ApiError::conflict(
                    "canonical recovery proof moved behind its acknowledged cut",
                ));
            }
            if let Some(event) = &previous.read.latest_event
                && proof
                    .latest_event
                    .as_ref()
                    .is_none_or(|next| next.position.position < event.position.position)
            {
                return Err(ApiError::conflict(
                    "canonical recovery proof lost acknowledged event history",
                ));
            }
        }
        engram_learn_canonical_history(history, proof)?;
        history
            .proofs
            .retain(|previous| !engram_same_recovery_run(&previous.binding, &binding));
        history.proofs.push(EngramAuthorityProof {
            binding: binding.clone(),
            read: proof.clone(),
        });
        // This discharges the read obligation only. Pending mutation payloads
        // remain in the journal and independently keep the common guard closed.
        history
            .unresolved_runs
            .retain(|pending| !engram_same_recovery_run(pending, &binding));
        if history.unresolved_runs.is_empty() {
            history.recovery_reason = None;
        }
        Ok(())
    }

    /// All retained associations participate, even if the original session
    /// disappeared before creating a journal. One owner and one eventual
    /// complete-image ACK cover this bounded batch; reads never adopt focus.
    fn recover_engram_authority_batch_until(
        &self,
        store: &EngramAuthorityStoreKey,
        owner: &EngramAuthorityTransition,
        deadline: std::time::Instant,
        settle_abandoned_writes: bool,
    ) -> Result<(), ApiError> {
        let budget = deadline.saturating_duration_since(std::time::Instant::now());
        let (target, runs, abandoned) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let history = inner
                .engram_work_naming_history
                .iter()
                .find(|history| &history.store == store && history.work_id == owner.binding.work_id)
                .ok_or_else(|| ApiError::conflict("recovery history disappeared"))?;
            if history
                .transition
                .as_ref()
                .is_none_or(|current| current.id != owner.id || current.version != owner.version)
            {
                return Err(ApiError::conflict(
                    "recovery batch belongs to an earlier owner",
                ));
            }
            let target = inner
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
                })
                .find(|target| {
                    target.settings.authority_store_key.as_ref() == Some(store)
                        && target.routing_token.is_some()
                });
            let abandoned = inner
                .engram_named_root_journal
                .iter()
                .filter_map(|journal| journal.pending.as_ref())
                .filter(|intent| !Self::engram_root_reporter_available(&inner, intent))
                .cloned()
                .collect::<Vec<_>>();
            (
                target,
                history
                    .unresolved_runs
                    .iter()
                    .take(8)
                    .cloned()
                    .collect::<Vec<_>>(),
                abandoned,
            )
        };
        if runs.is_empty() {
            return Ok(());
        }
        let reserve = budget / 4;
        for binding in runs {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let Some(_read_budget) = left.checked_sub(reserve).filter(|left| !left.is_zero())
            else {
                break;
            };
            let result = target
                .as_ref()
                .ok_or_else(|| {
                    ApiError::conflict(
                        "retained named-root run needs an authorized reader in its original store",
                    )
                })
                .and_then(|target| {
                    self.read_engram_authority_fact_until(target, &binding, deadline - reserve)
                });
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let current = inner
                .engram_work_naming_history
                .iter_mut()
                .find(|history| &history.store == store && history.work_id == owner.binding.work_id)
                .filter(|history| {
                    history.transition.as_ref().is_some_and(|current| {
                        current.id == owner.id && current.version == owner.version
                    })
                })
                .ok_or_else(|| ApiError::conflict("recovery batch was superseded during I/O"))?;
            match result {
                Err(error) => {
                    // A failed association moves behind the others. Saving
                    // this image makes restart/refresh retries fair too.
                    engram_retain_recovery_run(&mut current.unresolved_runs, &binding);
                    current.recovery_reason = Some(format!(
                        "retained run {} / claim {} remains unresolved: {}",
                        binding.run_id, binding.claim_id, error.message
                    ));
                }
                Ok(proof) => {
                    if let Err(error) =
                        Self::learn_engram_authority_locked(&mut inner, store, owner, &proof)
                    {
                        let history = inner
                            .engram_work_naming_history
                            .iter_mut()
                            .find(|history| {
                                &history.store == store && history.work_id == binding.work_id
                            })
                            .expect("owned recovery history");
                        engram_retain_recovery_run(&mut history.unresolved_runs, &binding);
                        history.recovery_reason = Some(error.message);
                        continue;
                    }
                    // Only exact committed/superseded intent history or a
                    // closed lifecycle settles a potentially executable write.
                    // Keep its original bytes as readback, never a receipt.
                    let lifecycle_closed =
                        matches!(proof.run.state.as_str(), "completed" | "cancelled")
                            || matches!(
                                proof.named_root,
                                EngramNamedRootState::UnboundByRelease { .. }
                            );
                    let caller_owns_binding = target
                        .as_ref()
                        .and_then(|target| target.work_binding.as_ref())
                        .is_some_and(|current| engram_same_recovery_run(current, &binding));
                    for journal in inner
                        .engram_named_root_journal
                        .iter_mut()
                        .filter(|journal| {
                            &journal.store == store
                                && journal.read_binding.as_ref().is_some_and(|retained| {
                                    engram_same_recovery_run(retained, &binding)
                                })
                        })
                    {
                        if settle_abandoned_writes
                            && !caller_owns_binding
                            && (proof.definitively_unbound() || lifecycle_closed)
                            && journal.pending.as_ref().is_some_and(|intent| {
                                abandoned.contains(intent) && proof.settles_intent(intent)
                            })
                            && let Some(intent) = journal.pending.take()
                        {
                            journal.reconciliation = Some(EngramRootReconciliation {
                                intent,
                                state: proof.named_root.clone(),
                                observed_by: target
                                    .as_ref()
                                    .expect("validated reader")
                                    .connection
                                    .session_id
                                    .clone(),
                                observed_at: chrono::Utc::now()
                                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                                settled: true,
                            });
                            journal.retire(EngramRootRetirement::Unbound);
                        } else if journal.pending.is_none()
                            && proof.covers(journal)
                            && proof.definitively_unbound()
                        {
                            journal.retire(EngramRootRetirement::Unbound);
                        }
                    }
                    if proof.definitively_unbound() || lifecycle_closed {
                        inner.engram_work_source_roots.retain(|root| {
                            &root.store != store
                                || root.work_id != binding.work_id
                                || root.claim_id != binding.claim_id
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// Restart and reconnect/refresh maintenance also enumerates bare guards,
    /// not just removed-session journals. Current focus is never substituted.
    fn recover_engram_authority_runs(&self, session_id: &str, budget: Duration) {
        let deadline = std::time::Instant::now() + budget.min(Duration::from_secs(2));
        let (store, runs) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(store) =
                Self::engram_binding_target_for_session_shape_locked(&inner, session_id, true)
                    .ok()
                    .flatten()
                    .and_then(|target| target.settings.authority_store_key)
            else {
                return;
            };
            let repairs = inner
                .engram_work_naming_history
                .iter()
                .filter(|history| {
                    history.store == store
                        && history.epoch != ENGRAM_NAMING_HISTORY_EPOCH
                        && history.known_generation > 0
                        && history.unresolved_runs.is_empty()
                        && history.transition.is_none()
                })
                .map(|history| (history.work_id.clone(), history.recovery_reason.clone()))
                .collect::<Vec<_>>();
            for (work, reason) in repairs {
                for record in &mut inner.sessions {
                    if record
                        .engram
                        .work_binding
                        .as_ref()
                        .is_some_and(|binding| binding.work_id == work)
                    {
                        record.engram.set_pending_source_root_line(format!(
                            "[TermAl] Retained source-root history requires repair before a fresh evaluation: {}. Source, test and evaluation evidence remain withheld; current absence cannot erase unexplained historical naming.",
                            reason.as_deref().unwrap_or("the original canonical association is unavailable")));
                    }
                }
            }
            let mut runs = inner
                .engram_work_naming_history
                .iter()
                .filter(|history| {
                    history.store == store
                        && engram_authority_work_unresolved(&inner, &store, &history.work_id)
                })
                .filter_map(|history| {
                    history
                        .unresolved_runs
                        .first()
                        .cloned()
                        .or_else(|| {
                            inner
                                .engram_named_root_journal
                                .iter()
                                .find(|journal| {
                                    journal.store == store
                                        && engram_journal_names_work(journal, &history.work_id)
                                        && journal.requires_reconciliation()
                                })
                                .and_then(|journal| journal.read_binding.clone())
                        })
                        .or_else(|| {
                            history
                                .transition
                                .as_ref()
                                .filter(|owner| owner.phase != EngramAuthorityPhase::Published)
                                .map(|owner| owner.binding.clone())
                        })
                })
                .collect::<Vec<_>>();
            runs.sort_by(|a, b| a.claim_id.cmp(&b.claim_id));
            if !runs.is_empty() {
                let offset = inner
                    .engram_authority_read_cursor
                    .get(&store)
                    .map_or(0, |last| {
                        runs.partition_point(|binding| binding.claim_id <= *last) % runs.len()
                    });
                runs.rotate_left(offset);
            }
            runs.truncate(8);
            (store, runs)
        };
        for binding in runs {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            self.inner
                .lock()
                .expect("state mutex poisoned")
                .engram_authority_read_cursor
                .insert(store.clone(), binding.claim_id.clone());
            if let Ok(owner) = self.prepare_engram_authority_until(&store, &binding, deadline) {
                let _ = self.recover_engram_authority_batch_until(&store, &owner, deadline, true);
                let _ = self.publish_engram_authority_until(&store, &owner, deadline);
            }
        }
    }

    /// Authority callers must never use the generic synchronous persistence
    /// fallback under StateInner. The enclosing owner supplies durability.
    fn commit_engram_root_locked(&self, inner: &mut StateInner) -> anyhow::Result<u64> {
        inner.revision = inner
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("state revision exhausted"))?;
        let _ = self.persist_tx.send(PersistRequest::Delta);
        self.publish_state_locked(inner);
        Ok(inner.revision)
    }

    fn read_engram_authority_fact(
        &self,
        target: &EngramBindingTarget,
        binding: &EngramControlWorkBinding,
        budget: Duration,
    ) -> Result<EngramNamedRootReadResponse, ApiError> {
        self.read_engram_authority_fact_until(target, binding, std::time::Instant::now() + budget)
    }

    fn read_engram_authority_fact_until(
        &self,
        target: &EngramBindingTarget,
        binding: &EngramControlWorkBinding,
        deadline: std::time::Instant,
    ) -> Result<EngramNamedRootReadResponse, ApiError> {
        self.read_engram_authority_fact_classified_until(target, binding, deadline)
            .map_err(EngramAuthorityReadFailure::into_api)
    }

    fn read_engram_authority_fact_classified_until(
        &self,
        target: &EngramBindingTarget,
        binding: &EngramControlWorkBinding,
        deadline: std::time::Instant,
    ) -> Result<EngramNamedRootReadResponse, EngramAuthorityReadFailure> {
        let store = target
            .settings
            .authority_store_key
            .as_ref()
            .ok_or_else(|| {
                EngramAuthorityReadFailure::AdmissionInvalid(EngramTransportError::local_state(
                    "named-root reader has no authority store",
                ))
            })?;
        let routing_token = target.routing_token.as_ref().ok_or_else(|| {
            EngramAuthorityReadFailure::AdmissionInvalid(EngramTransportError::local_state(
                "named-root reader has no routing authority",
            ))
        })?;
        self.validate_engram_authority_reader(target, store)
            .map_err(|error| {
                EngramAuthorityReadFailure::AdmissionInvalid(EngramTransportError::local_state(
                    error.message,
                ))
            })?;
        let budget = deadline.saturating_duration_since(std::time::Instant::now());
        if budget.is_zero() {
            return Err(EngramAuthorityReadFailure::Source(ApiError::conflict(
                "named-root canonical read budget was spent; authority remains withheld",
            )));
        }
        let read = target
            .adapter
            .request(
                &target.connection,
                &EngramControlRequest::NamedRootRead {
                    routing_token: routing_token.clone(),
                    run_id: binding.run_id.clone(),
                    claim_id: binding.claim_id.clone(),
                },
                budget.min(target.settings.call_timeout()),
            )
            .and_then(parse_engram_result::<EngramNamedRootReadResponse>)
            .map_err(|error| {
                if engram_status_error_requires_fresh_bind(&error) {
                    EngramAuthorityReadFailure::AdmissionInvalid(error)
                } else {
                    EngramAuthorityReadFailure::Source(ApiError::bad_gateway(format!(
                        "named-root canonical read is unconfirmed: {error}"
                    )))
                }
            })?;
        #[cfg(test)]
        if let Some(hook) =
            TEST_ENGRAM_DURING_ROOT_LIFECYCLE_READ.with(|hook| hook.borrow_mut().take())
        {
            hook();
        }
        read.validate_authority(store, binding)
            .map_err(EngramAuthorityReadFailure::Source)?;
        self.validate_engram_authority_reader(target, store)
            .map_err(|error| {
                EngramAuthorityReadFailure::AdmissionInvalid(EngramTransportError::local_state(
                    error.message,
                ))
            })?;
        Ok(read)
    }

    fn validate_engram_authority_reader(
        &self,
        target: &EngramBindingTarget,
        store: &EngramAuthorityStoreKey,
    ) -> Result<(), ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let current = Self::engram_binding_target_for_session_shape_locked(
            &inner,
            &target.connection.session_id,
            true,
        )
        .ok()
        .flatten()
        .ok_or_else(|| ApiError::conflict("named-root reader authority disappeared"))?;
        if current.connection != target.connection
            || current.routing_token != target.routing_token
            || current.settings.authority_store_key.as_ref() != Some(store)
        {
            return Err(ApiError::conflict(
                "named-root canonical read belongs to an earlier authority connection",
            ));
        }
        Ok(())
    }

    /// Install the withholding obligation before authority-changing I/O.
    /// Persistence uncertainty retains it; it never revives the old image.
    #[cfg(test)]
    fn prepare_engram_authority(
        &self,
        store: &EngramAuthorityStoreKey,
        binding: &EngramControlWorkBinding,
        budget: Duration,
    ) -> Result<EngramAuthorityTransition, ApiError> {
        self.prepare_engram_authority_until(store, binding, std::time::Instant::now() + budget)
    }

    fn prepare_engram_authority_until(
        &self,
        store: &EngramAuthorityStoreKey,
        binding: &EngramControlWorkBinding,
        deadline: std::time::Instant,
    ) -> Result<EngramAuthorityTransition, ApiError> {
        let recovery = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .engram_work_naming_history
                .iter()
                .find(|history| &history.store == store && history.work_id == binding.work_id)
                .and_then(|history| history.transition.as_ref())
                .filter(|transition| transition.phase == EngramAuthorityPhase::Candidate)
                .map(|_| EngramAuthorityImage::capture(&inner, store, &binding.work_id))
                .transpose()?
        };
        if let Some(image) = recovery {
            self.confirm_engram_authority_image_until(&image, deadline)?;
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if !image.still_owned(&inner) {
                return Err(ApiError::conflict("recovery candidate was superseded"));
            }
            inner
                .engram_work_naming_history
                .iter_mut()
                .find(|history| &history.store == store && history.work_id == binding.work_id)
                .expect("acknowledged recovery history")
                .transition
                .as_mut()
                .expect("acknowledged recovery owner")
                .phase = EngramAuthorityPhase::Published;
        }
        let image = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index =
                match inner.engram_work_naming_history.iter().position(|history| {
                    &history.store == store && history.work_id == binding.work_id
                }) {
                    Some(index) => index,
                    None => {
                        inner
                            .engram_work_naming_history
                            .push(EngramWorkNamingHistory {
                                store: store.clone(),
                                work_id: binding.work_id.clone(),
                                revision: 0,
                                known_generation: 0,
                                epoch: 0,
                                frontier: None,
                                owner_version: 0,
                                transition: None,
                                unresolved_runs: Vec::new(),
                                proofs: Vec::new(),
                                recovery_reason: None,
                            });
                        inner.engram_work_naming_history.len() - 1
                    }
                };
            let journal_runs = inner
                .engram_named_root_journal
                .iter()
                .filter(|journal| {
                    &journal.store == store
                        && engram_journal_names_work(journal, &binding.work_id)
                        && journal.requires_reconciliation()
                })
                .filter_map(|journal| journal.read_binding.clone())
                .collect::<Vec<_>>();
            let history = &mut inner.engram_work_naming_history[index];
            for retained in journal_runs {
                engram_retain_recovery_run(&mut history.unresolved_runs, &retained);
            }
            if let Some(previous) = history
                .transition
                .as_ref()
                .filter(|previous| previous.phase != EngramAuthorityPhase::Published)
            {
                engram_retain_recovery_run(&mut history.unresolved_runs, &previous.binding);
            }
            engram_retain_recovery_run(&mut history.unresolved_runs, binding);
            history.recovery_reason.get_or_insert_with(|| {
                "canonical naming history for a retained run still needs an acknowledged read"
                    .to_owned()
            });
            history.owner_version = history
                .owner_version
                .checked_add(1)
                .ok_or_else(|| ApiError::internal("named-root owner version exhausted"))?;
            history.transition = Some(EngramAuthorityTransition {
                id: Uuid::new_v4().to_string(),
                version: history.owner_version,
                binding: binding.clone(),
                phase: EngramAuthorityPhase::Prepared,
            });
            EngramAuthorityImage::capture(&inner, store, &binding.work_id)?
        };
        self.confirm_engram_authority_image_until(&image, deadline)?;
        Ok(image.history.transition.expect("prepared owner"))
    }

    fn confirm_engram_authority_image_until(
        &self,
        image: &EngramAuthorityImage,
        deadline: std::time::Instant,
    ) -> Result<(), ApiError> {
        let budget = deadline.saturating_duration_since(std::time::Instant::now());
        let (fence, waiter) = PersistFence::new(
            PersistFenceTarget::EngramWorkAuthority(Box::new(image.clone())),
            deadline,
        );
        if self
            .persist_tx
            .send(PersistRequest::Fence(Box::new(fence)))
            .is_ok()
        {
            waiter.wait().map_err(|error| ApiError::internal(format!(
                "named-root authority persistence is unconfirmed ({error:?}); recovery remains withheld")))?;
        } else {
            // Production authority cannot settle through a stopped writer.
            // Manually constructed test states use the same SQLite delta
            // writer off-lock; they never stand in for connected-writer proof.
            if !engram_authority_manual_writer_allowed() {
                return Err(ApiError::internal(
                    "named-root authority writer stopped; recovery remains withheld",
                ));
            }
            #[cfg(test)]
            {
                let delta = collect_persist_delta_from_shared_state(&self.inner, 0);
                if !image.matches_metadata(&delta.metadata) {
                    return Err(ApiError::conflict(
                        "named-root authority image was superseded",
                    ));
                }
                let mut cache = SqlitePersistConnectionCache::new();
                persist_delta_via_cache(&mut cache, self.persistence_path.as_path(), &delta)
                    .map_err(|error| {
                        ApiError::internal(format!(
                            "named-root authority persistence is unconfirmed: {error:#}"
                        ))
                    })?;
            }
        }
        let inner = self.inner.lock().expect("state mutex poisoned");
        if !image.still_owned(&inner) {
            return Err(ApiError::conflict(
                "named-root acknowledgement belongs to a superseded authority image",
            ));
        }
        if std::time::Instant::now() >= deadline {
            return Err(ApiError::conflict(format!(
                "named-root acknowledgement exceeded its remaining {} ms authority budget",
                budget.as_millis()
            )));
        }
        Ok(())
    }

    /// Candidates stay guarded until this exact work image commits. The guard
    /// on disk may redundantly survive the acknowledgement; restoration must
    /// recover that candidate rather than treating it as published.
    #[cfg(test)]
    fn publish_engram_authority(
        &self,
        store: &EngramAuthorityStoreKey,
        owner: &EngramAuthorityTransition,
        budget: Duration,
    ) -> Result<(), ApiError> {
        self.publish_engram_authority_until(store, owner, std::time::Instant::now() + budget)
    }

    fn publish_engram_authority_until(
        &self,
        store: &EngramAuthorityStoreKey,
        owner: &EngramAuthorityTransition,
        deadline: std::time::Instant,
    ) -> Result<(), ApiError> {
        self.recover_engram_authority_batch_until(store, owner, deadline, false)?;
        let image = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            engram_compact_root_journal(&mut inner);
            let pending_write = inner.engram_named_root_journal.iter().any(|journal| {
                &journal.store == store
                    && engram_journal_names_work(journal, &owner.binding.work_id)
                    && journal.requires_reconciliation()
            });
            let history = inner
                .engram_work_naming_history
                .iter_mut()
                .find(|history| &history.store == store && history.work_id == owner.binding.work_id)
                .ok_or_else(|| ApiError::conflict("named-root authority owner disappeared"))?;
            let current = history
                .transition
                .as_mut()
                .filter(|current| current.id == owner.id && current.version == owner.version)
                .ok_or_else(|| {
                    ApiError::conflict("named-root candidate belongs to an earlier owner")
                })?;
            current.phase = EngramAuthorityPhase::Candidate;
            if pending_write && history.recovery_reason.is_none() {
                history.recovery_reason = Some("an exact naming intent still needs receipt replay or covering lifecycle recovery".to_owned());
            }
            EngramAuthorityImage::capture(&inner, store, &owner.binding.work_id)?
        };
        self.confirm_engram_authority_image_until(&image, deadline)?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if !image.still_owned(&inner) {
            return Err(ApiError::conflict(
                "named-root candidate changed after acknowledgement",
            ));
        }
        let history = inner
            .engram_work_naming_history
            .iter_mut()
            .find(|history| &history.store == store && history.work_id == owner.binding.work_id)
            .expect("acknowledged history");
        history
            .transition
            .as_mut()
            .expect("acknowledged transition")
            .phase = EngramAuthorityPhase::Published;
        let reason = history.recovery_reason.clone();
        if engram_authority_work_unresolved(&inner, store, &owner.binding.work_id) {
            let line = format!(
                "[TermAl] Source-root authority recovery remains incomplete: {}. Source, test and evaluation evidence are withheld. Retry the original naming request or refresh authority; a missing historical association requires repair before a fresh evaluation.",
                reason
                    .as_deref()
                    .unwrap_or("retained canonical history is unresolved")
            );
            for record in &mut inner.sessions {
                if record
                    .engram
                    .work_binding
                    .as_ref()
                    .is_some_and(|binding| binding.work_id == owner.binding.work_id)
                    || record
                        .engram
                        .active_turn_naming_identity
                        .as_ref()
                        .is_some_and(|(active_store, work)| {
                            active_store == store && work == &owner.binding.work_id
                        })
                {
                    record.engram.set_pending_source_root_line(line.clone());
                }
            }
        }
        // Published is an already-acknowledged marker. Either durable side of
        // this optional cleanup is conservative; it removes no history proof.
        drop(inner);
        if self.persist_tx.send(PersistRequest::Delta).is_err() {
            #[cfg(test)]
            {
                // Manual fixtures have no worker to drain optional cleanup.
                // Preserve their persisted marker without doing I/O under
                // StateInner. A failed cleanup retains the durable candidate.
                if !engram_authority_manual_writer_allowed() {
                    return Ok(());
                }
                let delta = collect_persist_delta_from_shared_state(&self.inner, 0);
                let mut cache = SqlitePersistConnectionCache::new();
                let _ =
                    persist_delta_via_cache(&mut cache, self.persistence_path.as_path(), &delta);
            }
        }
        Ok(())
    }
}
