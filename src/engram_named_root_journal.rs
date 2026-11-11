// Owns named-root journal retention, compaction and bounded host-memory counts.
// Does not own naming authority, lifecycle reads, publication or retry custody.
// Retention was split out of engram_named_root_binding.rs.

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum EngramRootJournalRetention {
    UnpublishedRetirement,
    PendingIntent,
    UnsettledReconciliation,
    UnresolvedRetirement,
    Evaluator,
    ConfirmedBinding,
    SelectedRoot,
    RuntimeBinding,
    RuntimeRoot,
    RuntimeOpening,
    ClosingJob,
    ObservationReport,
    CapturePending,
    CaptureLockUnavailable,
    CapturedGeneration,
}

impl EngramRootJournalRetention {
    fn name(self) -> &'static str {
        match self {
            Self::UnpublishedRetirement => "unpublishedRetirement",
            Self::PendingIntent => "pendingIntent",
            Self::UnsettledReconciliation => "unsettledReconciliation",
            Self::UnresolvedRetirement => "unresolvedRetirement",
            Self::Evaluator => "evaluator",
            Self::ConfirmedBinding => "confirmedBinding",
            Self::SelectedRoot => "selectedRoot",
            Self::RuntimeBinding => "runtimeBinding",
            Self::RuntimeRoot => "runtimeRoot",
            Self::RuntimeOpening => "runtimeOpening",
            Self::ClosingJob => "closingJob",
            Self::ObservationReport => "observationReport",
            Self::CapturePending => "capturePending",
            Self::CaptureLockUnavailable => "captureLockUnavailable",
            Self::CapturedGeneration => "capturedGeneration",
        }
    }
}

impl Serialize for EngramRootJournalRetention {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

/// Counts are exclusive: the first applicable protection owns each entry.
/// No claim, work, session, path, command or source payload is exposed.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramRootJournalCensus {
    limit: usize,
    entries: usize,
    selected_roots: usize,
    retained: usize,
    reclaimable: usize,
    retryable_closing_jobs: usize,
    terminal_closing_jobs: usize,
    primary_reasons: BTreeMap<EngramRootJournalRetention, usize>,
}

impl EngramRootJournalCensus {
    fn capacity_message(&self) -> String {
        let reasons = self
            .primary_reasons
            .iter()
            .map(|(reason, count)| format!("{}={count}", reason.name()))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "named-root journal is full of protected transitions: entries={}, limit={}, \
             retained={}, reclaimable={}, primary retention classes=[{}]. \
             Let pending captures or capture-result publication finish; settle naming, \
             observation or evaluator retry custody through its existing owner; obtain \
             authoritative lifecycle reads for retired claims. Read \
             /api/diagnostics/named-root-journal for current counts. \
             No protected transition was discarded.",
            self.entries, self.limit, self.retained, self.reclaimable, reasons,
        )
    }
}

/// One inventory image for a pass. Entries keep their existing first-protection
/// order in `engram_root_journal_retention`; the index supplies membership only.
struct EngramJournalCustodyIndex<'a> {
    unpublished: HashSet<(EngramAuthorityStoreKey, String)>,
    evaluators: HashSet<(EngramAuthorityStoreKey, String)>,
    evaluator_work_after: HashMap<(EngramAuthorityStoreKey, String), u64>,
    selected: HashSet<(EngramAuthorityStoreKey, String)>,
    runtime: HashMap<String, EngramRootJournalRetention>,
    captures: Vec<&'a EngramBasisCapture>,
    capture_image: std::cell::OnceCell<EngramJournalCaptureImage>,
    retryable_jobs: usize,
    terminal_jobs: usize,
}

#[derive(Default)]
struct EngramJournalCaptureImage {
    generations: HashSet<Option<i64>>,
    pending: bool,
    unavailable: bool,
}

impl<'a> EngramJournalCustodyIndex<'a> {
    fn new(inner: &'a StateInner) -> Self {
        use EngramRootJournalRetention as Keep;
        let mut index = Self {
            unpublished: HashSet::new(),
            evaluators: HashSet::new(),
            evaluator_work_after: HashMap::new(),
            selected: HashSet::new(),
            runtime: HashMap::new(),
            captures: Vec::new(),
            capture_image: std::cell::OnceCell::new(),
            retryable_jobs: 0,
            terminal_jobs: 0,
        };
        for history in &inner.engram_work_naming_history {
            for notice in &history.retirements {
                // Published notices carry their own proof and delivery identity.
                if !notice.published {
                    index.unpublished.insert((
                        notice.selection.store.clone(),
                        notice.selection.claim_id.clone(),
                    ));
                }
            }
        }
        for delegation in &inner.delegations {
            let Some(target) = &delegation.acceptance_evaluation else {
                continue;
            };
            let Some(store) = &target.store else { continue };
            if !matches!(
                delegation.status,
                DelegationStatus::Queued | DelegationStatus::Running
            ) && !matches!(
                target.submission,
                AcceptanceEvaluationSubmission::Pending { .. }
                    | AcceptanceEvaluationSubmission::Unconfirmed { .. }
            ) {
                continue;
            }
            if let Some(claim) = &target.source_claim {
                index
                    .evaluators
                    .insert((store.clone(), claim.claim_id.clone()));
                let after = claim.named_generation_at_request.unwrap_or(0);
                index
                    .evaluator_work_after
                    .entry((store.clone(), claim.work_id.clone()))
                    .and_modify(|old| *old = (*old).min(after))
                    .or_insert(after);
            }
            if let Some(root) = &target.source_root {
                index
                    .evaluators
                    .insert((store.clone(), root.claim_id.clone()));
            }
        }
        for root in &inner.engram_work_source_roots {
            index
                .selected
                .insert((root.store.clone(), root.claim_id.clone()));
        }
        for record in &inner.sessions {
            #[cfg(test)]
            ENGRAM_JOURNAL_SESSION_VISITS.with(|count| count.set(count.get() + 1));
            let engram = &record.engram;
            if let Some(binding) = &engram.work_binding {
                index.keep(&binding.claim_id, Keep::RuntimeBinding);
            }
            if let Some(root) = engram.observed_source_root() {
                index.keep(&root.claim_id, Keep::RuntimeRoot);
            }
            if let Some(opening) = engram.observed_opening() {
                index.keep(&opening.binding.claim_id, Keep::RuntimeOpening);
            }
            for job in &engram.unadmitted_closing_jobs {
                // Terminal rows keep diagnostics, but the worker never retries them.
                if job.terminal_failure.is_some() {
                    index.terminal_jobs += 1;
                } else {
                    index.retryable_jobs += 1;
                    if let Some(binding) = &job.scope.binding {
                        index.keep(&binding.claim_id, Keep::ClosingJob);
                    }
                }
            }
            for report in &engram.unadmitted_reports {
                for envelope in &report.envelopes {
                    if let Some(claim) = envelope
                        .request
                        .get("binding")
                        .and_then(|binding| binding.get("claim_id"))
                        .and_then(Value::as_str)
                    {
                        index.keep(claim, Keep::ObservationReport);
                    }
                }
            }
            for check in &engram.active_turn_checks {
                index.captures.push(&check.start_basis);
                if let Some(end) = &check.end {
                    index.captures.push(&end.end_basis);
                }
            }
        }
        index
    }

    fn keep(&mut self, claim: &str, reason: EngramRootJournalRetention) {
        // Runtime classes have the same priority as their enum order. A later
        // session must not override higher-priority custody found elsewhere.
        self.runtime
            .entry(claim.to_owned())
            .and_modify(|old| *old = (*old).min(reason))
            .or_insert(reason);
    }

    fn capture_retention(
        &self,
        journal: &EngramNamedRootJournal,
    ) -> Option<EngramRootJournalRetention> {
        use EngramRootJournalRetention as Keep;
        // Higher-priority custody never needs a capture lock. Once needed,
        // sample each result at most once without waiting, retaining uncertain
        // custody conservatively for the rest of this pass.
        let image = self.capture_image.get_or_init(|| {
            let mut image = EngramJournalCaptureImage::default();
            for capture in &self.captures {
                #[cfg(test)]
                engram_journal_count_capture_read();
                match capture.result.try_lock() {
                    Err(_) => image.unavailable = true,
                    Ok(result) => match result.as_ref() {
                        None => image.pending = true,
                        Some(Some(basis)) => {
                            image.generations.insert(basis.source_root_generation);
                        }
                        Some(None) => {}
                    },
                }
            }
            image
        });
        if journal.confirmed.as_ref().is_some_and(|(event, _)| {
            image
                .generations
                .contains(&i64::try_from(event.root.generation).ok())
        }) {
            return Some(Keep::CapturedGeneration);
        }
        if image.pending {
            return Some(Keep::CapturePending);
        }
        if image.unavailable {
            return Some(Keep::CaptureLockUnavailable);
        }
        None
    }
}

/// Single production classification predicate for compaction and diagnostics.
fn engram_root_journal_retention(
    index: &EngramJournalCustodyIndex<'_>,
    journal: &EngramNamedRootJournal,
) -> Option<EngramRootJournalRetention> {
    use EngramRootJournalRetention as Keep;
    let key = (journal.store.clone(), journal.claim_id.clone());
    if index.unpublished.contains(&key) {
        return Some(Keep::UnpublishedRetirement);
    }
    if journal.requires_reconciliation() {
        return Some(if journal.pending.is_some() {
            Keep::PendingIntent
        } else if journal
            .reconciliation
            .as_ref()
            .is_some_and(|read| !read.settled)
        {
            Keep::UnsettledReconciliation
        } else {
            Keep::UnresolvedRetirement
        });
    }
    if index.evaluators.contains(&key)
        || !journal.obsolete
            && journal.confirmed.as_ref().is_some_and(|(event, _)| {
                index
                    .evaluator_work_after
                    .get(&(journal.store.clone(), event.root.work_id.clone()))
                    .is_some_and(|after| event.root.generation > *after)
            })
    {
        return Some(Keep::Evaluator);
    }
    if journal
        .confirmed
        .as_ref()
        .is_some_and(|(event, _)| event.kind == EngramNamedRootKind::Bound && !journal.obsolete)
    {
        return Some(Keep::ConfirmedBinding);
    }
    if index.selected.contains(&key) {
        return Some(Keep::SelectedRoot);
    }
    // Known claim custody across the host outranks unrelated uncertain captures.
    if let Some(reason) = index.runtime.get(&journal.claim_id) {
        return Some(*reason);
    }
    index.capture_retention(journal)
}

fn engram_root_journal_empty_census(
    inner: &StateInner,
    index: &EngramJournalCustodyIndex<'_>,
) -> EngramRootJournalCensus {
    let mut census = EngramRootJournalCensus {
        limit: engram_named_root_journal_limit(inner),
        entries: inner.engram_named_root_journal.len(),
        selected_roots: inner.engram_work_source_roots.len(),
        retained: 0,
        reclaimable: 0,
        retryable_closing_jobs: 0,
        terminal_closing_jobs: 0,
        primary_reasons: BTreeMap::new(),
    };
    census.retryable_closing_jobs = index.retryable_jobs;
    census.terminal_closing_jobs = index.terminal_jobs;
    census
}

fn engram_root_journal_census(inner: &StateInner) -> EngramRootJournalCensus {
    let index = EngramJournalCustodyIndex::new(inner);
    let mut census = engram_root_journal_empty_census(inner, &index);
    for journal in inner.engram_named_root_journal.iter() {
        match engram_root_journal_retention(&index, journal) {
            Some(reason) => {
                census.retained += 1;
                *census.primary_reasons.entry(reason).or_default() += 1;
            }
            None => census.reclaimable += 1,
        }
    }
    census
}

/// Only settled, unused history is disposable. In particular a capture
/// worker still resolving its basis conservatively protects history until
/// that basis can be inspected without waiting under the state lock.
fn engram_compact_root_journal(inner: &mut StateInner) {
    let _ = engram_compact_root_journal_with_census(inner);
}

/// Admission uses this same pass's count image if it still has no room. In
/// particular it need not rebuild every inventory for a refusal's census.
fn engram_compact_root_journal_with_census(inner: &mut StateInner) -> Option<EngramRootJournalCensus> {
    // An empty journal cannot refuse admission. Avoid walking or allocating
    // custody inventories on this common publication and unbind path.
    if inner.engram_named_root_journal.is_empty() {
        return None;
    }
    let index = EngramJournalCustodyIndex::new(inner);
    let mut census = engram_root_journal_empty_census(inner, &index);
    let mut retained = Vec::new();
    for journal in &inner.engram_named_root_journal {
        if let Some(reason) = engram_root_journal_retention(&index, journal) {
            retained.push(journal.clone());
            census.retained += 1;
            *census.primary_reasons.entry(reason).or_default() += 1;
        }
    }
    // This describes the retained image, not the history removed by this pass.
    census.entries = retained.len();
    drop(index);
    inner.engram_named_root_journal = retained.into();
    Some(census)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramRootJournalDiagnostics {
    server_instance_id: String,
    revision: u64,
    journal: EngramRootJournalCensus,
}

async fn get_engram_root_journal_diagnostics(
    State(state): State<AppState>,
) -> Result<Json<EngramRootJournalDiagnostics>, ApiError> {
    run_blocking_api(move || {
        let inner = state.inner.lock().expect("state mutex poisoned");
        Ok(Json(EngramRootJournalDiagnostics {
            server_instance_id: state.server_instance_id.clone(),
            revision: inner.revision,
            journal: engram_root_journal_census(&inner),
        }))
    })
    .await
}

#[cfg(test)]
thread_local! {
    static ENGRAM_JOURNAL_SESSION_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static ENGRAM_JOURNAL_CAPTURE_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn engram_journal_count_capture_read() {
    ENGRAM_JOURNAL_CAPTURE_READS.with(|count| count.set(count.get() + 1));
}
