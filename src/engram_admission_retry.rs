// Automatic re-admission of a retained Engram prompt parked after an unknown
// admission outcome: an evaluate that missed its deadline or lost its
// transport with no grant begun, or a control that was unavailable, open-
// circuited, backed off or out of budget. Owns the durable parked-admission
// retry record (`EngramAdmissionRetry`), which retry outcomes may enter it
// (`schedule_engram_admission_retry_locked`, called by the park), its step,
// release check and postponement, and its reconstruction after a
// restart, which also takes over a bind retry left on a retained head
// (`rebuild_engram_admission_retry_on_load`). Each attempt replays the exact
// retained intent through the ordinary admission path, so a retained
// evaluate goes out again with its original idempotency key; it is an
// automatic, owned Resume of that exact head. Does not own the delays, the
// durable acknowledgement, the one-attempt-per-head guard, the host-wide cap,
// the boot hold or the tick (`engram_retry_schedule.rs`), the public Stop of
// a retry head (`stop_engram_abort_retry` in `engram_abort_retry.rs`), the park itself
// (`park_unknown_engram_authorization` in `engram_queued_admission.rs`), a
// known Defer's own retry (it parks as before), or any hold this record does
// not name: a Reconcile disposition (a begin unknown included), a refusal,
// control disabled, a Stop or Cancel, an operator queue pause, a changed
// authority or a superseded owner keep the explicit Resume-or-Cancel hold.
// New module: before it, such a park waited for Resume indefinitely.

/// The Degraded card codes whose park enters the automatic schedule: no
/// grant was begun and replaying the retained intent is the reconciliation.
/// The same list is the Retry arm of `engram_admission_disposition`.
const ENGRAM_ADMISSION_RETRY_CODES: [&str; 5] = [
    "deadline_exceeded",
    "control_unavailable",
    "control_circuit_open",
    "control_backoff",
    "dispatch_budget_exhausted",
];

/// The preview of a parked head that is not scheduled for an automatic
/// retry, as every park showed before the schedule existed.
const ENGRAM_ADMISSION_HELD_PREVIEW: &str =
    "Engram: Waiting/Unknown. Original prompt retained; resume to retry or cancel.";

/// The start of the preview of a head scheduled for an automatic retry.
const ENGRAM_ADMISSION_RETRY_PREVIEW_PREFIX: &str =
    "Engram: waiting for admission; retrying automatically";

/// A parked head whose admission outcome is unknown and whose retained
/// intent is replayed automatically when due. Saved with the session, so a
/// restart after its acknowledgement was saved rebuilds the retry.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct EngramAdmissionRetry {
    /// The queue head this record is for; any other head drops it.
    prompt_id: String,
    /// The head's intent fingerprint; a changed prompt drops the record.
    fingerprint: String,
    /// The card code the last attempt ended with.
    code: String,
    /// The attempt the schedule runs next, the first automatic one being 1.
    attempts: u32,
    /// When the prompt was first held (RFC 3339); later attempts keep it.
    held_since: String,
    /// When the next attempt is due (RFC 3339).
    due_at: String,
    /// The authority the parked admission ran under (`engram_abort_authority`).
    authority: String,
    /// The dispatch generation of the retained intent. Anything that holds the
    /// head again (a Stop, a cancellation, a Defer, a project reset) moves it.
    generation: u64,
    /// The record is durably acknowledged. Saved once the acknowledgement
    /// arrived, so a record read back without it loads as the hold.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    acknowledged: bool,
}

fn engram_admission_retry_preview(retry: &EngramAdmissionRetry) -> String {
    format!(
        "{ENGRAM_ADMISSION_RETRY_PREVIEW_PREFIX}, attempt {}, next at {}.",
        retry.attempts, retry.due_at
    )
}

/// Whether the parked head still is exactly what `retry` was scheduled for:
/// the same prompt and intent at the same generation, parked, not
/// interrupted, with no grant begun or possibly begun, control not disabled
/// and the queue not paused by an operator.
fn engram_admission_retry_head_matches(record: &SessionRecord, retry: &EngramAdmissionRetry) -> bool {
    record.engram.dispatch_generation == retry.generation
        && record.engram.active_grant_id.is_none()
        && record.engram.uncertain_grant_id.is_none()
        && record.engram.disabled_reason.is_none()
        && !record.engram.operator_paused
        && record.queued_prompts.front().is_some_and(|head| {
            head.pending_prompt.id == retry.prompt_id
                && head.engram_waiting
                && !head.engram_interrupted
                && head
                    .engram_evaluate
                    .as_ref()
                    .is_none_or(|prepared| prepared.begun_grant_id.is_none())
                && engram_turn_intent_fingerprint(
                    &head.pending_prompt.text,
                    head.pending_prompt.expanded_text.as_deref(),
                    &head.attachments,
                    head.pending_prompt.source.as_ref(),
                    head.source,
                ) == retry.fingerprint
        })
}

/// Forgets the session's parked-admission retry, with its acknowledgement.
fn clear_engram_admission_retry(record: &mut SessionRecord) {
    if record.engram.admission_retry.take().is_some() {
        record.engram.abort_retry_fence = None;
        record.engram.abort_retry_acknowledged = false;
        record.engram.abort_retry_saved = false;
    }
}

/// Drops `record`'s retry and, when its preview still announced the retry,
/// shows the explicit hold instead. A Stop's or a Cancel's own preview stays.
fn drop_engram_admission_retry(record: &mut SessionRecord) {
    clear_engram_admission_retry(record);
    if record
        .session
        .preview
        .starts_with(ENGRAM_ADMISSION_RETRY_PREVIEW_PREFIX)
    {
        record.session.preview = ENGRAM_ADMISSION_HELD_PREVIEW.to_owned();
        sync_pending_prompts(record);
    }
}

/// Schedules, on the head just parked under the lock, the automatic retry of
/// its unknown admission when `code` is one the schedule takes and nothing
/// else holds the head; otherwise forgets any earlier retry so the explicit
/// hold stays. `authority` is the authority the admission would run under
/// now. A replay that parks again keeps the first-held time and moves to the
/// next attempt. Returns whether a retry is scheduled; the caller persists
/// and asks for its acknowledgement.
fn schedule_engram_admission_retry_locked(
    record: &mut SessionRecord,
    code: Option<&str>,
    authority: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
    budget_now: std::time::Instant,
) -> bool {
    let fingerprint = record.queued_prompts.front().map(|head| {
        engram_turn_intent_fingerprint(
            &head.pending_prompt.text,
            head.pending_prompt.expanded_text.as_deref(),
            &head.attachments,
            head.pending_prompt.source.as_ref(),
            head.source,
        )
    });
    let (Some(code), Some(authority), Some(fingerprint), Some(prompt_id)) = (
        code.filter(|code| ENGRAM_ADMISSION_RETRY_CODES.contains(code)),
        authority,
        fingerprint,
        record
            .queued_prompts
            .front()
            .map(|head| head.pending_prompt.id.clone()),
    ) else {
        drop_engram_admission_retry(record);
        return false;
    };
    let prior = record
        .engram
        .admission_retry
        .clone()
        .filter(|retry| retry.prompt_id == prompt_id);
    let attempts = prior
        .as_ref()
        .map_or(1, |retry| retry.attempts.saturating_add(1));
    // A head first held by a bind retry keeps that first-held time, as a
    // restart's conversion of that bind retry does.
    let held_since = prior
        .map(|retry| retry.held_since)
        .or_else(|| {
            record
                .engram
                .bind_retry
                .as_ref()
                .filter(|bind| bind.proof.prompt_id == prompt_id)
                .and_then(|bind| bind.held_since.clone())
        })
        .unwrap_or_else(|| now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    let mut due = now + engram_abort_retry_delay(&record.session.id, attempts);
    // Never earlier than the circuit deadline or the bind backoff.
    if let Some(backoff) = record
        .engram
        .next_bind_retry_at
        .and_then(|at| at.checked_duration_since(budget_now))
        .and_then(|left| chrono::Duration::from_std(left).ok())
    {
        due = due.max(now + backoff);
    }
    let retry = EngramAdmissionRetry {
        prompt_id,
        fingerprint,
        code: code.to_owned(),
        attempts,
        held_since,
        due_at: due.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        authority: authority.to_owned(),
        generation: record.engram.dispatch_generation,
        acknowledged: false,
    };
    if !engram_admission_retry_head_matches(record, &retry) || record.runtime_stop_in_progress {
        clear_engram_admission_retry(record);
        return false;
    }
    if record
        .engram
        .abort_retry
        .as_ref()
        .is_some_and(|abort| abort.prompt_id == retry.prompt_id)
    {
        record.engram.abort_retry = None;
    }
    clear_engram_bind_retry(record);
    record.session.preview = engram_admission_retry_preview(&retry);
    record.engram.admission_retry = Some(retry);
    record.engram.abort_retry_acknowledged = false;
    record.engram.abort_retry_saved = false;
    record.engram.abort_retry_fence = None;
    true
}

/// One tick's step for `record`'s parked-admission retry at `now`, given the
/// authority its admission would run under now (`None` when Engram no longer
/// admits it). Mutates the record; the caller persists.
fn engram_admission_retry_step(
    record: &mut SessionRecord,
    authority: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
    budget_now: std::time::Instant,
) -> EngramAbortRetryStep {
    let Some(retry) = record.engram.admission_retry.clone() else {
        return EngramAbortRetryStep::Wait;
    };
    if record.dedicated_cleanup_holds_admission() {
        return EngramAbortRetryStep::Wait;
    }
    // Its own replay, or another admission of the head, is running: whatever
    // that ends in decides (a park schedules the next attempt).
    if engram_retry_attempt_in_flight(record)
        || matches!(
            record.session.status,
            SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
        )
    {
        return EngramAbortRetryStep::Wait;
    }
    if !engram_admission_retry_head_matches(record, &retry)
        || authority != Some(retry.authority.as_str())
        || record.runtime_stop_in_progress
    {
        drop_engram_admission_retry(record);
        return EngramAbortRetryStep::Dropped;
    }
    if let Some(step) = engram_retry_acknowledgement_step(record, |record| {
        if let Some(saved) = record.engram.admission_retry.as_mut() {
            saved.acknowledged = true;
        }
    }) {
        return step;
    }
    let bind_backed_off = record
        .engram
        .next_bind_retry_at
        .is_some_and(|at| at > budget_now);
    if !engram_retry_due_passed(&retry.due_at, now)
        || bind_backed_off
        || !matches!(
            record.session.status,
            SessionStatus::Idle | SessionStatus::Error
        )
        || record.engram.project_reset_in_progress
    {
        return EngramAbortRetryStep::Wait;
    }
    // A restart's readiness fence still raised is no reason to wait: the due
    // attempt starts the session's one lazy boot recovery instead of
    // admitting, and is not charged (`run_engram_retry_attempt_unguarded`).
    EngramAbortRetryStep::Due
}

/// Whether `record`'s acknowledged parked-admission retry still releases its
/// head for the attempt now, under the lock where that attempt is promoted.
fn engram_admission_retry_releases(record: &SessionRecord, authority: Option<&str>) -> bool {
    record.engram.admission_retry.as_ref().is_some_and(|retry| {
        retry.acknowledged
            && !record.dedicated_cleanup_holds_admission()
            && record.engram.abort_retry_acknowledged
            && authority == Some(retry.authority.as_str())
            && !record.runtime_stop_in_progress
            && engram_admission_retry_head_matches(record, retry)
    })
}

/// Rebuilds, on a record just loaded from the store, the parked-admission
/// retry its acknowledgement was saved with, while the head is still exactly
/// that park; the queue stays paused and the tick replays the retained
/// intent when due, from the saved attempt index. A bind retry left on a
/// retained head has no live runtime proof after a restart; once acknowledged
/// it becomes the same record, so its exact retained bind is replayed the
/// same way. Anything else keeps the conservative hold.
fn rebuild_engram_admission_retry_on_load(
    record: &mut SessionRecord,
    bind_retry: Option<&EngramBindRetry>,
) {
    if record.engram.admission_retry.is_none()
        && let Some(bind) = bind_retry.filter(|bind| bind.acknowledged)
    {
        record.engram.admission_retry = Some(EngramAdmissionRetry {
            prompt_id: bind.proof.prompt_id.clone(),
            fingerprint: bind.proof.fingerprint.clone(),
            code: "control_backoff".to_owned(),
            attempts: bind.attempts,
            held_since: bind
                .held_since
                .clone()
                .unwrap_or_else(|| bind.due_at.clone()),
            due_at: bind.due_at.clone(),
            authority: bind.proof.authority.clone(),
            generation: bind.proof.dispatch_generation,
            acknowledged: true,
        });
    }
    let rebuilt = record.engram.admission_retry.as_ref().is_some_and(|retry| {
        retry.acknowledged
            && engram_admission_retry_head_matches(record, retry)
            && record
                .queued_prompts
                .front()
                .is_some_and(|head| head.promotion_disposition_known)
    });
    if rebuilt {
        record.engram.abort_retry_acknowledged = true;
        record.set_auto_dispatch_blocked(true);
        if let Some(retry) = record.engram.admission_retry.clone() {
            record.session.preview = engram_admission_retry_preview(&retry);
        }
    } else {
        record.engram.admission_retry = None;
        // Nothing retries it now, so a saved preview announcing the retry
        // shows the explicit hold it is in instead.
        if record
            .session
            .preview
            .starts_with(ENGRAM_ADMISSION_RETRY_PREVIEW_PREFIX)
        {
            record.session.preview = ENGRAM_ADMISSION_HELD_PREVIEW.to_owned();
        }
    }
}

/// The retry-head rule's answer for one automatic Engram call (a boot
/// reconciliation or a bind made outside the head's own attempt) on a
/// session: proceed as before when it holds no parked-admission retry
/// record; with a record, proceed only holding a retry slot, or defer to the
/// head's own due attempt, which makes the call under its slot.
enum EngramRetryHeadPermit {
    NoRecord,
    Held(EngramRetrySlot),
    Deferred,
}

impl AppState {
    /// `EngramRetryHeadPermit` for `session_id`, read under the caller's
    /// state lock. The slot is a lock-free atomic taken under that lock, so
    /// no lock order is involved; it is released when the permit is dropped,
    /// on every exit of the caller. An opportunistic slot never goes ahead of
    /// due attempts the tick deferred (`try_acquire_opportunistic`).
    fn engram_retry_head_permit_locked(
        &self,
        inner: &StateInner,
        session_id: &str,
    ) -> EngramRetryHeadPermit {
        let holds_record = inner
            .find_session_index(session_id)
            .is_some_and(|index| inner.sessions[index].engram.admission_retry.is_some());
        if !holds_record {
            return EngramRetryHeadPermit::NoRecord;
        }
        match self.engram_retry_slots.try_acquire_opportunistic() {
            Some(slot) => EngramRetryHeadPermit::Held(slot),
            None => EngramRetryHeadPermit::Deferred,
        }
    }

    /// `engram_retry_head_permit_locked`, taking the state lock briefly.
    fn engram_retry_head_permit(&self, session_id: &str) -> EngramRetryHeadPermit {
        let inner = self.inner.lock().expect("state mutex poisoned");
        self.engram_retry_head_permit_locked(&inner, session_id)
    }
}

impl AppState {
    /// The attempt of `owner`'s head failed before admission: count it as an
    /// attempt and move its due time along the schedule, so the tick does not
    /// try it again every pass. An attempt the drain merely declined is not
    /// charged (`run_engram_retry_attempt_unguarded`).
    fn postpone_engram_admission_retry(
        &self,
        session_id: &str,
        owner: &EngramQueuedAdmissionOwner,
        now: chrono::DateTime<chrono::Utc>,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let record = &mut inner.sessions[index];
        // Another admission of the head is running: it decides, and editing
        // the record now would change the content its durability fence checks.
        // A restart's readiness fence held the attempt back and the drain has
        // asked for the lazy boot recovery that lowers it: the attempt waits
        // for that recovery uncharged, and the next tick tries again.
        if !owner.matches(record)
            || engram_retry_attempt_in_flight(record)
            || record.engram_boot_recovery_pending
        {
            return;
        }
        let Some(mut retry) = record
            .engram
            .admission_retry
            .clone()
            .filter(|retry| retry.prompt_id == owner.prompt_id)
        else {
            return;
        };
        if !engram_admission_retry_head_matches(record, &retry) {
            return;
        }
        retry.attempts = retry.attempts.saturating_add(1);
        retry.due_at = (now + engram_abort_retry_delay(session_id, retry.attempts))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        record.session.preview = engram_admission_retry_preview(&retry);
        record.engram.admission_retry = Some(retry);
        sync_pending_prompts(record);
        inner.stamp_session_at_index(index);
        if let Err(error) = self.commit_locked(&mut inner) {
            eprintln!(
                "engram> session={session_id} failed persisting the postponed admission retry: \
                 {error:#}"
            );
        }
        drop(inner);
        self.sync_delegation_attempt_for_child_session(session_id);
    }

    /// The retried admission of a parked head (`engram_admission_retry.rs`):
    /// it bypasses the paused queue only for `owner`, the exact head the
    /// retry was due for, and only while its acknowledged record and
    /// authority still release that head, checked under the promotion lock.
    fn dispatch_next_queued_turn_for_admission_retry(
        &self,
        session_id: &str,
        owner: EngramQueuedAdmissionOwner,
    ) -> Result<Option<TurnDispatch>> {
        self.revalidate_queued_mailbox_wakeups_before_dispatch(session_id);
        Ok(self
            .start_next_queued_turn_off_lock_for_owner(
                session_id,
                true,
                false,
                Some(QueuedDrainOwner::AdmissionRetry(owner)),
            )?
            .map(|started| started.dispatch))
    }
}
