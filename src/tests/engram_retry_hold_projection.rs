//! What a queue held by an automatic Engram retry looks like from outside:
//! the mailbox wake disposition and the session's wire projections (the full
//! session and its summary). A head that an admission, begin-preparation,
//! abort or bind retry owns, with a due time, is reported as that retry,
//! `waitingForAutomaticRetry` with its cause and next due time, never as a
//! bare pause. Every other hold (a Stop, an operator's pause, a retained
//! explicit hold with nothing scheduled, a cancelled head, exhaustion, changed
//! authority) keeps `heldBehindPausedQueue` and no retry hold.
//!
//! Owns: the shared assertions the producer tests use, so each producer's
//! fixture module states the same contract. Does not own the producers or
//! their fixtures (src/tests/engram_retry_hold_producers.rs and
//! src/tests/engram_root_begin_unsent.rs) or the latch itself, whose
//! semantics are unchanged. New module.

use super::*;

/// The disposition a mailbox wake queued behind a due automatic retry gets.
pub(crate) const AUTOMATIC_RETRY_DISPOSITION: &str = "waitingForAutomaticRetry";

/// The disposition a wake queued behind any other hold of an idle session
/// gets, as before.
pub(crate) const PAUSED_QUEUE_DISPOSITION: &str = "heldBehindPausedQueue";

/// The `queueHold` field of the session's full wire projection and of its
/// summary, as the API serializes them (`Value::Null` when absent).
pub(crate) fn wire_queue_holds(state: &AppState, session: &str) -> (Value, Value) {
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    let full = serde_json::to_value(AppState::wire_session_from_record(
        &state.server_instance_id,
        record,
    ))
    .unwrap();
    let summary = serde_json::to_value(AppState::wire_session_summary_from_record(
        &state.server_instance_id,
        record,
    ))
    .unwrap();
    (
        full.get("queueHold").cloned().unwrap_or(Value::Null),
        summary.get("queueHold").cloned().unwrap_or(Value::Null),
    )
}

/// The due time of the retry record that owns the head, if one does: the
/// admission retry's, else the abort retry's, else the bind retry's.
fn owning_retry_due_at(state: &AppState, session: &str) -> Option<String> {
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    record
        .engram
        .admission_retry
        .as_ref()
        .map(|retry| retry.due_at.clone())
        .or_else(|| {
            record
                .engram
                .abort_retry
                .as_ref()
                .map(|retry| retry.due_at.clone())
        })
        .or_else(|| {
            record
                .engram
                .bind_retry
                .as_ref()
                .map(|retry| retry.due_at.clone())
        })
}

fn latched(state: &AppState, session: &str) -> bool {
    let inner = state.inner.lock().unwrap();
    inner.sessions[inner.find_session_index(session).unwrap()].orchestrator_auto_dispatch_blocked
}

/// Asserts the session's queue is held by a due automatic retry and says so:
/// the latch is set as before (`queuePaused` stays true), a wake queued now
/// gets the distinct disposition, and both wire projections carry
/// `queueHold { kind: "automaticRetry", cause, retryAt }`, `retryAt` being the
/// owning retry record's due time and `cause` its code, when `cause` is given.
pub(crate) fn assert_automatic_retry_hold(
    state: &AppState,
    session: &str,
    cause: Option<&str>,
    label: &str,
) {
    assert!(
        latched(state, session),
        "{label}: the latch is unchanged: the queue is held"
    );
    let due = owning_retry_due_at(state, session)
        .unwrap_or_else(|| panic!("{label}: an automatic retry owns the head"));
    assert_eq!(
        state.mailbox_queued_wake_disposition(session),
        AUTOMATIC_RETRY_DISPOSITION,
        "{label}: a wake behind a due automatic retry is not reported as a paused queue"
    );
    let (full, summary) = wire_queue_holds(state, session);
    assert_eq!(full, summary, "{label}: the summary projects the same hold");
    assert_eq!(
        full["kind"], "automaticRetry",
        "{label}: the hold names its kind: {full}"
    );
    assert_eq!(
        full["retryAt"],
        due.as_str(),
        "{label}: the hold names its next due time"
    );
    match cause {
        Some(cause) => assert_eq!(full["cause"], cause, "{label}: the hold names its cause"),
        None => assert!(
            full["cause"]
                .as_str()
                .is_some_and(|cause| !cause.is_empty()),
            "{label}: the hold names a cause: {full}"
        ),
    }
}

/// Asserts the session's queue is held by anything but a due automatic
/// retry, exactly as before: the latch is set, a wake queued now gets
/// `heldBehindPausedQueue`, and neither wire projection carries a retry hold.
pub(crate) fn assert_plain_pause(state: &AppState, session: &str, label: &str) {
    assert!(latched(state, session), "{label}: the queue stays paused");
    assert_eq!(
        state.mailbox_queued_wake_disposition(session),
        PAUSED_QUEUE_DISPOSITION,
        "{label}: a wake behind this hold is held behind the paused queue"
    );
    let (full, summary) = wire_queue_holds(state, session);
    assert_eq!(full, Value::Null, "{label}: no automatic retry is claimed");
    assert_eq!(summary, Value::Null, "{label}: nor in the summary");
}

/// Asserts the session's queue is not held at all and no retry hold is
/// projected, as after the retry delivered its head.
pub(crate) fn assert_no_hold(state: &AppState, session: &str, label: &str) {
    assert!(
        !latched(state, session),
        "{label}: the queue is no longer held"
    );
    let (full, summary) = wire_queue_holds(state, session);
    assert_eq!(full, Value::Null, "{label}: no retry hold remains");
    assert_eq!(summary, Value::Null, "{label}: nor in the summary");
}
