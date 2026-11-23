// Read-only presentation of a host-held receipt. Neither a file beside a run
// nor an ordinary check can supply this authority. Artifact reads are off-lock.
// EngramOtherRootNoticeRead moved here from engram_turn_checks.rs, extended
// with receipt presentation; its existing owned-observer lifecycle is reused.
// Presentation confirms receipt of the registered run's historical record.
pub(crate) const RECEIVED_NOTICE_CAPTURE_BUDGET: Duration = ENGRAM_SOURCE_COMMAND_BUDGET;

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
            TestRunVerificationAck::Received { .. }
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
        let disk = TestRunDisk::read(directory, None)?;
        if !test_run_registered_disk_matches(registration, &disk)
            || !registered_closing_clean_input(registration, &disk)
        {
            return None;
        }
        frozen.body.input.clean_commit()?;
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
        Some(Self {
            registration: registration.clone(),
            receipt: receipt.clone(),
            deadline,
        })
    }

    fn matches_root(
        &self,
        root: &EngramWorkSourceRoot,
        binding: &EngramControlWorkBinding,
    ) -> bool {
        Self::registration_matches_root(&self.registration, root, binding)
    }

    fn registration_matches_root(
        run: &TestRunRegistration,
        root: &EngramWorkSourceRoot,
        binding: &EngramControlWorkBinding,
    ) -> bool {
        run.owning_session_id == root.named_by_session
            && run.authority_store.as_ref() == Some(&root.store)
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

    pub(crate) fn line(&self) -> String {
        let outcome = match self.receipt.run_outcome {
            EngramVerificationRecordRunOutcome::Passed => "passed",
            EngramVerificationRecordRunOutcome::Failed => "failed",
            EngramVerificationRecordRunOutcome::Indeterminate => "indeterminate",
        };
        let records = self
            .receipt
            .verifications
            .iter()
            .take(8)
            .map(|record| record.record_id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let more = self.receipt.verifications.len().saturating_sub(8);
        let records = if more == 0 {
            records
        } else {
            format!("{records} (+{more} more in the run record)")
        };
        let recorded = format!(
            "{ENGRAM_CHECK_CREDIT_LINE_PREFIX} registered launcher run {} {outcome}; \
            Engram received verification record(s) {records} for that run.",
            self.registration.run_id
        );
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

fn registered_launcher_notice(run: &TestRunRegistration, state: &TestRunState) -> String {
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
    pub(crate) fn schedule_received_launcher_notice(
        &self,
        registration: &TestRunRegistration,
        deadline: std::time::Instant,
    ) {
        let Some(current) = self.pending_received_launcher_registration(registration) else {
            return;
        };
        let owner = self.clone();
        if let Err(error) =
            self.capture_lifetime
                .spawn_observer("termal-received-launcher-notice", move || {
                    owner.refresh_received_launcher_notice(&current, deadline);
                })
        {
            eprintln!("engram> optional received notice worker failed: {error}");
        }
    }

    // Terminal paths publish their original native correlation first. The
    // existing cancellable observer reads settled receipts off the provider
    // reader. One absolute budget covers the whole batch.
    fn schedule_received_launcher_notices(&self, mut prepared: Vec<RegisteredCheckNotice>) {
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
                            owner.publish_received_launcher_notice(received);
                        }
                    }
                })
        {
            eprintln!("engram> optional received notice worker failed: {error}");
        }
    }

    // A receipt can arrive after turn close but before its pending advice is
    // delivered. Replace only the exact registered-run fragment emitted by
    // the original correlation owner, preserving every other check's warning.
    // Text controls delivery only; host registration, artifact and live root
    // identity below remain the authority. Never match by command/path/time.
    fn pending_received_launcher_registration(
        &self,
        registration: &TestRunRegistration,
    ) -> Option<TestRunRegistration> {
        let current = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(&registration.owning_session_id)?;
            let pending = inner.sessions[index]
                .engram
                .pending_source_root_line
                .as_deref()
                .unwrap_or_default();
            if ![
                TestRunState::Running,
                TestRunState::Passed,
                TestRunState::Failed,
            ]
            .iter()
            .any(|state| {
                let line = registered_launcher_notice(registration, state);
                pending.contains(
                    line.strip_prefix(ENGRAM_CHECK_CREDIT_LINE_PREFIX)
                        .unwrap_or(&line),
                )
            }) {
                return None;
            }
            inner
                .test_run_registrations
                .get(&registration.run_id)
                .cloned()
        };
        current.filter(|current| {
            current.request_identity == registration.request_identity
                && current.run_directory == registration.run_directory
                && matches!(
                    current.verification_record,
                    Some(TestRunVerificationAck::Settled { .. })
                )
        })
    }

    fn refresh_received_launcher_notice(
        &self,
        registration: &TestRunRegistration,
        deadline: std::time::Instant,
    ) {
        let Some(current) = self.pending_received_launcher_registration(registration) else {
            return;
        };
        let Some(received) = ReceivedCheckNotice::read(&current, deadline) else {
            return;
        };
        self.publish_received_launcher_notice(received);
    }

    fn publish_received_launcher_notice(&self, received: ReceivedCheckNotice) {
        if check_capture_cancellation().is_err() {
            return;
        }
        let current = &received.registration;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(&current.owning_session_id) else {
            return;
        };
        if !received.is_current(&inner) {
            return;
        }
        let Some(target) =
            Self::engram_source_root_target_locked(&inner, &current.owning_session_id)
                .ok()
                .flatten()
        else {
            return;
        };
        let Some(binding) = target.work_binding.as_ref() else {
            return;
        };
        if target.settings.authority_store_key.as_ref() != current.authority_store.as_ref()
            || !inner
                .engram_work_source_roots
                .iter()
                .any(|root| received.matches_root(root, binding))
            || !inner.test_run_observations.values().any(|row| {
                engram_verification_record_row_matches(row, current)
                    && row.work_binding.as_ref() == Some(binding)
            })
        {
            return;
        }
        let engram = &mut inner.sessions[index].engram;
        let Some(pending) = engram.pending_source_root_line.as_ref() else {
            return;
        };
        let replacement = received.line();
        let replacement = replacement
            .strip_prefix(ENGRAM_CHECK_CREDIT_LINE_PREFIX)
            .unwrap_or(&replacement);
        let mut lines = pending.lines().map(str::to_owned).collect::<Vec<_>>();
        let mut changed = false;
        for line in &mut lines {
            if !line.starts_with(ENGRAM_CHECK_CREDIT_LINE_PREFIX) {
                continue;
            }
            for state in [
                TestRunState::Running,
                TestRunState::Passed,
                TestRunState::Failed,
            ] {
                let old = registered_launcher_notice(current, &state);
                let old = old
                    .strip_prefix(ENGRAM_CHECK_CREDIT_LINE_PREFIX)
                    .unwrap_or(&old);
                if line.contains(old) {
                    let replaced = line.replace(old, replacement);
                    if replaced.len() <= ENGRAM_CHECK_CREDIT_LINE_MAX_BYTES {
                        *line = replaced;
                        changed = true;
                    }
                }
            }
        }
        if changed {
            engram.pending_source_root_line = Some(lines.join("\n"));
            // Delivery removes only the exact text already composed. This
            // receipt rewrite is not newer command feedback and must not
            // cancel an in-flight placement notice for another command.
        }
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
