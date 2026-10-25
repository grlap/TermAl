// Owns: naming the automatic Engram retry that holds a session's queue head
// while it is due, for the mailbox wake disposition and the session's wire
// projections (`queueHold`), so a queue that moves on by itself is never
// reported as a bare pause that someone must resume. Does not own the latch
// (`set_auto_dispatch_blocked`, state.rs), whose semantics are unchanged; the
// retry records, their schedule or any decision to retry
// (engram_admission_retry.rs, engram_abort_retry.rs, engram_bind_retry.rs,
// engram_retry_schedule.rs); or how the UI words the hold
// (ui/src/panels/queue-pause-cause.ts). New file.

/// The wire kind of a queue hold owned by a due automatic retry.
const SESSION_QUEUE_HOLD_AUTOMATIC_RETRY: &str = "automaticRetry";

/// The disposition of a mailbox wake queued behind such a hold.
const MAILBOX_WAKE_WAITING_FOR_AUTOMATIC_RETRY: &str = "waitingForAutomaticRetry";

/// The cause a bind retry is named by, as a restart names the admission
/// retry it becomes (`rebuild_engram_admission_retry_on_load`).
const ENGRAM_BIND_RETRY_HOLD_CAUSE: &str = "control_backoff";

impl SessionQueueHold {
    fn automatic_retry(cause: &str, retry_at: &str) -> Self {
        Self {
            kind: SESSION_QUEUE_HOLD_AUTOMATIC_RETRY.to_owned(),
            cause: cause.to_owned(),
            retry_at: retry_at.to_owned(),
        }
    }
}

/// The automatic Engram retry that holds `record`'s queue head while it is
/// due, read from the record alone, with the conditions the retry tick itself
/// needs to run it (`engram_retry_schedule.rs`):
/// - the latch is set; nothing an operator did holds it (a Stop or a pause
///   sets `operator_paused`); control is not disabled; no runtime stop is in
///   progress (the step drops the retry);
/// - no dedicated process cleanup holds admission: the admission and bind
///   steps wait at that barrier, and a new turn is refused until a Stop or a
///   Delete finishes the cleanup (`dedicated_cleanup_holds_admission`);
/// - no retained source observation holds admission: the tick stops at that
///   barrier and only an explicit Resume lifts it
///   (`engram_source_observation_holds_admission`). The barrier lives in the
///   host's sightings, so the record carries its marks instead: a Delete's
///   recovery request, the hold `hold_source_observation_admission` set, and
///   an abort retry settled for a retired source observation, the one kept
///   across a restart;
/// - the head is not a receipted head the paused queue sends only on an
///   explicit Resume (`receipted_head_retry_park` says `WaitingForResume`:
///   the tick parks its retry and begins nothing). A Resume's write-ahead in
///   flight continues by itself and stays an automatic retry;
/// - a retry record owns the current head exactly as its next attempt
///   requires: an admission retry by `engram_admission_retry_head_matches`, a
///   bind retry by `engram_bind_retry_head_matches` and the live runtime its
///   proof names (`engram_bind_retry_releases`), an abort retry by its
///   generation and head (`engram_abort_retry_holds_head`);
/// - the record is acknowledged, or its acknowledgement is still being
///   sought in this process (`abort_retry_acknowledged` unset): the tick asks
///   again until it is saved (`engram_retry_acknowledgement_step`), and a
///   restart drops a record saved before its acknowledgement
///   (`persisted_state.rs`, `rebuild_engram_admission_retry_on_load`).
///
/// A Cancel, exhaustion, a project settings change or anything else that
/// leaves no such record, or one for another head or generation, is no
/// automatic retry, and the queue is reported as paused as before. The cause
/// is the retry's code: the admission retry's card code, `control_backoff`
/// for a bind retry, the abort reason for an abort retry; `retryAt` is the
/// record's due time.
fn engram_automatic_retry_hold(record: &SessionRecord) -> Option<SessionQueueHold> {
    if !record.orchestrator_auto_dispatch_blocked
        || record.engram.operator_paused
        || record.engram.disabled_reason.is_some()
        || record.runtime_stop_in_progress
        || record.dedicated_cleanup_holds_admission()
        || record.engram.source_observation_delete_requested
        || record.engram.source_observation_admission_held
    {
        return None;
    }
    let head = record.queued_prompts.front()?;
    if receipted_head_retry_park(record) == Some(ReceiptedRetryPark::WaitingForResume) {
        return None;
    }
    // Acknowledged, or its acknowledgement still sought in this process.
    let acknowledged_or_pending =
        |acknowledged: bool| acknowledged || !record.engram.abort_retry_acknowledged;
    if let Some(retry) = record.engram.bind_retry.as_ref()
        && acknowledged_or_pending(retry.acknowledged)
        && engram_bind_retry_head_matches(record, &retry.proof)
        && record
            .engram
            .bind_retry_runtime
            .as_ref()
            .is_some_and(|runtime| record.runtime.matches_runtime_token(runtime))
    {
        return Some(SessionQueueHold::automatic_retry(
            ENGRAM_BIND_RETRY_HOLD_CAUSE,
            &retry.due_at,
        ));
    }
    if let Some(retry) = record.engram.admission_retry.as_ref()
        && acknowledged_or_pending(retry.acknowledged)
        && engram_admission_retry_head_matches(record, retry)
    {
        return Some(SessionQueueHold::automatic_retry(
            &retry.code,
            &retry.due_at,
        ));
    }
    if let Some(retry) = record.engram.abort_retry.as_ref()
        && retry.reason != EngramAbortReason::SourceObservation
        && acknowledged_or_pending(retry.acknowledged)
        && record.engram.dispatch_generation == retry.generation
        && head.pending_prompt.id == retry.prompt_id
        && !head.engram_waiting
        && !head.has_engram_intent()
    {
        return Some(SessionQueueHold::automatic_retry(
            &engram_abort_reason_cause(retry.reason),
            &retry.due_at,
        ));
    }
    None
}

/// An abort reason as its wire name, the cause of an abort retry's hold.
fn engram_abort_reason_cause(reason: EngramAbortReason) -> String {
    serde_json::to_value(reason)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "abortRetry".to_owned())
}

/// The disposition of a mailbox wake whose dispatch was held before provider
/// hand-off: waiting for the automatic retry that now owns the head, if one
/// does. Otherwise `None`, which the caller records as before.
fn mailbox_wake_disposition_behind_automatic_retry(record: &SessionRecord) -> Option<&'static str> {
    engram_automatic_retry_hold(record).map(|_| MAILBOX_WAKE_WAITING_FOR_AUTOMATIC_RETRY)
}
