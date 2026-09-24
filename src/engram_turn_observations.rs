// Execution observations a mediated turn reports on the checkpoint that
// closes its Engram grant (Engram w-108a13d58018, tm-winf). Owns the turn
// report: what one observation says, how the source basis is taken and
// bounded, how a begin-time basis makes `source_changed` authoritative, and
// how the record keeps the report for retries. Also hosts
// `engram_evaluate_requested_effects`, the reader of a prepared evaluate's
// requested effects, which admission's begin-refusal re-evaluation shares.
// Does not own the checkpoint request, the grant lifecycle or the work
// binding, which stay in `engram_host_adapter.rs`. New fragment beside that
// file, created instead of growing it.

/// What a checkpoint's report needs. Taken twice under the lock: once before
/// the off-lock basis capture, to decide whether one is needed, and again
/// under the checkpoint claim, to build the report from what the record holds
/// by then.
enum EngramTurnReportPlan {
    /// The session is unbound, or the checkpoint closes a turn this process
    /// never saw end: nothing is reported.
    Nothing,
    /// An earlier attempt for this grant built the report; it is repeated
    /// verbatim so the idempotency key repeats.
    Cached(Vec<EngramExecutionObservationInput>),
    /// A report is built for this attempt once the end basis has been read
    /// from `workdir` off-lock.
    Fresh {
        workdir: String,
        outcome: EngramExecutionOutcome,
    },
}

/// Decides what a checkpoint closing `grant_id` with `outcome` reports.
/// Engram admits observations only from a session bound to claimed work; an
/// unbound session's checkpoint reports nothing, or the evidence gate would
/// reject it and leave the turn open. `None` closes a grant whose turn this
/// process never saw end and reports nothing either.
fn engram_turn_report_plan(
    record: &SessionRecord,
    grant_id: &str,
    outcome: Option<EngramExecutionOutcome>,
) -> EngramTurnReportPlan {
    let Some(outcome) = outcome else {
        return EngramTurnReportPlan::Nothing;
    };
    if record.engram.work_binding.is_none() {
        return EngramTurnReportPlan::Nothing;
    }
    match &record.engram.active_turn_report {
        Some((cached_grant_id, observations)) if cached_grant_id == grant_id => {
            EngramTurnReportPlan::Cached(observations.clone())
        }
        _ => EngramTurnReportPlan::Fresh {
            workdir: record.session.workdir.clone(),
            outcome,
        },
    }
}

/// Whether `grant_id`, the grant closing now, mediates local mutation, which
/// is what Engram checks an observation's effect against: the effects its
/// own evaluate requested when recorded for this very grant, otherwise the
/// session's current set.
fn engram_turn_mutation_granted(
    record: &SessionRecord,
    grant_id: &str,
    target: Option<&EngramBindingTarget>,
) -> bool {
    engram_grant_mediates_mutation(
        record.engram.active_turn_grant_mutates.as_ref(),
        grant_id,
        target.is_some_and(|target| {
            target
                .effects
                .iter()
                .any(|effect| matches!(effect, EngramEffect::MutateLocal))
        }),
    )
}

/// The rule behind [`engram_turn_mutation_granted`]: a flag recorded for this
/// very grant decides; one recorded for another grant, or none, leaves it to
/// whether the session's current set mediates mutation.
fn engram_grant_mediates_mutation(
    recorded: Option<&(String, bool)>,
    grant_id: &str,
    current_set_mutates: bool,
) -> bool {
    match recorded {
        Some((recorded_grant_id, mutates)) if recorded_grant_id == grant_id => *mutates,
        _ => current_set_mutates,
    }
}

/// Whether the prepared evaluate on the session's queued head requested
/// `mutate_local`, when that evaluate is the one the mirrored grant was
/// issued for (its intent is `released_intent`); `None` when no such
/// evaluate is there to say, and the current set decides.
fn engram_prepared_evaluate_requests_mutation(
    record: &SessionRecord,
    released_intent: Option<&str>,
) -> Option<bool> {
    let prepared = record.queued_prompts.front()?.engram_evaluate.as_ref()?;
    engram_evaluate_requests_mutation(&prepared.request, released_intent?)
}

/// Whether `request`, an evaluate issued for `intent`, requested
/// `mutate_local`; `None` for any other request, or one for another intent.
fn engram_evaluate_requests_mutation(
    request: &EngramControlRequest,
    intent: &str,
) -> Option<bool> {
    engram_evaluate_requested_effects(request, intent).map(|requested_effects| {
        requested_effects
            .iter()
            .any(|effect| matches!(effect, EngramEffect::MutateLocal))
    })
}

/// The effects `request`, an evaluate issued for `intent`, requested; `None`
/// for any other request, or one for another intent.
fn engram_evaluate_requested_effects<'a>(
    request: &'a EngramControlRequest,
    intent: &str,
) -> Option<&'a [EngramEffect]> {
    match request {
        EngramControlRequest::TurnEvaluate {
            requested_effects,
            intent_fingerprint,
            ..
        } if intent_fingerprint == intent => Some(requested_effects),
        _ => None,
    }
}

/// The execution observation a mediated turn reports on the checkpoint that
/// closes its grant, built under the checkpoint's lock before the terminal
/// transition clears the turn's file-change tracking. `source_changed` is
/// authoritative when both the begin-time and the end-time basis exist and
/// their revisions differ; the turn's file-change tracking, a debounced hint
/// that can arrive late or skip ignored paths, is only a lower bound, and
/// decides alone when the closing basis is missing. A turn whose begin-time
/// basis is missing but whose closing basis exists cannot be cleared by the
/// comparison and counts as changed under a mutation grant. A changed source is
/// reported with the `mutate_local` effect Engram requires for it, which
/// must be one of the grant's requested effects; a turn that changed source
/// under a grant that mediates no local mutation is withheld rather than
/// misreported, and the omission is logged. The id is deterministic for the
/// grant; the basis and its time are the moment's, which is why the record
/// keeps the finished report for retries.
fn engram_turn_execution_observation(
    record: &SessionRecord,
    session_id: &str,
    grant_id: &str,
    outcome: EngramExecutionOutcome,
    mutation_granted: bool,
    end_basis: Option<EngramExecutionSourceBasis>,
) -> Option<EngramExecutionObservationInput> {
    let tracked_change = !record.active_turn_file_changes.is_empty();
    let content_changed = match (&record.engram.active_turn_start_basis, &end_basis) {
        (Some(start), Some(end)) => start.source_revision != end.source_revision,
        // Without a begin-time basis the comparison cannot clear the turn.
        // Under a grant that mediates local mutation the conservative answer
        // is a change, which opens an obligation a later check can satisfy;
        // under an observe-only grant a change could only withhold the
        // report, so the tracking decides.
        (None, Some(_)) => mutation_granted,
        _ => false,
    };
    let source_changed = tracked_change || content_changed;
    if source_changed && !mutation_granted {
        eprintln!(
            "engram> session={session_id} turn changed source under a grant that mediates no \
             local mutation; its checkpoint reports no observation"
        );
        return None;
    }
    let observed_at = end_basis
        .as_ref()
        .map(|_| chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    Some(EngramExecutionObservationInput {
        observation_id: sha256_hex(
            format!("termal-turn-observation:{session_id}:{grant_id}").as_bytes(),
        ),
        // The intent this grant was issued for. A grant restored across a
        // restart has none in memory; its observation then names the grant.
        action_fingerprint: record
            .engram
            .active_turn_intent_fingerprint
            .clone()
            .unwrap_or_else(|| sha256_hex(format!("termal-turn-grant:{grant_id}").as_bytes())),
        effect: if source_changed {
            EngramEffect::MutateLocal
        } else {
            EngramEffect::Observe
        },
        outcome,
        source_changed,
        source_basis: end_basis,
        observed_at,
    })
}

/// The source identity of a workspace at one moment: the canonical worktree
/// root the fingerprint was taken on, so two captures of the same root
/// compare equal however the path was spelled, and the schema-1 review-freeze
/// fingerprint of its full working content, tracked and untracked, which is
/// what Engram compares a later check's revision with. Bounded by the
/// reviewer's shared freeze budget ([`REVIEW_FREEZE_TIMEOUT`]), the bound the
/// same fingerprint of the same workspace already runs under: the capture
/// spawns eight Git processes in sequence, and the moment a turn closes is
/// often the host's busiest, so a tighter bound would drop the basis exactly
/// when Git is slow rather than stuck. The bound is paid only in that case;
/// the closing capture precedes the checkpoint claim, so teardown's settle
/// wait never spans it. Absent, and logged, when the workspace is not a
/// worktree root or the fingerprint cannot be taken in time; the contract
/// keeps the basis optional, at the price of an obligation that can only be
/// waived.
fn engram_execution_source_basis(workdir: &FsPath) -> Option<EngramExecutionSourceBasis> {
    match review_freeze_fingerprint(workdir) {
        Ok((root, source_revision)) => Some(EngramExecutionSourceBasis {
            workspace_id: root.to_string_lossy().into_owned(),
            source_revision,
        }),
        Err(error) => {
            eprintln!(
                "engram> no source basis for workspace {}: {error:#}",
                workdir.display()
            );
            None
        }
    }
}

impl AppState {
    /// Takes the begin-time source basis of the turn `grant_id` begins, off
    /// the lock and bounded, and keeps it on the record while that grant is
    /// mirrored, so the closing checkpoint can tell whether the turn changed
    /// the workspace's content. Only a session bound to claimed work reports
    /// observations, so an unbound session skips the capture.
    fn record_engram_turn_start_basis_off_lock(&self, session_id: &str, grant_id: &str) {
        let workdir = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            let record = &inner.sessions[index];
            if record.engram.work_binding.is_none()
                || record.engram.active_grant_id.as_deref() != Some(grant_id)
            {
                return;
            }
            record.session.workdir.clone()
        };
        let basis = engram_execution_source_basis(FsPath::new(&workdir));
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if let Some(index) = inner.find_session_index(session_id)
            && inner.sessions[index].engram.active_grant_id.as_deref() == Some(grant_id)
        {
            inner.sessions[index].engram.active_turn_start_basis = basis;
        }
    }

    /// The report this process built for `grant_id`, for a checkpoint that
    /// closes the grant outside the turn's own close, such as the recovery
    /// checkpoint of a rebind. Empty when none was built, after a restart, or
    /// after Engram refused it.
    fn cached_engram_turn_report(
        &self,
        session_id: &str,
        grant_id: &str,
    ) -> Vec<EngramExecutionObservationInput> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner
            .find_session_index(session_id)
            .and_then(|index| inner.sessions[index].engram.active_turn_report.as_ref())
            .filter(|(cached_grant_id, _)| cached_grant_id == grant_id)
            .map(|(_, observations)| observations.clone())
            .unwrap_or_default()
    }

    /// Engram answered a checkpoint that carried `grant_id`'s report by
    /// refusing it. The report is replaced by an empty one, so every later
    /// checkpoint of the grant, the turn's own retries and a rebind's
    /// recovery alike, goes without it, as every checkpoint did before turns
    /// reported observations. Any refusal counts, not only one about the
    /// payload: a refused report resent forever would hold the grant open,
    /// and the session's queued prompts with it, until a restart, while
    /// dropping it costs at most this turn's evidence, and nothing when the
    /// refusal is about the grant's state after an earlier attempt was
    /// accepted. A lost or timed-out call is not an answer; its report is
    /// kept for the retry.
    fn forget_refused_engram_turn_report(
        &self,
        session_id: &str,
        grant_id: &str,
        code: Option<&str>,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let holds_report = inner.sessions[index]
            .engram
            .active_turn_report
            .as_ref()
            .is_some_and(|(cached_grant_id, observations)| {
                cached_grant_id == grant_id && !observations.is_empty()
            });
        if !holds_report {
            // Already dropped, or a newer grant's report: nothing to forget.
            return;
        }
        inner.sessions[index].engram.active_turn_report = Some((grant_id.to_owned(), Vec::new()));
        drop(inner);
        eprintln!(
            "engram> session={session_id} Engram refused the checkpoint carrying the turn's \
             execution observation ({}); the grant's next closing attempt goes without it",
            code.unwrap_or("unknown")
        );
    }
}

/// The refusal code when Engram answered a rebind's recovery checkpoint by
/// refusing it, as an error or as a refuse result; `None` for acceptance, for
/// a lost or timed-out call, and for the issued-but-never-begun answer, which
/// a fresh bind settles and which says nothing about the report.
fn engram_recovery_checkpoint_refusal(
    checkpoint: &std::result::Result<EngramTurnCheckpointResponse, EngramTransportError>,
) -> Option<Option<&str>> {
    match checkpoint {
        Err(error)
            if error.kind == EngramTransportErrorKind::Remote
                && !engram_grant_was_issued_but_not_begun(error) =>
        {
            Some(error.code.as_deref())
        }
        Ok(EngramTurnCheckpointResponse::Refuse { code })
            if !engram_grant_code_was_issued_but_not_begun(code) =>
        {
            Some(Some(code.as_str()))
        }
        _ => None,
    }
}

// Test-only gate between a checkpoint's off-lock basis capture and its
// claim, so a test can hold one closer of a grant there while another closer
// claims, reports and loses its reply. Same shape as the Stop fence gate in
// `session_lifecycle.rs`: keyed by state and session, claimed once, released
// by the fixture (also on unwind).
#[cfg(test)]
struct TestEngramTurnReportGate {
    claimed_tx: std::sync::mpsc::Sender<()>,
    release_rx: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
type TestEngramTurnReportGateKey = (usize, String);

#[cfg(test)]
static TEST_ENGRAM_TURN_REPORT_GATES: std::sync::LazyLock<
    std::sync::Mutex<HashMap<TestEngramTurnReportGateKey, TestEngramTurnReportGate>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

#[cfg(test)]
struct TestEngramTurnReportGateControl {
    key: TestEngramTurnReportGateKey,
    claimed_rx: std::sync::mpsc::Receiver<()>,
    release_tx: std::sync::mpsc::Sender<()>,
}

#[cfg(test)]
impl TestEngramTurnReportGateControl {
    fn wait_until_claimed(&self) {
        let started = std::time::Instant::now();
        self.claimed_rx
            .recv_timeout(TEST_PHASE_DEADLOCK_GUARD)
            .unwrap_or_else(|error| {
                panic!(
                    "no checkpoint of {} reached its report gate after {:?}: {error}",
                    self.key.1,
                    started.elapsed()
                )
            });
    }

    fn release(&self) {
        self.release_tx
            .send(())
            .expect("report gate should remain connected");
    }
}

#[cfg(test)]
impl Drop for TestEngramTurnReportGateControl {
    fn drop(&mut self) {
        let _ = self.release_tx.send(());
        TEST_ENGRAM_TURN_REPORT_GATES
            .lock()
            .expect("test report gate mutex poisoned")
            .remove(&self.key);
    }
}

#[cfg(test)]
fn test_engram_turn_report_gate_key(state: &AppState, session_id: &str) -> TestEngramTurnReportGateKey {
    (Arc::as_ptr(&state.inner) as usize, session_id.to_owned())
}

#[cfg(test)]
fn install_test_engram_turn_report_gate(
    state: &AppState,
    session_id: &str,
) -> TestEngramTurnReportGateControl {
    let (claimed_tx, claimed_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let key = test_engram_turn_report_gate_key(state, session_id);
    TEST_ENGRAM_TURN_REPORT_GATES
        .lock()
        .expect("test report gate mutex poisoned")
        .insert(
            key.clone(),
            TestEngramTurnReportGate {
                claimed_tx,
                release_rx,
            },
        );
    TestEngramTurnReportGateControl {
        key,
        claimed_rx,
        release_tx,
    }
}

#[cfg(test)]
fn wait_at_test_engram_turn_report_gate(state: &AppState, session_id: &str) {
    let gate = TEST_ENGRAM_TURN_REPORT_GATES
        .lock()
        .expect("test report gate mutex poisoned")
        .remove(&test_engram_turn_report_gate_key(state, session_id));
    if let Some(gate) = gate
        && gate.claimed_tx.send(()).is_ok()
    {
        let _ = gate.release_rx.recv();
    }
}
