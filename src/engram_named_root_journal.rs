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

fn engram_root_journal_capture_retention(
    capture: &EngramBasisCapture,
    event: Option<&EngramNamedRootEvent>,
) -> Option<EngramRootJournalRetention> {
    let Ok(result) = capture.result.try_lock() else {
        // A busy or poisoned result cannot establish that capture custody ended.
        return Some(EngramRootJournalRetention::CaptureLockUnavailable);
    };
    match result.as_ref() {
        None => Some(EngramRootJournalRetention::CapturePending),
        Some(Some(basis))
            if event.is_some_and(|event| {
                basis.source_root_generation == i64::try_from(event.root.generation).ok()
            }) =>
        {
            Some(EngramRootJournalRetention::CapturedGeneration)
        }
        _ => None,
    }
}

/// The single retention predicate used by compaction and diagnostics. The
/// order identifies the first protection, not every overlapping reference.
fn engram_root_journal_retention(
    inner: &StateInner,
    journal: &EngramNamedRootJournal,
) -> Option<EngramRootJournalRetention> {
    use EngramRootJournalRetention as Keep;
    if inner
        .engram_work_naming_history
        .iter()
        .flat_map(|history| &history.retirements)
        .any(|notice| {
            !notice.published
                && notice.selection.store == journal.store
                && notice.selection.claim_id == journal.claim_id
        })
    {
        // Published notices carry their own proof and delivery identity.
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
    if inner.delegations.iter().any(|delegation| {
        delegation
            .acceptance_evaluation
            .as_ref()
            .is_some_and(|target| {
                target.store.as_ref() == Some(&journal.store)
                    && (target.source_claim.as_ref().is_some_and(|claim| {
                        claim.claim_id == journal.claim_id
                            || !journal.obsolete
                                && journal.confirmed.as_ref().is_some_and(|(event, _)| {
                                    event.root.work_id == claim.work_id
                                        && event.root.generation
                                            > claim.named_generation_at_request.unwrap_or(0)
                                })
                    }) || target
                        .source_root
                        .as_ref()
                        .is_some_and(|root| root.claim_id == journal.claim_id))
                    && (matches!(
                        delegation.status,
                        DelegationStatus::Queued | DelegationStatus::Running
                    ) || matches!(
                        target.submission,
                        AcceptanceEvaluationSubmission::Pending { .. }
                            | AcceptanceEvaluationSubmission::Unconfirmed { .. }
                    ))
            })
    }) {
        return Some(Keep::Evaluator);
    }
    if journal
        .confirmed
        .as_ref()
        .is_some_and(|(event, _)| event.kind == EngramNamedRootKind::Bound && !journal.obsolete)
    {
        return Some(Keep::ConfirmedBinding);
    }
    if inner
        .engram_work_source_roots
        .iter()
        .any(|root| root.store == journal.store && root.claim_id == journal.claim_id)
    {
        return Some(Keep::SelectedRoot);
    }
    // Known claim-specific custody outranks uncertain captures across the
    // whole host. Session insertion order must not suggest that completing
    // an unrelated capture would free an entry still held by another session.
    if inner.sessions.iter().any(|record| {
        record
            .engram
            .work_binding
            .as_ref()
            .is_some_and(|binding| binding.claim_id == journal.claim_id)
    }) {
        return Some(Keep::RuntimeBinding);
    }
    if inner.sessions.iter().any(|record| {
        record
            .engram
            .observed_source_root()
            .is_some_and(|root| root.claim_id == journal.claim_id)
    }) {
        return Some(Keep::RuntimeRoot);
    }
    if inner.sessions.iter().any(|record| {
        record
            .engram
            .observed_opening()
            .is_some_and(|opening| opening.binding.claim_id == journal.claim_id)
    }) {
        return Some(Keep::RuntimeOpening);
    }
    if inner.sessions.iter().any(|record| {
        record.engram.unadmitted_closing_jobs.iter().any(|job| {
            // Terminal rows preserve diagnostics; the worker never retries
            // them, so they no longer own executable journal custody.
            job.terminal_failure.is_none()
                && job
                    .scope
                    .binding
                    .as_ref()
                    .is_some_and(|binding| binding.claim_id == journal.claim_id)
        })
    }) {
        return Some(Keep::ClosingJob);
    }
    if inner.sessions.iter().any(|record| {
        record.engram.unadmitted_reports.iter().any(|report| {
            report.envelopes.iter().any(|envelope| {
                envelope
                    .request
                    .get("binding")
                    .and_then(|binding| binding.get("claim_id"))
                    .and_then(Value::as_str)
                    == Some(journal.claim_id.as_str())
            })
        })
    }) {
        return Some(Keep::ObservationReport);
    }
    let event = journal.confirmed.as_ref().map(|(event, _)| event);
    let mut pending = false;
    let mut unavailable = false;
    for record in &inner.sessions {
        for check in &record.engram.active_turn_checks {
            for capture in std::iter::once(&check.start_basis)
                .chain(check.end.as_ref().map(|end| &end.end_basis))
            {
                match engram_root_journal_capture_retention(capture, event) {
                    Some(Keep::CapturedGeneration) => return Some(Keep::CapturedGeneration),
                    Some(Keep::CapturePending) => pending = true,
                    Some(Keep::CaptureLockUnavailable) => unavailable = true,
                    // This helper yields only capture protections.
                    _ => {}
                }
            }
        }
    }
    if pending {
        return Some(Keep::CapturePending);
    }
    if unavailable {
        return Some(Keep::CaptureLockUnavailable);
    }
    None
}

fn engram_root_journal_census(inner: &StateInner) -> EngramRootJournalCensus {
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
    for job in inner
        .sessions
        .iter()
        .flat_map(|record| &record.engram.unadmitted_closing_jobs)
    {
        if job.terminal_failure.is_some() {
            census.terminal_closing_jobs += 1;
        } else {
            census.retryable_closing_jobs += 1;
        }
    }
    for journal in inner.engram_named_root_journal.iter() {
        match engram_root_journal_retention(inner, journal) {
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
    let retained: Vec<_> = inner
        .engram_named_root_journal
        .iter()
        .filter(|journal| engram_root_journal_retention(inner, journal).is_some())
        .cloned()
        .collect();
    inner.engram_named_root_journal = retained.into();
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
