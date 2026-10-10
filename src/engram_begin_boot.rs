// Boot reconstruction of a begin whose outcome a host restart left unknown.
// A saved head whose retained evaluate carries a Prepared begin is read as
// begin unknown even when the host stopped before it recorded the begin's
// failure: its grant becomes the session's uncertain grant and the exact
// begin replay (`engram_begin_replay.rs`) is scheduled. A head saved by a host
// before the prepared begin (a retained evaluate and an uncertain grant only)
// is scheduled for reconstruction: its exact retained evaluate is replayed,
// and only the grant Engram returns for it, when that is the uncertain grant,
// is begun again, under the key derived from that evaluate as the earlier
// host derived it. Another grant holds the head; no begin is fabricated.
// Owns the reconstruction predicate, its retry code and preview, and the boot
// pass that reads saved heads and schedules their recovery. Does not own the
// replay itself, the schedule, tick and acknowledgement
// (`engram_admission_retry.rs`, `engram_retry_schedule.rs`), the cold-restore
// and bind routes that leave these heads to that recovery
// (`engram_queued_admission.rs`, `engram_host_adapter.rs`), or the evaluate
// replay and the Begin send (`engram_host_adapter.rs`). New module, kept apart
// so that adapter does not grow.

/// The admission-retry code of a scheduled reconstruction: the retained
/// evaluate is replayed to recover the begin an earlier host may have sent.
const ENGRAM_BEGIN_RECONSTRUCT_CODE: &str = "begin_reconstruct";

/// The grant a head saved by a host before the prepared begin may have
/// begun: the session's uncertain grant, beside a retained evaluate with no
/// prepared begin and no begun grant, while no grant is active. An uncertain
/// grant whose id was never learned cannot be matched to an answer, and keeps
/// its hold. The shape alone decides only what the boot pass schedules; the
/// routes that leave a head to reconstruction require its scheduled record
/// (`engram_begin_reconstruction_pending`).
fn engram_begin_reconstruction_shape(record: &SessionRecord) -> Option<&str> {
    let uncertain = record.engram.uncertain_grant_id.as_deref()?;
    if record.engram.active_grant_id.is_some() || uncertain == ENGRAM_UNCERTAIN_GRANT_UNKNOWN {
        return None;
    }
    let evaluate = record.queued_prompts.front()?.engram_evaluate.as_ref()?;
    (evaluate.prepared_begin.is_none()
        && evaluate.begun_grant_id.is_none()
        && matches!(evaluate.request, EngramControlRequest::TurnEvaluate { .. }))
    .then_some(uncertain)
}

/// The grant a head awaiting reconstruction may have begun: the shape above,
/// while the session's retry record is the reconstruction the boot pass
/// scheduled. Without that record the head keeps the routes it had before.
fn engram_begin_reconstruction_pending(record: &SessionRecord) -> Option<&str> {
    record
        .engram
        .admission_retry
        .as_ref()
        .is_some_and(|retry| retry.code == ENGRAM_BEGIN_RECONSTRUCT_CODE)
        .then(|| engram_begin_reconstruction_shape(record))
        .flatten()
}

fn engram_begin_reconstruct_preview(retry: &EngramAdmissionRetry) -> String {
    format!(
        "{ENGRAM_ADMISSION_RETRY_PREVIEW_PREFIX} (begin reconciliation: replaying the retained \
         evaluate to recover the same begin), attempt {}, next at {}.",
        retry.attempts, retry.due_at
    )
}

/// What the boot pass does for one saved session: the recovery to schedule,
/// and the grant to mark possibly begun first, if any.
struct EngramBeginBootPlan {
    code: &'static str,
    mark_uncertain: Option<String>,
}

/// The boot pass's reading of `record`, without changing it. A Prepared begin
/// whose grant nothing marks possibly begun was acknowledged before its send,
/// and the host may have made that send before it stopped, so its grant is
/// possibly begun and its exact replay is scheduled. That holds even beside an
/// ordinary retry record: the record's own attempt may have prepared the
/// begin, and the replay replaces it. A begin-unknown head
/// (`engram_begin_replay_pending`) is scheduled its replay and a pre-fix head
/// its reconstruction. A head with a begin recovery already scheduled, or
/// one another hold owns (an interrupted one, a Stop, an operator pause,
/// control disabled, an unknown transcript position), is left as it is.
fn engram_begin_boot_plan(record: &SessionRecord) -> Option<EngramBeginBootPlan> {
    let retry_code = record.engram.admission_retry.as_ref().map(|retry| retry.code.as_str());
    let eligible = !matches!(
        retry_code,
        Some(code) if code == ENGRAM_BEGIN_REPLAY_CODE || code == ENGRAM_BEGIN_RECONSTRUCT_CODE
    ) && record.engram.disabled_reason.is_none()
        && !record.engram.operator_paused
        && !record.runtime_stop_in_progress
        && record.queued_prompts.front().is_some_and(|head| {
            !head.engram_interrupted && head.promotion_disposition_known
        });
    if !eligible {
        return None;
    }
    if record.engram.uncertain_grant_id.is_none()
        && record.engram.active_grant_id.is_none()
        && let Some(grant_id) = record
            .queued_prompts
            .front()
            .and_then(|head| head.engram_evaluate.as_ref())
            .filter(|evaluate| evaluate.begun_grant_id.is_none())
            .and_then(|evaluate| evaluate.prepared_begin.as_ref())
            .filter(|prepared| prepared.phase == EngramPreparedBeginPhase::Prepared)
            .map(|prepared| prepared.grant_id.clone())
    {
        return Some(EngramBeginBootPlan {
            code: ENGRAM_BEGIN_REPLAY_CODE,
            mark_uncertain: Some(grant_id),
        });
    }
    // Any other head with a retry record keeps it.
    if retry_code.is_some() {
        return None;
    }
    let code = if engram_begin_replay_pending(record).is_some() {
        ENGRAM_BEGIN_REPLAY_CODE
    } else if engram_begin_reconstruction_shape(record).is_some() {
        ENGRAM_BEGIN_RECONSTRUCT_CODE
    } else {
        return None;
    };
    Some(EngramBeginBootPlan {
        code,
        mark_uncertain: None,
    })
}

/// Applies `plan` to `record` under `authority`. The head waits for this
/// recovery and nothing else: parked by an earlier host, or caught
/// mid-admission when that host stopped. Without an authority nothing is
/// scheduled, and the head keeps the explicit hold, with its grant still
/// marked possibly begun.
fn apply_engram_begin_boot_plan(
    record: &mut SessionRecord,
    plan: EngramBeginBootPlan,
    authority: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
    budget_now: std::time::Instant,
) {
    if let Some(grant_id) = plan.mark_uncertain {
        record.engram.uncertain_grant_id = Some(grant_id);
    }
    if let Some(head) = record.queued_prompts.front_mut() {
        head.engram_waiting = true;
    }
    record.set_auto_dispatch_blocked(true);
    schedule_engram_admission_retry_locked(record, Some(plan.code), authority, now, budget_now);
}

impl AppState {
    /// Whether `session_id`'s head is awaiting reconstruction of a begin an
    /// earlier host may have sent and, given an owner, is still that owner's
    /// head: no status read clears its grant and no restart checkpoint closes
    /// it before the retained evaluate's answer is known.
    fn engram_begin_reconstruction_for(
        &self,
        session_id: &str,
        owner: Option<&EngramQueuedAdmissionOwner>,
    ) -> Option<String> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner.find_session_index(session_id)?];
        if owner.is_some_and(|owner| !owner.matches(record)) {
            return None;
        }
        engram_begin_reconstruction_pending(record).map(str::to_owned)
    }

    /// Under boot preparation's lock, before boot recovery picks its targets:
    /// the boot pass (`engram_begin_boot_plan`) over every session, under the
    /// authority its admission would run under now. Only a session it changes
    /// is stamped, and the budget clock is read only once one does. Returns
    /// whether a record changed.
    fn reconstruct_engram_begin_recovery_on_boot_locked(&self, inner: &mut StateInner) -> bool {
        let now = chrono::Utc::now();
        let mut budget_now = None;
        let mut changed = false;
        for index in 0..inner.sessions.len() {
            let Some(plan) = engram_begin_boot_plan(&inner.sessions[index]) else {
                continue;
            };
            // The caller holds the state lock, so the clock is read from the
            // state it holds: `engram_budget_clock` takes that lock again in
            // tests.
            let budget_now =
                *budget_now.get_or_insert_with(|| inner.engram_budget_clock_snapshot().now());
            let session_id = inner.sessions[index].session.id.clone();
            // The recovery runs under the authority of the retained evaluate:
            // the current target's, only while the evaluate was issued under
            // that same connection and those settings. Otherwise nothing is
            // scheduled and the head keeps its explicit hold.
            let retained = inner.sessions[index]
                .queued_prompts
                .front()
                .and_then(|head| head.engram_evaluate.as_ref());
            let authority =
                Self::engram_binding_target_for_session_shape_locked(inner, &session_id, true)
                    .ok()
                    .flatten()
                    .filter(|target| {
                        retained.is_some_and(|evaluate| {
                            evaluate.connection == target.connection
                                && evaluate.settings.same_admission_settings(&target.settings)
                        })
                    })
                    .map(|target| engram_abort_authority(&target));
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            apply_engram_begin_boot_plan(record, plan, authority.as_deref(), now, budget_now);
            changed = true;
        }
        changed
    }
}
