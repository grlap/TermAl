// Read-only presentation of a host-held receipt. Neither a file beside a run
// nor an ordinary check can supply this authority. All disk/Git work is off-lock.
// EngramOtherRootNoticeRead moved here from engram_turn_checks.rs, extended
// with receipt presentation; its existing owned-observer lifecycle is reused.
// Source observations share the owner's 30 s bound. This does not extend
// registration durability or allow a partial observation after expiry.
const RECEIVED_NOTICE_CAPTURE_BUDGET: Duration = ENGRAM_SOURCE_COMMAND_BUDGET;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceivedNoticeSource {
    Current,
    SourceUnavailable(ReceivedNoticeSourceUnavailable),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceivedNoticeSourceUnavailable {
    Deadline,
    Read,
}

fn received_notice_step_timeout(deadline: std::time::Instant, maximum: Duration) -> Duration {
    maximum.min(deadline.saturating_duration_since(std::time::Instant::now()))
}

#[cfg(test)]
static TEST_RECEIVED_NOTICE_SOURCE_READ: std::sync::LazyLock<std::sync::Mutex<
    HashMap<String, Box<dyn FnOnce(std::time::Instant) + Send>>,
>> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

// Protocol fixtures may supply only the off-lock source result. They still
// pass every registration, receipt, disk, claim and publication fence. Key by
// unique run rather than thread: companion notices use owned worker threads.
#[cfg(test)]
#[derive(Clone)]
enum TestReceivedNoticeSourceResult {
    Status(String, Vec<SourceRevisionDirtyPath>),
    Unavailable,
}

#[cfg(test)]
static TEST_RECEIVED_NOTICE_SOURCE_RESULTS: std::sync::LazyLock<std::sync::Mutex<
    HashMap<String, TestReceivedNoticeSourceResult>,
>> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

#[derive(Clone)]
struct ReceivedCheckNotice {
    registration: TestRunRegistration,
    receipt: EngramVerificationRecordReceipt,
    source: ReceivedNoticeSource,
    deadline: std::time::Instant,
}

impl ReceivedCheckNotice {
    fn read(registration: &TestRunRegistration, deadline: std::time::Instant) -> Option<Self> {
        let ack = registration.verification_record.as_ref()?;
        let (receipt, digest) = match ack {
            TestRunVerificationAck::Settled { receipt, body_digest } => (receipt, body_digest),
            // Receipt custody precedes the association fence. A held receipt
            // cannot bypass settlement through a closing or companion read.
            TestRunVerificationAck::Received { .. } | TestRunVerificationAck::Rejected { .. } => return None,
        };
        let directory = FsPath::new(&registration.run_directory);
        let frozen = engram_verification_record_frozen(directory, &registration.run_id)?;
        if !engram_verification_record_digest_answers(digest, &frozen.body)
            || !engram_verification_record_is_host_body(registration, &frozen.body)
            || engram_verification_record_receipt_check(&frozen.body, receipt).is_err() { return None; }
        let disk = TestRunDisk::read(directory, None)?;
        if !test_run_registered_disk_matches(registration, &disk)
            || !registered_closing_clean_input(registration, &disk) { return None; }
        let commit = frozen.body.input.clean_commit()?;
        #[cfg(test)]
        {
            let hook = TEST_RECEIVED_NOTICE_SOURCE_READ.lock().unwrap().remove(&registration.run_id);
            if let Some(hook) = hook { hook(deadline); }
        }
        let source = if std::time::Instant::now() >= deadline {
            ReceivedNoticeSource::SourceUnavailable(ReceivedNoticeSourceUnavailable::Deadline)
        } else {
            let read_source = || ReviewFreezeGit::new_with_deadline(FsPath::new(&registration.worktree), Some(deadline))
                .and_then(|git| committed_source_status(&git));
            #[cfg(not(test))]
            let status = read_source();
            #[cfg(test)]
            let status = {
                let scripted = TEST_RECEIVED_NOTICE_SOURCE_RESULTS.lock().unwrap()
                    .get(&registration.run_id).cloned();
                match scripted {
                    Some(TestReceivedNoticeSourceResult::Status(head, dirty)) => Ok((head, dirty)),
                    Some(TestReceivedNoticeSourceResult::Unavailable) => Err(anyhow::anyhow!("fixture source read unavailable")),
                    None => read_source(),
                }
            };
            match status {
                Ok((head, dirty)) => {
                    if head != commit || !dirty.is_empty() { return None; }
                    if std::time::Instant::now() >= deadline {
                        ReceivedNoticeSource::SourceUnavailable(ReceivedNoticeSourceUnavailable::Deadline)
                    } else { ReceivedNoticeSource::Current }
                }
                Err(_) => ReceivedNoticeSource::SourceUnavailable(if std::time::Instant::now() >= deadline {
                    ReceivedNoticeSourceUnavailable::Deadline
                } else { ReceivedNoticeSourceUnavailable::Read }),
            }
        };
        Some(Self { registration: registration.clone(), receipt: receipt.clone(), source, deadline })
    }

    fn source(&self) -> ReceivedNoticeSource {
        if std::time::Instant::now() >= self.deadline {
            ReceivedNoticeSource::SourceUnavailable(ReceivedNoticeSourceUnavailable::Deadline)
        } else { self.source }
    }

    fn matches_root(&self, root: &EngramWorkSourceRoot, binding: &EngramControlWorkBinding) -> bool {
        Self::registration_matches_root(&self.registration, root, binding)
    }

    fn registration_matches_root(run: &TestRunRegistration, root: &EngramWorkSourceRoot, binding: &EngramControlWorkBinding) -> bool {
        run.owning_session_id == root.named_by_session
            && run.authority_store.as_ref() == Some(&root.store)
            && run.work_binding.as_ref() == Some(binding)
            && binding.work_id == root.work_id && binding.claim_id == root.claim_id
            && binding.claim_fence == root.claim_fence
            && test_run_path_key(&run.worktree) == test_run_path_key(&root.root)
            && run.named_root.as_ref().is_some_and(|named|
                named.workspace_id == root.root && named.generation == root.generation)
    }

    fn is_current(&self, inner: &StateInner) -> bool {
        inner.test_run_registrations.get(&self.registration.run_id)
            .is_some_and(|actual| actual.matches_registration_snapshot(&self.registration))
    }

    fn line(&self) -> String {
        let outcome = match self.receipt.run_outcome {
            EngramVerificationRecordRunOutcome::Passed => "passed",
            EngramVerificationRecordRunOutcome::Failed => "failed",
            EngramVerificationRecordRunOutcome::Indeterminate => "indeterminate",
        };
        let records = self.receipt.verifications.iter().take(8).map(|record| record.record_id.as_str())
            .collect::<Vec<_>>().join(", ");
        let more = self.receipt.verifications.len().saturating_sub(8);
        let records = if more == 0 { records } else { format!("{records} (+{more} more in the run record)") };
        if let ReceivedNoticeSource::SourceUnavailable(reason) = self.source() {
            let reason = match reason {
                ReceivedNoticeSourceUnavailable::Deadline => "notice capture deadline elapsed",
                ReceivedNoticeSourceUnavailable::Read => "source read unavailable",
            };
            return format!("{ENGRAM_CHECK_CREDIT_LINE_PREFIX} registered launcher run {} {outcome}; \
                Engram received verification record(s) {records} for that run. Current input \
                confirmation is unavailable ({reason}); this notice confirms no current-input \
                or ordinary-check credit. No rerun is requested.", self.registration.run_id);
        }
        format!("{ENGRAM_CHECK_CREDIT_LINE_PREFIX} registered launcher run {} {outcome}; \
            Engram received verification record(s) {records} for its own claim and unchanged input. \
            This is the registered outcome; the ordinary check supplies no credit. Do not start a duplicate run.",
            self.registration.run_id)
    }
}

fn registered_launcher_notice(run: &TestRunRegistration, state: &TestRunState) -> String {
    let status = match state {
        TestRunState::Passed => "passed; consult its terminal result record",
        TestRunState::Failed => "failed; consult its terminal result record",
        _ => "still running; its outcome is pending",
    };
    format!("{ENGRAM_CHECK_CREDIT_LINE_PREFIX} registered launcher run {} is {status}. \
        It is correlated with this command's explicit launcher locator. Native verification \
        uses its own run record; do not start a duplicate run.", run.run_id)
}

impl AppState {
    fn schedule_received_launcher_notice(&self, registration: &TestRunRegistration, deadline: std::time::Instant) {
        let Some(current) = self.pending_received_launcher_registration(registration) else { return; };
        let owner = self.clone();
        if let Err(error) = self.capture_lifetime.spawn_observer("termal-received-launcher-notice", move || {
            owner.refresh_received_launcher_notice(&current, deadline);
        }) {
            eprintln!("engram> optional received notice worker failed: {error}");
        }
    }

    // Terminal paths publish their original native correlation first. Source
    // confirmation belongs to the existing cancellable capture owner, never
    // the shared provider reader. One absolute budget covers the whole batch.
    fn schedule_received_launcher_notices(&self, mut prepared: Vec<RegisteredCheckNotice>) {
        prepared.retain(|notice| notice.runs.iter().any(|(run, _)| matches!(run.verification_record,
            Some(TestRunVerificationAck::Settled { .. }))));
        if prepared.is_empty() { return; }
        let owner = self.clone();
        if let Err(error) = self.capture_lifetime.spawn_observer("termal-received-launcher-notice", move || {
            let deadline = std::time::Instant::now() + RECEIVED_NOTICE_CAPTURE_BUDGET;
            read_received_registered_check_notices_until(&mut prepared, deadline);
            for notice in prepared {
                if let Some(received) = notice.received {
                    owner.publish_received_launcher_notice(received);
                }
            }
        }) {
            eprintln!("engram> optional received notice worker failed: {error}");
        }
    }

    // A receipt can arrive after turn close but before its pending advice is
    // delivered. Replace only the exact registered-run fragment emitted by
    // the original correlation owner, preserving every other check's warning.
    // Text controls delivery only; host registration, artifact and live root
    // identity below remain the authority. Never match by command/path/time.
    fn pending_received_launcher_registration(&self, registration: &TestRunRegistration) -> Option<TestRunRegistration> {
        let current = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner.find_session_index(&registration.owning_session_id)?;
            let pending = inner.sessions[index].engram.pending_source_root_line.as_deref().unwrap_or_default();
            if ![TestRunState::Running, TestRunState::Passed, TestRunState::Failed].iter().any(|state| {
                let line = registered_launcher_notice(registration, state);
                pending.contains(line.strip_prefix(ENGRAM_CHECK_CREDIT_LINE_PREFIX).unwrap_or(&line))
            }) { return None; }
            inner.test_run_registrations.get(&registration.run_id).cloned()
        };
        current.filter(|current| current.request_identity == registration.request_identity
            && current.run_directory == registration.run_directory
            && matches!(current.verification_record, Some(TestRunVerificationAck::Settled { .. })))
    }

    fn refresh_received_launcher_notice(&self, registration: &TestRunRegistration, deadline: std::time::Instant) {
        let Some(current) = self.pending_received_launcher_registration(registration) else { return; };
        let Some(received) = ReceivedCheckNotice::read(&current, deadline) else { return; };
        self.publish_received_launcher_notice(received);
    }

    fn publish_received_launcher_notice(&self, received: ReceivedCheckNotice) {
        if check_capture_cancellation().is_err() { return; }
        let current = &received.registration;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(&current.owning_session_id) else { return; };
        if !received.is_current(&inner) { return; }
        let Some(target) = Self::engram_source_root_target_locked(&inner, &current.owning_session_id).ok().flatten()
            else { return; };
        let Some(binding) = target.work_binding.as_ref() else { return; };
        if target.settings.authority_store_key.as_ref() != current.authority_store.as_ref()
            || !inner.engram_work_source_roots.iter().any(|root| received.matches_root(root, binding))
            || !inner.test_run_observations.values().any(|row|
                engram_verification_record_row_matches(row, current)
                    && row.work_binding.as_ref() == Some(binding)) { return; }
        let engram = &mut inner.sessions[index].engram;
        let Some(pending) = engram.pending_source_root_line.as_ref() else { return; };
        let replacement = received.line();
        let replacement = replacement.strip_prefix(ENGRAM_CHECK_CREDIT_LINE_PREFIX).unwrap_or(&replacement);
        let mut lines = pending.lines().map(str::to_owned).collect::<Vec<_>>();
        let mut changed = false;
        for line in &mut lines {
            if !line.starts_with(ENGRAM_CHECK_CREDIT_LINE_PREFIX) { continue; }
            for state in [TestRunState::Running, TestRunState::Passed, TestRunState::Failed] {
                let old = registered_launcher_notice(current, &state);
                let old = old.strip_prefix(ENGRAM_CHECK_CREDIT_LINE_PREFIX).unwrap_or(&old);
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
struct EngramOtherRootNoticeRead {
    session_id: String,
    target: EngramBindingTarget,
    roots: Vec<EngramWorkSourceRoot>,
    host_turn_ref: String,
    turn_generation: u64,
    runtime: Option<RuntimeToken>,
    workdir: String,
    source_root: Option<EngramTurnSourceRoot>,
    bound_root: Option<EngramWorkSourceRoot>,
    received_candidates: Vec<TestRunRegistration>,
}

impl EngramOtherRootNoticeRead {
    fn capture(inner: &StateInner, index: usize) -> Option<Self> {
        let record = &inner.sessions[index];
        let session_id = &record.session.id;
        if AppState::engram_session_has_child_binding_shape_locked(inner, session_id) {
            return None;
        }
        let host_turn_ref = runtime_observation_check_owner(record)?.to_owned();
        let target = AppState::engram_source_root_target_locked(inner, session_id).ok()??;
        let store = target.settings.authority_store_key.as_ref()?;
        let binding = target.work_binding.as_ref()?;
        let roots: Vec<_> = inner.engram_work_source_roots.iter().filter(|entry| {
            entry.store == *store && entry.named_by_session == *session_id
                && (entry.work_id != binding.work_id || entry.claim_id != binding.claim_id)
        }).cloned().collect();
        if roots.is_empty() { return None; }
        let bound_root = inner.engram_work_source_roots.iter().find(|entry|
            entry.store == *store && entry.named_by_session == *session_id
                && entry.work_id == binding.work_id && entry.claim_id == binding.claim_id).cloned();
        // Bounded in-memory selection of this naming session's settled
        // receipts. Selection alone suppresses nothing; the body and current
        // source and canonical live binding are checked off-lock below.
        let mut received_candidates = Vec::new();
        for root in roots.iter().chain(bound_root.iter()) {
            let mut runs = inner.test_run_registrations.values().filter(|run|
                run.owning_session_id == *session_id && run.authority_store.as_ref() == Some(store)
                    && matches!(run.verification_record, Some(TestRunVerificationAck::Settled { .. }))
                    && run.named_root.as_ref().is_some_and(|named|
                        named.workspace_id == root.root && named.generation == root.generation)
                    && run.work_binding.as_ref().is_some_and(|binding|
                        binding.work_id == root.work_id && binding.claim_id == root.claim_id))
                .collect::<Vec<_>>();
            runs.sort_by(|a, b| b.registered_at.cmp(&a.registered_at));
            received_candidates.extend(runs.into_iter().take(8).cloned());
        }
        Some(Self { session_id: session_id.clone(), target, roots, host_turn_ref,
            turn_generation: record.active_turn_generation,
            runtime: record.runtime.runtime_token(),
            workdir: record.session.workdir.clone(),
            source_root: record.engram.observed_source_root().cloned(), bound_root, received_candidates })
    }

    fn read(&self, deadline: std::time::Instant, shared: bool) -> Result<
        (Vec<EngramWorkSourceRoot>, Vec<(EngramWorkSourceRoot, ReceivedCheckNotice)>),
        ReceivedNoticeSourceUnavailable,
    > {
        let timeout = received_notice_step_timeout(deadline,
            self.target.settings.call_timeout().min(ENGRAM_WORK_BINDING_COMMAND_TIMEOUT));
        if timeout.is_zero() { return Err(ReceivedNoticeSourceUnavailable::Deadline); }
        let held = self.target.adapter.read_held_claims(&self.target.connection,
            timeout);
        let held = held.map_err(|_| if std::time::Instant::now() >= deadline {
            ReceivedNoticeSourceUnavailable::Deadline
        } else { ReceivedNoticeSourceUnavailable::Read })?;
        if std::time::Instant::now() >= deadline { return Err(ReceivedNoticeSourceUnavailable::Deadline); }
        let roots = self.roots.iter().filter(|entry| held.items.iter().any(|claim| {
            claim.work_id == entry.work_id && claim.claim_id == entry.claim_id
        })).cloned().collect();
        let mut received = Vec::new();
        for root in self.roots.iter().filter(|_| shared).chain(self.bound_root.iter().filter(|_| !shared)) {
            let Some(binding) = held.items.iter().find(|claim|
                claim.work_id == root.work_id && claim.claim_id == root.claim_id
                    && claim.claim_fence == root.claim_fence)
                .and_then(|claim| claim.control_binding.as_ref()) else { continue; };
            for run in &self.received_candidates {
                if !ReceivedCheckNotice::registration_matches_root(run, root, binding) { continue; }
                if let Some(notice) = ReceivedCheckNotice::read(run, deadline)
                    .filter(|notice| notice.matches_root(root, binding)) {
                    received.push((root.clone(), notice));
                    break;
                }
            }
        }
        Ok((roots, received))
    }

    fn is_current(&self, inner: &StateInner, index: usize) -> bool {
        let record = &inner.sessions[index];
        record.active_turn_generation == self.turn_generation
            && record.runtime.runtime_token() == self.runtime
            && record.session.workdir == self.workdir
            && record.engram.observed_source_root() == self.source_root.as_ref()
            && runtime_observation_check_owner(record) == Some(self.host_turn_ref.as_str())
            && AppState::engram_source_root_target_locked(inner, &record.session.id)
                .ok().flatten().is_some_and(|current| {
                    current.connection == self.target.connection
                        && current.project_id == self.target.project_id
                        && current.work_binding == self.target.work_binding
                        && current.settings.same_work_service_settings(&self.target.settings)
                })
    }

    fn schedule(self, state: &AppState, bound: String, shared: bool, feedback_revision: u64) {
        if self.roots.is_empty() { return; }
        let owner = state.clone();
        if let Err(error) = state.capture_lifetime.spawn_observer("termal-other-root-notice", move || {
            let deadline = std::time::Instant::now() + RECEIVED_NOTICE_CAPTURE_BUDGET;
            let (mut roots, received, authority_unavailable) = match self.read(deadline, shared) {
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
            let mut unavailable = if let Some(reason) = authority_unavailable {
                let reason = match reason {
                    ReceivedNoticeSourceUnavailable::Deadline => "notice capture deadline elapsed",
                    ReceivedNoticeSourceUnavailable::Read => "held-claim read unavailable",
                };
                vec![format!("[TermAl] Named-root claim confirmation is unavailable ({reason}); this notice confirms no current-input or ordinary-check credit. No rerun is requested.")]
            } else { Vec::new() };
            if shared {
                roots.retain(|root| {
                    let Some((_, notice)) = received.iter().find(|(verified, _)| verified == root) else { return true; };
                    if matches!(notice.source(), ReceivedNoticeSource::SourceUnavailable(_)) {
                        unavailable.push(notice.line());
                    }
                    false
                });
            } else if let Some((_, notice)) = received.iter().find(|(root, _)| Some(root) == self.bound_root.as_ref()) {
                if notice.source() == ReceivedNoticeSource::Current { return; }
                unavailable.push(notice.line());
                roots.clear();
            }
            if roots.is_empty() && unavailable.is_empty() { return; }
            let others = roots.iter().map(|entry| entry.short_ref.as_str())
                .collect::<Vec<_>>().join(", ");
            let line = if roots.is_empty() {
                unavailable.join("\n")
            } else if shared {
                format!("[TermAl] A recognised test in a shared named root is reported for \
                    bound item {bound} only; {others} receives no credit. Make that item \
                    the focus and run its test in the next turn for its own credit.")
            } else {
                format!("[TermAl] A recognised test ran in the named root of {others}, \
                    outside bound item {bound}'s root, and gets no credit for {bound}. \
                    Make that item the focus and run its test in the next turn.")
            };
            let line = if !roots.is_empty() && !unavailable.is_empty() {
                format!("{line}\n{}", unavailable.join("\n"))
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
