// Owns the settlement of a work's retained recovery runs: which definitive
// Engram refusals retire a run, the durable record a retirement leaves, how a
// retirement settles the run's root journals, the store-integrity alarm for a
// damaged store, the guard that keeps a retired run from being retained again,
// the bounded background reads at host startup and at turn opening, and the
// naming's report of runs that stay retained. Does not own the canonical read
// or the recovery batch (engram_root_authority.rs) or the naming history itself
// (engram_named_root_binding.rs). New module.

/// The Engram error code of a refused named-root read.
const ENGRAM_NAMED_ROOT_READ_REFUSED_CODE: &str = "named_root_read_refused";

/// The Engram error code a damaged work projection is reported under, and the
/// start of that error's message.
const ENGRAM_STORAGE_ERROR_CODE: &str = "storage_error";
const ENGRAM_INVALID_WORK_PROJECTION: &str = "work projection contains invalid data";

/// The prefix of a recovery reason, and of the host log line, that reports a
/// canonical read showing a damaged store.
const ENGRAM_STORE_INTEGRITY_ALARM: &str = "store integrity alarm";

/// Why Engram refused a named-root read, as its typed `reason` names it. Each
/// is definitive for the exact run and claim that was asked for.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramNamedRootReadRefusal {
    /// No run with this id exists in the store that answered.
    RunUnknown,
    /// The claim is not the claim the run records.
    ClaimNotCurrentForRun,
    /// The run belongs to another project than the reading session's.
    OtherProject,
}

impl EngramNamedRootReadRefusal {
    fn of_reason(reason: &str) -> Option<Self> {
        match reason {
            "run_unknown" => Some(Self::RunUnknown),
            "claim_not_current_for_run" => Some(Self::ClaimNotCurrentForRun),
            "other_project" => Some(Self::OtherProject),
            _ => None,
        }
    }

    /// How a retirement settles the run's root journals: a newer claim
    /// displaced this one, or Engram holds no root for it at all.
    fn journal_retirement(self) -> EngramRootRetirement {
        match self {
            Self::ClaimNotCurrentForRun => EngramRootRetirement::Displaced,
            Self::RunUnknown | Self::OtherProject => EngramRootRetirement::Unbound,
        }
    }
}

/// The definitive refusal `error` carries: Engram's `named_root_read_refused`
/// code with a typed reason, which the wire decoder keeps at the end of the
/// message as ` (reason: …)`. Anything else, including that code without a
/// recognised reason, is not definitive.
fn engram_named_root_read_refusal(
    error: &EngramTransportError,
) -> Option<EngramNamedRootReadRefusal> {
    if error.kind != EngramTransportErrorKind::Remote
        || error.code.as_deref() != Some(ENGRAM_NAMED_ROOT_READ_REFUSED_CODE)
    {
        return None;
    }
    let (_, reason) = error.message.strip_suffix(')')?.rsplit_once(" (reason: ")?;
    EngramNamedRootReadRefusal::of_reason(reason)
}

/// Whether Engram answered the read with its own report of a damaged work
/// projection. Its code is the generic `storage_error`, so the message decides.
fn engram_transport_error_shows_store_damage(error: &EngramTransportError) -> bool {
    error.kind == EngramTransportErrorKind::Remote
        && error.code.as_deref() == Some(ENGRAM_STORAGE_ERROR_CODE)
        && error.message.starts_with(ENGRAM_INVALID_WORK_PROJECTION)
}

/// A retained run Engram definitively refused. It no longer blocks the work's
/// confirmation, never earns credit, and stays here for audit.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramRetiredRecoveryRun {
    binding: EngramControlWorkBinding,
    refusal: EngramNamedRootReadRefusal,
    /// The store whose verified session answered with the refusal.
    store: EngramAuthorityStoreKey,
    /// The refusal as Engram worded it.
    reason: String,
    retired_at: String,
}

impl EngramWorkNamingHistory {
    fn run_is_retired(&self, binding: &EngramControlWorkBinding) -> bool {
        self.retired_runs
            .iter()
            .any(|retired| engram_same_recovery_run(&retired.binding, binding))
    }

    /// Retains `binding` for a canonical read, unless it was retired: a
    /// retired run can never be read, so retaining it again would block the
    /// work for good.
    fn retain_recovery_run(&mut self, binding: &EngramControlWorkBinding) {
        if !self.run_is_retired(binding) {
            engram_retain_recovery_run(&mut self.unresolved_runs, binding);
        }
    }

    /// Records why `binding` stays retained, as a store-integrity alarm when
    /// the read showed damage. Returns whether the reason is new, so the
    /// console reports it once instead of on every pass.
    fn note_unresolved_run(
        &mut self,
        binding: &EngramControlWorkBinding,
        message: &str,
        damaged: bool,
    ) -> bool {
        let reason = format!(
            "{}retained run {} / claim {} remains unresolved: {message}",
            if damaged { format!("{ENGRAM_STORE_INTEGRITY_ALARM}: ") } else { String::new() },
            binding.run_id, binding.claim_id,
        );
        let new = self.recovery_reason.as_deref() != Some(reason.as_str());
        self.recovery_reason = Some(reason);
        new
    }

    /// Settles a run Engram definitively refused under a verified reader of
    /// `store`: it leaves the retained runs and is recorded as retired.
    fn retire_recovery_run(
        &mut self,
        binding: &EngramControlWorkBinding,
        refusal: EngramNamedRootReadRefusal,
        store: &EngramAuthorityStoreKey,
        reason: &str,
    ) {
        self.unresolved_runs
            .retain(|pending| !engram_same_recovery_run(pending, binding));
        if !self.run_is_retired(binding) {
            self.retired_runs.push(EngramRetiredRecoveryRun {
                binding: binding.clone(),
                refusal,
                store: store.clone(),
                reason: reason.to_owned(),
                retired_at: chrono::Utc::now()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            });
        }
        if self.unresolved_runs.is_empty() {
            self.recovery_reason = None;
        }
    }
}

/// Whether the run `binding` reads in `store` was retired: its journal is
/// settled with it and must not reopen.
fn engram_run_retired(
    histories: &[EngramWorkNamingHistory],
    store: &EngramAuthorityStoreKey,
    binding: &EngramControlWorkBinding,
) -> bool {
    histories.iter().any(|history| {
        &history.store == store
            && history.work_id == binding.work_id
            && history.run_is_retired(binding)
    })
}

fn engram_journal_reads_run(
    journal: &EngramNamedRootJournal,
    store: &EngramAuthorityStoreKey,
    binding: &EngramControlWorkBinding,
) -> bool {
    &journal.store == store
        && journal
            .read_binding
            .as_ref()
            .is_some_and(|retained| engram_same_recovery_run(retained, binding))
}

/// Whether every root journal of the refused run can be settled with it: a
/// pending intent only when its reporter abandoned it before the read and is
/// still gone now, under this lock, and no unsettled reconciliation. A live
/// reporter's intent keeps the run retained.
fn engram_refused_run_journals_settle_locked(
    inner: &StateInner,
    store: &EngramAuthorityStoreKey,
    binding: &EngramControlWorkBinding,
    abandoned: &[EngramNamedRootEvent],
) -> bool {
    inner
        .engram_named_root_journal
        .iter()
        .filter(|journal| engram_journal_reads_run(journal, store, binding))
        .all(|journal| {
            journal.pending.as_ref().is_none_or(|intent| {
                abandoned.contains(intent) && !AppState::engram_root_reporter_available(inner, intent)
            }) && journal.reconciliation.as_ref().is_none_or(|read| read.settled)
        })
}

/// Whether `history`'s work needs a background recovery pass: it retains
/// runs, a publication was interrupted (a Candidate has nothing left but to
/// publish), the publication of a retired run's owner was interrupted, or a
/// journal of one of its retired runs was reopened. Another Prepared owner
/// belongs to its naming, which a background pass must not supersede. A
/// Candidate's publisher may still be waiting on its fence: publishing that
/// exact image is safe, and that publisher's caller is refused and can retry.
/// This never runs on a turn's path. A turn keeps its opening capture for its
/// whole length, so a turn that opens before this pass finishes stays unconfirmed
/// and loses its own evidence, and an ordinary turn opening after it is confirmed.
fn engram_history_needs_recovery_locked(inner: &StateInner, history: &EngramWorkNamingHistory) -> bool {
    !history.unresolved_runs.is_empty()
        || history.transition.as_ref().is_some_and(|owner| {
            owner.phase == EngramAuthorityPhase::Candidate
                || owner.phase != EngramAuthorityPhase::Published
                    && history.run_is_retired(&owner.binding)
        })
        || inner.engram_named_root_journal.iter().any(|journal| {
            journal.store == history.store
                && journal.requires_reconciliation()
                && journal.read_binding.as_ref().is_some_and(|binding| {
                    binding.work_id == history.work_id && history.run_is_retired(binding)
                })
        })
}

/// Settles, without a read, the root journals of runs already retired that
/// some path reopened: a retired run is never read again, so nothing else
/// could. A live reporter's pending intent is left as it is.
fn engram_settle_reopened_retired_journals_locked(inner: &mut StateInner) -> bool {
    let shared: &StateInner = inner;
    let reopened = shared
        .engram_named_root_journal
        .iter()
        .filter(|journal| journal.requires_reconciliation())
        .filter(|journal| {
            journal.pending.as_ref().is_none_or(|intent| {
                !AppState::engram_root_reporter_available(shared, intent)
            }) && journal.reconciliation.as_ref().is_none_or(|read| read.settled)
        })
        .filter_map(|journal| {
            let binding = journal.read_binding.as_ref()?;
            shared
                .engram_work_naming_history
                .iter()
                .filter(|history| history.store == journal.store && history.work_id == binding.work_id)
                .flat_map(|history| &history.retired_runs)
                .find(|retired| engram_same_recovery_run(&retired.binding, binding))
                .map(|retired| (journal.store.clone(), binding.clone(), retired.refusal))
        })
        .collect::<Vec<_>>();
    let settled = !reopened.is_empty();
    for (store, binding, refusal) in reopened {
        engram_settle_refused_run_journals_locked(inner, &store, &binding, refusal, "retired-run-settlement");
    }
    settled
}

/// Settles the root journals of a retired run: an abandoned intent is kept
/// as settled readback, never a receipt, and each journal that still needs
/// reconciliation retires the way the refusal says.
fn engram_settle_refused_run_journals_locked(
    inner: &mut StateInner,
    store: &EngramAuthorityStoreKey,
    binding: &EngramControlWorkBinding,
    refusal: EngramNamedRootReadRefusal,
    observed_by: &str,
) {
    let retirement = refusal.journal_retirement();
    let observed_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    for journal in inner
        .engram_named_root_journal
        .iter_mut()
        .filter(|journal| engram_journal_reads_run(journal, store, binding))
    {
        if let Some(intent) = journal.pending.take() {
            journal.reconciliation = Some(EngramRootReconciliation {
                intent,
                state: EngramNamedRootState::None,
                observed_by: observed_by.to_owned(),
                observed_at: observed_at.clone(),
                settled: true,
            });
            journal.retire(retirement);
        } else if journal.requires_reconciliation() {
            journal.retire(retirement);
        }
    }
}

/// The start of the sentence a naming adds when its work still retains runs
/// that no canonical read has answered.
const ENGRAM_RETAINED_RUNS_NOTICE: &str = "Earlier runs of this work are still unresolved";

/// What a naming reports when `work` still retains unread runs after the
/// naming's own reads: its turns stay unconfirmed until they are read back.
/// None when nothing is retained.
fn engram_retained_runs_notice_locked(
    inner: &StateInner,
    store: &EngramAuthorityStoreKey,
    work: &str,
) -> Option<String> {
    let history = inner
        .engram_work_naming_history
        .iter()
        .find(|history| &history.store == store && history.work_id == work)?;
    if history.unresolved_runs.is_empty() {
        return None;
    }
    Some(format!(
        "{ENGRAM_RETAINED_RUNS_NOTICE} ({}), so its turns stay unconfirmed until they are read \
         back: {}.",
        history.unresolved_runs.len(),
        history
            .recovery_reason
            .as_deref()
            .unwrap_or("no canonical read has answered them yet")
    ))
}

/// Whether a session currently holds `binding` as its work binding. Such a run
/// is never retired: a refusal of a binding still in use stays unresolved, so
/// that session's evidence stays withheld instead of being credited.
fn engram_recovery_run_held_locked(inner: &StateInner, binding: &EngramControlWorkBinding) -> bool {
    inner.sessions.iter().any(|record| {
        record
            .engram
            .work_binding
            .as_ref()
            .is_some_and(|held| engram_same_recovery_run(held, binding))
    })
        // A naming session kept aside at restore may load again holding it.
        || inner.engram_work_source_roots.iter().any(|root| {
            root.claim_id == binding.claim_id
                && root.work_id == binding.work_id
                && inner.quarantined_persisted_session_ids.contains(&root.named_by_session)
        })
}

/// The bounded background reads of retained runs. Not persisted: a restarted
/// host reads again.
#[derive(Clone, Debug, Default)]
struct EngramRetainedRunReads {
    /// Stores that had their startup read. A store without a reader yet is
    /// tried again on a later tick; one that had it is not read again.
    startup_read: BTreeSet<EngramAuthorityStoreKey>,
    /// Stores with a read running; at most one per store.
    in_flight: BTreeSet<EngramAuthorityStoreKey>,
}

/// The store a session's work belongs to and that work, when it has both.
fn engram_session_store_and_work_locked(
    inner: &StateInner,
    session_id: &str,
) -> Option<(EngramAuthorityStoreKey, String)> {
    let index = inner.find_session_index(session_id)?;
    let work = inner.sessions[index].engram.work_binding.as_ref()?.work_id.clone();
    let store = engram_project_for_session_locked(inner, session_id)?
        .engram
        .as_ref()?
        .authority_store_key
        .clone()?;
    Some((store, work))
}

/// Releases a store's read flight however its read ends, a panic included.
struct EngramRetainedRunReadFlight {
    state: AppState,
    store: EngramAuthorityStoreKey,
}

impl Drop for EngramRetainedRunReadFlight {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.state.inner.lock() {
            inner.engram_retained_run_reads.in_flight.remove(&self.store);
        }
    }
}

impl AppState {
    /// Starts bounded reads of retained runs off every lock. With a session,
    /// at the opening of its turn: its work's retained runs are read first.
    /// Without one, at host startup: each store with retained runs is read
    /// once a session can read it. No read starts while a reclamation pass
    /// is running. A reclamation or release that starts during a read is not
    /// held back: owner fencing makes one of them a superseded owner, which
    /// fails closed.
    fn schedule_engram_retained_run_reads(&self, session_id: Option<&str>) {
        #[cfg(test)]
        if !TEST_ENGRAM_RETAINED_RUN_READS_ENABLED.with(std::cell::Cell::get) {
            return;
        }
        // Under the lock only what to read and a reader snapshot; the readers
        // are chosen off it.
        let (wanted, snapshot) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if inner.engram_root_reclamation.flight.is_some() {
                return;
            }
            match session_id {
                Some(session_id) => {
                    let Some((store, work)) = engram_session_store_and_work_locked(&inner, session_id)
                    else {
                        return;
                    };
                    if !inner.engram_work_naming_history.iter().any(|history| {
                        history.store == store
                            && history.work_id == work
                            && engram_history_needs_recovery_locked(&inner, history)
                    }) {
                        return;
                    }
                    (vec![(store, Some(session_id.to_owned()), Some(work))], None)
                }
                None => {
                    // Only stores a project still names and that still owe
                    // their startup read are looked at: once every store had
                    // it, a tick costs one pass over the projects. A store no
                    // project names can have no reader.
                    let owed = inner
                        .projects
                        .iter()
                        .filter_map(|project| {
                            project.engram.as_ref().and_then(|settings| settings.authority_store_key.clone())
                        })
                        .filter(|store| !inner.engram_retained_run_reads.startup_read.contains(store))
                        .collect::<BTreeSet<_>>();
                    if owed.is_empty() {
                        return;
                    }
                    let stores = inner
                        .engram_work_naming_history
                        .iter()
                        .filter(|history| owed.contains(&history.store))
                        .filter(|history| engram_history_needs_recovery_locked(&inner, history))
                        .map(|history| history.store.clone())
                        .collect::<BTreeSet<_>>();
                    // A store with nothing to recover at startup is done; a
                    // run retained later is read when its turn opens.
                    for store in owed.difference(&stores) {
                        inner.engram_retained_run_reads.startup_read.insert(store.clone());
                    }
                    if stores.is_empty() {
                        return;
                    }
                    (
                        stores.into_iter().map(|store| (store, None, None)).collect(),
                        Some(EngramRootReaderSnapshot::capture(&inner)),
                    )
                }
            }
        };
        let readers = snapshot.as_ref().map(EngramRootReaderSnapshot::readers).unwrap_or_default();
        let chosen = wanted
            .into_iter()
            .filter_map(|(store, session, work)| {
                let reader = session.or_else(|| {
                    engram_root_recovery_readers(&readers, &store, None)
                        .first()
                        .map(|reader| (*reader).to_owned())
                })?;
                Some((store, reader, work))
            })
            .collect::<Vec<_>>();
        let reads = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if inner.engram_root_reclamation.flight.is_some() {
                return;
            }
            let mut reads = Vec::new();
            for (store, reader, work) in chosen {
                if inner.engram_retained_run_reads.in_flight.insert(store.clone()) {
                    if work.is_none() {
                        inner.engram_retained_run_reads.startup_read.insert(store.clone());
                    }
                    reads.push((store, reader, work));
                }
            }
            reads
        };
        for (store, reader, work) in reads {
            let state = self.clone();
            let flight = EngramRetainedRunReadFlight { state: self.clone(), store };
            let spawned = std::thread::Builder::new()
                .name("engram-retained-run-read".to_owned())
                .spawn(move || {
                    let _flight = flight;
                    // The recovery publishes and persists what it settles.
                    state.recover_engram_authority_runs_preferring(
                        &reader,
                        work.as_deref(),
                        Duration::from_secs(2),
                    );
                });
            match spawned {
                #[cfg(test)]
                Ok(handle) => {
                    TEST_ENGRAM_RETAINED_RUN_READS.with(|reads| reads.borrow_mut().push(handle));
                }
                #[cfg(not(test))]
                Ok(_) => {}
                // The unstarted closure drops its flight, which releases it.
                Err(error) => eprintln!("engram> retained-run read could not start: {error}"),
            }
        }
    }
}

/// Console lines a recovery batch collects under the state lock. They are
/// written when the batch ends, however it ends: the batch's state guard is
/// always released before this guard drops.
struct EngramRetainedRunConsole<'a> {
    state: &'a AppState,
    lines: Vec<String>,
}

impl Drop for EngramRetainedRunConsole<'_> {
    fn drop(&mut self) {
        for line in std::mem::take(&mut self.lines) {
            engram_retained_run_log(self.state, line);
        }
    }
}

/// Reports a retained-run settlement on the host console: a host-binding bug
/// when Engram placed the run in another project, or a store-integrity alarm.
/// Never under the global state lock: a slow or blocked console must not
/// extend the guard every session waits on.
fn engram_retained_run_log(state: &AppState, line: String) {
    #[cfg(test)]
    {
        assert!(state.inner.is_not_held_by_current_thread_for_test(),
            "retained-run console lines are written off the state lock");
        TEST_ENGRAM_RETAINED_RUN_LOG.with(|log| log.borrow_mut().push(line.clone()));
    }
    #[cfg(not(test))]
    let _ = state;
    eprintln!("{line}");
}

#[cfg(test)]
thread_local! {
    static TEST_ENGRAM_RETAINED_RUN_LOG: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// The background reads this test thread started, for the test to join.
    static TEST_ENGRAM_RETAINED_RUN_READS: std::cell::RefCell<Vec<std::thread::JoinHandle<()>>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Background reads start in a test only when it opts in, so no other
    /// test gains an unjoined reader thread.
    static TEST_ENGRAM_RETAINED_RUN_READS_ENABLED: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}
