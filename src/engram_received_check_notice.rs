// Read-only presentation of a host-held receipt. Neither a file beside a run
// nor an ordinary check can supply this authority. Artifact reads are off-lock.
// EngramOtherRootNoticeRead moved here from engram_turn_checks.rs, extended
// with receipt presentation; its existing owned-observer lifecycle is reused.
// Presentation confirms receipt of the registered run's historical record.

use super::*;
pub(crate) const RECEIVED_NOTICE_CAPTURE_BUDGET: Duration = ENGRAM_SOURCE_COMMAND_BUDGET;
/// A settled verification record's own host line. It is never merged into
/// the check-credit line or cut with it, and never counts toward the
/// pending-line cap, so the next prompt carries every undelivered settlement.
pub(crate) const ENGRAM_SETTLED_RECORD_LINE_PREFIX: &str = "[TermAl] Verification record settled:";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReceivedNoticeSourceUnavailable {
    Deadline,
    Read,
}

pub(crate) fn received_notice_step_timeout(
    deadline: std::time::Instant,
    maximum: Duration,
) -> Duration {
    maximum.min(deadline.saturating_duration_since(std::time::Instant::now()))
}

#[cfg(test)]
pub(crate) static TEST_RECEIVED_NOTICE_SOURCE_READ: std::sync::LazyLock<
    std::sync::Mutex<HashMap<String, Box<dyn FnOnce(std::time::Instant) + Send>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

#[derive(Clone)]
pub(crate) struct ReceivedCheckNotice {
    pub(crate) registration: TestRunRegistration,
    pub(crate) receipt: EngramVerificationRecordReceipt,
    pub(crate) deadline: std::time::Instant,
}

impl ReceivedCheckNotice {
    pub(crate) fn read(
        registration: &TestRunRegistration,
        deadline: std::time::Instant,
    ) -> Option<Self> {
        let ack = registration.verification_record.as_ref()?;
        let (receipt, digest) = match ack {
            TestRunVerificationAck::Settled {
                receipt,
                body_digest,
            } => (receipt, body_digest),
            // Receipt custody precedes the association fence. A held receipt
            // cannot bypass settlement through a closing or companion read.
            TestRunVerificationAck::BindingRetry { .. }
            | TestRunVerificationAck::Received { .. }
            | TestRunVerificationAck::Rejected { .. }
            | TestRunVerificationAck::LocalUnclassified { .. } => return None,
        };
        let directory = FsPath::new(&registration.run_directory);
        let frozen = engram_verification_record_frozen(directory, &registration.run_id)?;
        if !engram_verification_record_digest_answers(digest, &frozen.body)
            || !engram_verification_record_is_host_body(registration, &frozen.body)
            || engram_verification_record_receipt_check(&frozen.body, receipt).is_err()
        {
            return None;
        }
        Self::read_run(registration, &frozen.body, receipt, deadline)
    }

    /// At settlement, the body and receipt the settlement itself validated
    /// stand in for the frozen file and the Settled ack, and the run's files
    /// are not read again: a read that fails now would leave the settled
    /// record untold. The body is already the host's: the producer froze it
    /// from its own rebuild or compared it equal to that rebuild before the
    /// receipt was held, and completion refuses any other body
    /// (`AcknowledgedBodyChanged`) before this runs. Every settled record is
    /// told, whatever its input: a native run's unchanged dirty source too.
    /// The line names the run and its records and confirms no credit, so it
    /// needs no clean commit (`read_run`, which feeds credit, keeps its gate).
    /// Called off the state lock, before the ack is written.
    pub(crate) fn at_settlement(
        registration: &TestRunRegistration,
        body: &EngramVerificationRecordBody,
        receipt: &EngramVerificationRecordReceipt,
        deadline: std::time::Instant,
    ) -> Option<Self> {
        if engram_verification_record_receipt_check(body, receipt).is_err() {
            return None;
        }
        Some(Self::validated(registration, receipt, deadline))
    }

    fn read_run(
        registration: &TestRunRegistration,
        body: &EngramVerificationRecordBody,
        receipt: &EngramVerificationRecordReceipt,
        deadline: std::time::Instant,
    ) -> Option<Self> {
        let directory = FsPath::new(&registration.run_directory);
        let disk = TestRunDisk::read(directory, None)?;
        if !test_run_registered_disk_matches(registration, &disk)
            || !registered_settled_clean_input(registration, &disk)
        {
            return None;
        }
        body.input.clean_commit()?;
        Some(Self::validated(registration, receipt, deadline))
    }

    fn validated(
        registration: &TestRunRegistration,
        receipt: &EngramVerificationRecordReceipt,
        deadline: std::time::Instant,
    ) -> Self {
        #[cfg(test)]
        {
            let hook = TEST_RECEIVED_NOTICE_SOURCE_READ
                .lock()
                .unwrap()
                .remove(&registration.run_id);
            if let Some(hook) = hook {
                hook(deadline);
            }
        }
        Self {
            registration: registration.clone(),
            receipt: receipt.clone(),
            deadline,
        }
    }

    pub(super) fn matches_root(
        &self,
        root: &EngramWorkSourceRoot,
        binding: &EngramControlWorkBinding,
    ) -> bool {
        Self::registration_matches_root(&self.registration, root, binding)
    }

    // The run's claim, store, worktree and named-root generation tie it to
    // the root; who named the root does not. A successor that inherited the
    // root keeps its predecessor as the namer, and the notice still goes
    // only to the run's own owner.
    fn registration_matches_root(
        run: &TestRunRegistration,
        root: &EngramWorkSourceRoot,
        binding: &EngramControlWorkBinding,
    ) -> bool {
        run.authority_store.as_ref() == Some(&root.store)
            && run.work_binding.as_ref() == Some(binding)
            && binding.work_id == root.work_id
            && binding.claim_id == root.claim_id
            && binding.claim_fence == root.claim_fence
            && test_run_path_key(&run.worktree) == test_run_path_key(&root.root)
            && run.named_root.as_ref().is_some_and(|named| {
                named.workspace_id == root.root && named.generation == root.generation
            })
    }

    pub(crate) fn is_current(&self, inner: &StateInner) -> bool {
        inner
            .test_run_registrations
            .get(&self.registration.run_id)
            .is_some_and(|actual| actual.matches_registration_snapshot(&self.registration))
    }

    /// The part of `line` that names the run and its records, whatever the
    /// reason suffix says; two lines for the same settlement share it.
    fn recorded(&self) -> String {
        format!(
            "{ENGRAM_SETTLED_RECORD_LINE_PREFIX}{}",
            settled_receipt_recorded(&self.registration.run_id, &self.receipt)
        )
    }

    pub(crate) fn line(&self) -> String {
        let recorded = self.recorded();
        let reason = if std::time::Instant::now() >= self.deadline {
            "notice capture deadline elapsed"
        } else {
            "no source observation available for this notice"
        };
        format!(
            "{recorded} Current input confirmation is unavailable ({reason}); \
            this notice confirms no current-input or ordinary-check credit."
        )
    }
}

/// `ReceivedCheckNotice::recorded` without the credit-line prefix: the text a
/// pending or delivered line holds once it tells this settlement.
fn settled_receipt_recorded(run_id: &str, receipt: &EngramVerificationRecordReceipt) -> String {
    let outcome = match receipt.run_outcome {
        EngramVerificationRecordRunOutcome::Passed => "passed",
        EngramVerificationRecordRunOutcome::Failed => "failed",
        EngramVerificationRecordRunOutcome::Indeterminate => "indeterminate",
    };
    let records = receipt
        .verifications
        .iter()
        .take(8)
        .map(|record| record.record_id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let more = receipt.verifications.len().saturating_sub(8);
    let records = if more == 0 {
        records
    } else {
        format!("{records} (+{more} more in the run record)")
    };
    format!(
        " registered launcher run {run_id} {outcome}; \
        Engram received verification record(s) {records} for that run."
    )
}

/// A settlement whose notice no accepted prompt has carried yet, with the
/// text that tells it.
fn undelivered_settlement(run: &TestRunRegistration) -> Option<String> {
    match run.verification_record.as_ref() {
        Some(TestRunVerificationAck::Settled { receipt, .. })
            if run.settlement_notice_undelivered =>
        {
            Some(settled_receipt_recorded(&run.run_id, receipt))
        }
        _ => None,
    }
}

pub(super) fn registered_launcher_notice(
    run: &TestRunRegistration,
    state: &TestRunState,
) -> String {
    let status = match state {
        TestRunState::Passed => "passed; consult its terminal result record",
        TestRunState::Failed => "failed; consult its terminal result record",
        _ => "still running; its outcome is pending",
    };
    format!(
        "{ENGRAM_CHECK_CREDIT_LINE_PREFIX} registered launcher run {} is {status}. \
        It is correlated with this command's explicit launcher locator. Native verification \
        uses its own run record; do not start a duplicate run.",
        run.run_id
    )
}

impl AppState {
    // Terminal paths publish their original native correlation first. The
    // existing cancellable observer reads settled receipts off the provider
    // reader. One absolute budget covers the whole batch.
    pub(super) fn schedule_received_launcher_notices(
        &self,
        mut prepared: Vec<RegisteredCheckNotice>,
    ) {
        prepared.retain(|notice| {
            notice.runs.iter().any(|(run, _)| {
                matches!(
                    run.verification_record,
                    Some(TestRunVerificationAck::Settled { .. })
                )
            })
        });
        if prepared.is_empty() {
            return;
        }
        let owner = self.clone();
        if let Err(error) =
            self.capture_lifetime
                .spawn_observer("termal-received-launcher-notice", move || {
                    let deadline = std::time::Instant::now() + RECEIVED_NOTICE_CAPTURE_BUDGET;
                    read_received_registered_check_notices_until(&mut prepared, deadline);
                    for notice in prepared {
                        if let Some(received) = notice.received {
                            if check_capture_cancellation().is_err() {
                                return;
                            }
                            let mut inner = owner.inner.lock().expect("state mutex poisoned");
                            // The settlement step already presented this
                            // receipt; the close only upgrades its own
                            // fragment and never adds the line again.
                            Self::publish_received_launcher_notice_locked(
                                &mut inner, &received, false,
                            );
                        }
                    }
                })
        {
            eprintln!("engram> optional received notice worker failed: {error}");
        }
    }

    // A receipt settles in the same state step that presents it
    // (`note_engram_verification_record_ack_presenting`), so no turn can open
    // between the two. The line is its own line for the owning session's next
    // prompt, also while the turn that launched the run is still open: that
    // turn's close composes only the plain fragment beside it.
    pub(crate) fn present_settled_receipt_locked(
        inner: &mut StateInner,
        received: &mut ReceivedCheckNotice,
    ) {
        let registration = &received.registration;
        let Some(current) = inner
            .test_run_registrations
            .get(&registration.run_id)
            .filter(|current| {
                current.request_identity == registration.request_identity
                    && current.run_directory == registration.run_directory
                    && matches!(
                        current.verification_record,
                        Some(TestRunVerificationAck::Settled { .. })
                    )
            })
            .cloned()
        else {
            return;
        };
        received.registration = current;
        Self::publish_received_launcher_notice_locked(inner, received, true);
        // Pending lines live in memory only, so the Settled ack keeps, in
        // the same persist, that this notice is still to be delivered.
        let recorded = settled_receipt_recorded(&received.registration.run_id, &received.receipt);
        let pending = inner
            .find_session_index(&received.registration.owning_session_id)
            .and_then(|index| {
                inner.sessions[index]
                    .engram
                    .pending_source_root_line
                    .as_deref()
            })
            .is_some_and(|pending| pending.contains(&recorded));
        if pending {
            inner
                .test_run_registrations
                .update_unindexed_fields(&received.registration.run_id, |row| {
                    row.settlement_notice_undelivered = true
                });
        }
    }

    /// Clears the undelivered mark of each of `session_id`'s settlements that
    /// the prompt the runtime just accepted carried (`delivered`). Returns
    /// whether any was cleared, for the caller to persist.
    pub(crate) fn note_settlement_notices_delivered_locked(
        inner: &mut StateInner,
        session_id: &str,
        delivered: &str,
    ) -> bool {
        let told = inner
            .test_run_registrations
            .values()
            .filter(|run| run.owning_session_id == session_id)
            .filter(|run| undelivered_settlement(run).is_some_and(|text| delivered.contains(&text)))
            .map(|run| run.run_id.clone())
            .collect::<Vec<_>>();
        for run_id in &told {
            inner
                .test_run_registrations
                .update_unindexed_fields(run_id, |row| row.settlement_notice_undelivered = false);
        }
        if !told.is_empty() {
            // The mark held these runs out of history compaction; a pass
            // that already looked selects them again.
            inner.test_runs.compaction_examined_revision = None;
        }
        // Safety net: a settlement still undelivered whose line has left the
        // pending lines without an accepted prompt is put back.
        Self::present_undelivered_settlements_locked(inner, Some(session_id));
        !told.is_empty()
    }

    /// At load, presents again each settlement no accepted prompt carried
    /// before the host stopped, to its owning session's next prompt.
    pub(crate) fn present_undelivered_settlements_at_load(inner: &mut StateInner) {
        Self::present_undelivered_settlements_locked(inner, None);
    }

    /// Puts the line of each settlement no accepted prompt has carried, of
    /// `session` or of every session, before its owner's next prompt unless
    /// a pending line already tells it. The Settled ack was checked when it
    /// settled; nothing is read again.
    pub(crate) fn present_undelivered_settlements_locked(
        inner: &mut StateInner,
        session: Option<&str>,
    ) {
        let lines = inner
            .test_run_registrations
            .values()
            .filter(|run| session.is_none_or(|session| run.owning_session_id == session))
            .filter_map(|run| {
                let Some(TestRunVerificationAck::Settled { receipt, .. }) =
                    run.verification_record.as_ref()
                else {
                    return None;
                };
                let text = undelivered_settlement(run)?;
                let notice = ReceivedCheckNotice {
                    registration: run.clone(),
                    receipt: receipt.clone(),
                    deadline: std::time::Instant::now() + RECEIVED_NOTICE_CAPTURE_BUDGET,
                };
                Some((run.owning_session_id.clone(), text, notice.line()))
            })
            .collect::<Vec<_>>();
        for (session_id, text, line) in lines {
            if let Some(index) = inner.find_session_index(&session_id) {
                let engram = &mut inner.sessions[index].engram;
                if !engram
                    .pending_source_root_line
                    .as_deref()
                    .is_some_and(|pending| pending.contains(&text))
                {
                    engram.set_pending_source_root_line(line);
                }
            }
        }
    }

    // Text controls delivery only; host registration, artifact and live root
    // identity below remain the authority. Never match by command/path/time.
    // A pending line that already names this settlement, whatever its reason
    // suffix, is left as it is. Only the settlement step (`append`) adds the
    // line; the turn-close worker's late read adds nothing.
    pub(crate) fn publish_received_launcher_notice_locked(
        inner: &mut StateInner,
        received: &ReceivedCheckNotice,
        append: bool,
    ) {
        let current = &received.registration;
        let Some(index) = inner.find_session_index(&current.owning_session_id) else {
            return;
        };
        // The current registration and its validated Settled receipt are the
        // authority, as for the load and reset re-presentations: the line goes
        // only to the run's own owning session, whatever claim, root or
        // Engram setting that session has now, and confirms no credit. A run
        // with no launch observation row (a lint-only run) is told as well.
        if !received.is_current(inner) {
            return;
        }
        let engram = &mut inner.sessions[index].engram;
        let recorded = settled_receipt_recorded(&current.run_id, &received.receipt);
        if !append
            || engram
                .pending_source_root_line
                .as_deref()
                .is_some_and(|pending| pending.contains(&recorded))
        {
            return;
        }
        // Its own line, never merged into the check-credit line nor counted
        // toward the pending-line cap: the next prompt carries every
        // settlement still undelivered, beside the run's plain fragment.
        engram.set_pending_source_root_line(received.line());
    }
}

// Other claims are read only to explain command placement. This snapshot
// owns no turn admission or evidence. Its read runs in an owned observer,
// never on the runtime frame path.
pub(crate) struct EngramOtherRootNoticeRead {
    pub(crate) session_id: String,
    pub(crate) target: EngramBindingTarget,
    pub(crate) roots: Vec<EngramWorkSourceRoot>,
    pub(crate) host_turn_ref: String,
    pub(crate) turn_generation: u64,
    pub(crate) runtime: Option<RuntimeToken>,
    pub(crate) workdir: String,
    pub(crate) source_root: Option<EngramTurnSourceRoot>,
    bound_root: Option<EngramWorkSourceRoot>,
    received_candidates: Vec<TestRunRegistration>,
}

impl EngramOtherRootNoticeRead {
    pub(crate) fn capture(inner: &StateInner, index: usize) -> Option<Self> {
        let record = &inner.sessions[index];
        let session_id = &record.session.id;
        if AppState::engram_session_has_child_binding_shape_locked(inner, session_id) {
            return None;
        }
        let host_turn_ref = runtime_observation_check_owner(record)?.to_owned();
        let target = AppState::engram_source_root_target_locked(inner, session_id).ok()??;
        let store = target.settings.authority_store_key.as_ref()?;
        let binding = target.work_binding.as_ref()?;
        let roots: Vec<_> = inner
            .engram_work_source_roots
            .iter()
            .filter(|entry| {
                entry.store == *store
                    && entry.named_by_session == *session_id
                    && (entry.work_id != binding.work_id || entry.claim_id != binding.claim_id)
            })
            .cloned()
            .collect();
        if roots.is_empty() {
            return None;
        }
        let bound_root = inner
            .engram_work_source_roots
            .iter()
            .find(|entry| {
                entry.store == *store
                    && entry.named_by_session == *session_id
                    && entry.work_id == binding.work_id
                    && entry.claim_id == binding.claim_id
            })
            .cloned();
        // Bounded in-memory selection of this naming session's settled
        // receipts. The body and canonical live binding are checked off-lock
        // below. A historical receipt does not change placement guidance.
        let mut received_candidates = Vec::new();
        for root in roots.iter().chain(bound_root.iter()) {
            let mut runs = inner
                .test_run_registrations
                .values()
                .filter(|run| {
                    run.owning_session_id == *session_id
                        && run.authority_store.as_ref() == Some(store)
                        && matches!(
                            run.verification_record,
                            Some(TestRunVerificationAck::Settled { .. })
                        )
                        && run.named_root.as_ref().is_some_and(|named| {
                            named.workspace_id == root.root && named.generation == root.generation
                        })
                        && run.work_binding.as_ref().is_some_and(|binding| {
                            binding.work_id == root.work_id && binding.claim_id == root.claim_id
                        })
                })
                .collect::<Vec<_>>();
            runs.sort_by(|a, b| b.registered_at.cmp(&a.registered_at));
            received_candidates.extend(runs.into_iter().take(8).cloned());
        }
        Some(Self {
            session_id: session_id.clone(),
            target,
            roots,
            host_turn_ref,
            turn_generation: record.active_turn_generation,
            runtime: record.runtime.runtime_token(),
            workdir: record.session.workdir.clone(),
            source_root: record.engram.observed_source_root().cloned(),
            bound_root,
            received_candidates,
        })
    }

    pub(crate) fn read(
        &self,
        deadline: std::time::Instant,
        shared: bool,
    ) -> Result<
        (
            Vec<EngramWorkSourceRoot>,
            Vec<(EngramWorkSourceRoot, ReceivedCheckNotice)>,
        ),
        ReceivedNoticeSourceUnavailable,
    > {
        let timeout = received_notice_step_timeout(
            deadline,
            self.target
                .settings
                .call_timeout()
                .min(ENGRAM_WORK_BINDING_COMMAND_TIMEOUT),
        );
        if timeout.is_zero() {
            return Err(ReceivedNoticeSourceUnavailable::Deadline);
        }
        let held = self
            .target
            .adapter
            .read_held_claims(&self.target.connection, timeout);
        let held = held.map_err(|_| {
            if std::time::Instant::now() >= deadline {
                ReceivedNoticeSourceUnavailable::Deadline
            } else {
                ReceivedNoticeSourceUnavailable::Read
            }
        })?;
        if std::time::Instant::now() >= deadline {
            return Err(ReceivedNoticeSourceUnavailable::Deadline);
        }
        let roots = self
            .roots
            .iter()
            .filter(|entry| {
                held.items
                    .iter()
                    .any(|claim| claim.work_id == entry.work_id && claim.claim_id == entry.claim_id)
            })
            .cloned()
            .collect();
        let mut received = Vec::new();
        for root in self
            .roots
            .iter()
            .filter(|_| shared)
            .chain(self.bound_root.iter().filter(|_| !shared))
        {
            let Some(binding) = held
                .items
                .iter()
                .find(|claim| {
                    claim.work_id == root.work_id
                        && claim.claim_id == root.claim_id
                        && claim.claim_fence == root.claim_fence
                })
                .and_then(|claim| claim.control_binding.as_ref())
            else {
                continue;
            };
            for run in &self.received_candidates {
                if !ReceivedCheckNotice::registration_matches_root(run, root, binding) {
                    continue;
                }
                if let Some(notice) = ReceivedCheckNotice::read(run, deadline)
                    .filter(|notice| notice.matches_root(root, binding))
                {
                    received.push((root.clone(), notice));
                    break;
                }
            }
        }
        Ok((roots, received))
    }

    pub(crate) fn is_current(&self, inner: &StateInner, index: usize) -> bool {
        let record = &inner.sessions[index];
        record.active_turn_generation == self.turn_generation
            && record.runtime.runtime_token() == self.runtime
            && record.session.workdir == self.workdir
            && record.engram.observed_source_root() == self.source_root.as_ref()
            && runtime_observation_check_owner(record) == Some(self.host_turn_ref.as_str())
            && AppState::engram_source_root_target_locked(inner, &record.session.id)
                .ok()
                .flatten()
                .is_some_and(|current| {
                    current.connection == self.target.connection
                        && current.project_id == self.target.project_id
                        && current.work_binding == self.target.work_binding
                        && current
                            .settings
                            .same_work_service_settings(&self.target.settings)
                })
    }

    pub(crate) fn schedule(
        self,
        state: &AppState,
        bound: String,
        shared: bool,
        feedback_revision: u64,
    ) {
        if self.roots.is_empty() {
            return;
        }
        let owner = state.clone();
        if let Err(error) = state.capture_lifetime.spawn_observer("termal-other-root-notice", move || {
            let deadline = std::time::Instant::now() + RECEIVED_NOTICE_CAPTURE_BUDGET;
            let (roots, received, authority_unavailable) = match self.read(deadline, shared) {
                Ok((roots, received)) => (roots, received, None),
                Err(_) if self.received_candidates.is_empty() => return,
                Err(reason) => (Vec::new(), Vec::new(), Some(reason)),
            };
            if roots.is_empty() && authority_unavailable.is_none() { return; }
            let mut inner = owner.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(&self.session_id) else { return; };
            if !self.is_current(&inner, index)
                || inner.sessions[index].engram.uncredited_feedback_revision != feedback_revision
                || !roots.iter().all(|entry| inner.engram_work_source_roots.contains(entry)) { return; }
            let received = received.iter().filter(|(root, notice)|
                inner.engram_work_source_roots.contains(root) && notice.is_current(&inner))
                .collect::<Vec<_>>();
            let mut details = if let Some(reason) = authority_unavailable {
                let reason = match reason {
                    ReceivedNoticeSourceUnavailable::Deadline => "notice capture deadline elapsed",
                    ReceivedNoticeSourceUnavailable::Read => "held-claim read unavailable",
                };
                vec![format!("[TermAl] Named-root claim confirmation is unavailable ({reason}); this notice confirms no current-input or ordinary-check credit. No rerun is requested.")]
            } else { Vec::new() };
            details.extend(received.into_iter().map(|(_, notice)| notice.line()));
            if roots.is_empty() && details.is_empty() { return; }
            let others = roots.iter().map(|entry| entry.short_ref.as_str())
                .collect::<Vec<_>>().join(", ");
            let line = if roots.is_empty() {
                details.join("\n")
            } else if shared {
                format!("[TermAl] A recognised test in a shared named root is reported for \
                    bound item {bound} only; {others} receives no credit. Make that item \
                    the focus and run its test in the next turn for its own credit.")
            } else {
                format!("[TermAl] A recognised test ran in the named root of {others}, \
                    outside bound item {bound}'s root, and gets no credit for {bound}. \
                    Make that item the focus and run its test in the next turn.")
            };
            let line = if !roots.is_empty() && !details.is_empty() {
                format!("{line}\n{}", details.join("\n"))
            } else { line };
            if self.is_current(&inner, index)
                && inner.sessions[index].engram.uncredited_feedback_revision == feedback_revision
                && roots.iter().all(|entry| inner.engram_work_source_roots.contains(entry))
            {
                inner.sessions[index].engram.set_pending_source_root_line(line);
            }
        }) {
            eprintln!("engram> optional other-root notice worker failed: {error}");
        }
    }
}
