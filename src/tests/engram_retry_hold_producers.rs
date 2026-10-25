//! The real producers of an automatic Engram retry hold, each driven through
//! the production dispatch path from a mailbox wake, as the stalls seen on
//! live lanes were: a prepared begin whose acknowledgement missed its
//! deadline (`begin_preparation_pending`), a failed preparation write settled
//! for a fresh admission (the abort retry), and an admission whose budget a
//! slow evaluate spent before its begin (`dispatch_budget_exhausted`). While
//! the retry is due, the wake and the session say so; when it is due, the
//! head is delivered once with no Resume. Next to them, the holds that are not
//! a due automatic retry: a Stop, an operator's pause, a retained hold with
//! nothing scheduled, a cancelled head, exhaustion and changed authority keep
//! the paused-queue disposition and no retry hold.
//!
//! Owns: those producer and negative cases. Does not own the shared
//! assertions (src/tests/engram_retry_hold_projection.rs), the named-root
//! guard producer (src/tests/engram_root_begin_unsent.rs) or the retry
//! schedule itself (src/tests/engram_admission_retry.rs). New module, a child
//! of src/tests/engram_begin_unknown_recovery.rs, whose fixtures it uses.

use super::*;
use crate::tests::engram_retry_hold_projection::{
    AUTOMATIC_RETRY_DISPOSITION, PAUSED_QUEUE_DISPOSITION, assert_automatic_retry_hold,
    assert_no_hold, assert_plain_pause, wire_queue_holds,
};
use crate::tests::mailbox_acknowledged_wake::send;

/// `state` with a coordination store, so mailbox sends wake its sessions.
fn with_mailbox(state: AppState) -> AppState {
    let path = resolve_coordination_persistence_path(state.persistence_path.as_ref());
    AppState {
        mailbox_store: Arc::new(MailboxStore::open(&path).expect("mailbox test store should open")),
        ..state
    }
}

/// The dispatch budget the slow-evaluate fixture runs under, as in the root
/// causal-failure fixture.
const DISPATCH_BUDGET: Duration = Duration::from_secs(20);

/// The stateful producer, answering every operation in order, except that
/// the second turn_evaluate, a queued wake's admission once the turn before it
/// ended, is answered only after the whole dispatch budget has passed on the
/// scripted clock: the grant arrives with nothing left for its begin, as an
/// Engram slowed by load left it. This is the fault the root causal-failure
/// tests use, under the same budget. (Before the evaluate's reply the same
/// deadline is checked by every step, which then fails first as
/// `deadline_exceeded`.)
struct SlowQueuedEvaluate {
    producer: Arc<StatefulEngramControlTransport>,
    clock: EngramBudgetClock,
    evaluates: std::sync::atomic::AtomicUsize,
}

impl EngramControlTransport for SlowQueuedEvaluate {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        let reply = self.producer.request(connection, request, timeout);
        if matches!(request, EngramControlRequest::TurnEvaluate { .. })
            && self.evaluates.fetch_add(1, Ordering::SeqCst) == 1
        {
            self.clock.advance(DISPATCH_BUDGET + Duration::from_secs(1));
        }
        reply
    }

    fn shutdown_session(&self, session_id: &str) {
        self.producer.shutdown_session(session_id);
    }
}

/// A root session with a mailbox whose first turn runs; a wake sent meanwhile
/// queues behind it, and when the turn ends the wake's admission spends its
/// budget, as on the live lanes: the head is parked for its automatic retry.
/// Returns the state, the session, its runtime (the first turn's prompt
/// already taken) and the sender.
fn budget_exhausted_hold() -> (
    AppState,
    String,
    mpsc::Receiver<CodexRuntimeCommand>,
    String,
) {
    let (state, session, receiver, _) = root_fixture([]);
    let state = with_mailbox(state);
    state.install_control_test_transport(Arc::new(SlowQueuedEvaluate {
        producer: StatefulEngramControlTransport::new(),
        clock: state.engram_budget_clock(),
        evaluates: std::sync::atomic::AtomicUsize::new(0),
    }));
    // Installing a control transport sets the test dispatch budget to the
    // deadlock guard; the slow evaluate must outlast the real-sized budget the
    // root causal-failure fixture uses, so it is set back to that size.
    state.inner.lock().unwrap().test_engram_dispatch_budget = Some(DISPATCH_BUDGET);
    let sender = test_session_id(&state, Agent::Claude);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    assert_eq!(
        abort_retry::prompts_received(&receiver),
        1,
        "the first turn runs"
    );
    let queued = send(&state, &sender, &session, "budget-queued");
    assert_eq!(queued.notification_disposition, "queuedBehindActiveTurn");
    let token = with_record(&state, &session, |record| {
        record
            .runtime
            .runtime_token()
            .expect("the first turn has a runtime")
    });
    state
        .finish_turn_ok_if_runtime_matches(&session, &token)
        .expect("the first turn ends");
    assert!(
        receiver.try_recv().is_err(),
        "the queued wake does not reach the provider yet"
    );
    assert_eq!(
        admission_retry(&state, &session)
            .map(|retry| retry.code)
            .as_deref(),
        Some("dispatch_budget_exhausted"),
        "the spent budget parks the head for its automatic retry"
    );
    (state, session, receiver, sender)
}

/// Criterion 1, `dispatch_budget_exhausted`: while the retry is due, the
/// session names its cause and next due time and a wake sent then is reported
/// as waiting for it; when due, the retained wake is delivered once.
#[test]
fn a_spent_dispatch_budget_is_reported_as_an_automatic_retry_and_delivered_without_resume() {
    let (state, session, receiver, sender) = budget_exhausted_hold();
    assert_automatic_retry_hold(
        &state,
        &session,
        Some("dispatch_budget_exhausted"),
        "spent budget",
    );
    let held = send(&state, &sender, &session, "budget-held");
    assert_eq!(
        held.notification_disposition, AUTOMATIC_RETRY_DISPOSITION,
        "a wake sent while the retry is due names it"
    );

    tick_past_due(&state, &session);
    assert_eq!(
        abort_retry::prompts_received(&receiver),
        1,
        "the retained wake reaches the provider once, without Resume"
    );
    assert_no_hold(&state, &session, "after the retry delivered");
}

/// Criterion 1, `begin_preparation_pending`: a prepared begin whose
/// acknowledgement missed its deadline parks the head for its admission
/// retry; the wakes and the session say so, and when due the same begin is
/// sent once and the head delivered once.
#[test]
fn a_pending_begin_preparation_is_reported_as_an_automatic_retry_and_delivered_without_resume() {
    let (state, session, receiver, transport) = lost_begin_fixture(false);
    let mut state = with_mailbox(state);
    let sender = test_session_id(&state, Agent::Claude);
    let writer = AcknowledgingWriter::install(
        &mut state,
        transport.log.clone(),
        Some(PersistFenceError::Deadline),
    );
    let first = send(&state, &sender, &session, "preparation-first");
    assert!(
        writer.injected(),
        "the prepared-begin fence missed its deadline"
    );
    assert!(
        begin_requests(&transport).is_empty(),
        "the first send is withheld"
    );
    assert!(receiver.try_recv().is_err(), "no provider handoff yet");
    assert_eq!(
        admission_retry(&state, &session)
            .map(|retry| retry.code)
            .as_deref(),
        Some(ENGRAM_BEGIN_PREPARATION_PENDING_CODE)
    );
    assert_eq!(first.notification_disposition, AUTOMATIC_RETRY_DISPOSITION);
    assert_automatic_retry_hold(
        &state,
        &session,
        Some(ENGRAM_BEGIN_PREPARATION_PENDING_CODE),
        "pending preparation",
    );
    let second = send(&state, &sender, &session, "preparation-second");
    assert_eq!(second.notification_disposition, AUTOMATIC_RETRY_DISPOSITION);

    tick_past_any_due(&state, &session);
    tick_past_any_due(&state, &session);
    writer.finish(&state);
    assert_eq!(
        abort_retry::prompts_received(&receiver),
        1,
        "delivered once, without Resume"
    );
    assert_eq!(
        begin_requests(&transport).len(),
        1,
        "exactly one begin is sent"
    );
    assert_no_hold(&state, &session, "after the retry delivered");
}

/// Criterion 1, the abort retry: a failed preparation write withdraws the
/// begin and settles the head for a fresh automatic admission, which is due
/// on its own schedule and reported as that retry.
#[test]
fn a_failed_preparation_write_settled_for_a_fresh_admission_is_reported_as_an_automatic_retry() {
    let (state, session, receiver, transport) = lost_begin_fixture(false);
    let mut state = with_mailbox(state);
    let sender = test_session_id(&state, Agent::Claude);
    let writer = AcknowledgingWriter::install(
        &mut state,
        transport.log.clone(),
        Some(PersistFenceError::WriteFailed(
            "controlled prepared-begin write failure".to_owned(),
        )),
    );
    let first = send(&state, &sender, &session, "write-failure-first");
    assert!(writer.injected());
    assert!(receiver.try_recv().is_err(), "no provider handoff");
    assert_eq!(
        with_record(&state, &session, |record| record.engram.abort_retry.clone())
            .map(|retry| retry.reason),
        Some(EngramAbortReason::BeginPreparation)
    );
    assert_eq!(first.notification_disposition, AUTOMATIC_RETRY_DISPOSITION);
    assert_automatic_retry_hold(&state, &session, None, "fresh admission settled");
    writer.finish(&state);
}

/// Criterion 2, Stop: the user's Stop ends the automatic retry, and the queue
/// is held as a paused queue, exactly as before.
#[test]
fn a_stop_during_an_automatic_retry_keeps_the_paused_queue() {
    let (state, session, _receiver, _sender) = budget_exhausted_hold();
    state
        .request_stop_session(&session)
        .expect("Stop is accepted");
    assert_plain_pause(&state, &session, "stopped");
}

/// Criterion 2, an operator's pause: a retry record still present behind an
/// operator's pause is not a due automatic retry, since nothing passes the
/// pause.
#[test]
fn an_operator_pause_over_a_scheduled_retry_keeps_the_paused_queue() {
    let (state, session, _receiver, _sender) = budget_exhausted_hold();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram.operator_paused = true;
    }
    assert_plain_pause(&state, &session, "operator pause");
}

/// Criterion 2, changed authority: a change of the project's Engram settings
/// (here its store home, part of the authority an admission ran under) resets
/// the session's Engram state, its retry records included, and keeps the
/// retained head as an interrupted hold: a paused queue, not a due retry.
#[test]
fn a_retry_under_changed_authority_keeps_the_paused_queue() {
    let (state, session, _receiver, _sender) = budget_exhausted_hold();
    // A settings update validates its binary, which the fixture's sentinel
    // is not: the project first names the real control fixture, as the bind
    // backoff tests' settings reset does (engram_bind_backoff_retry.rs).
    let (project, mut settings, root) = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let project = inner.sessions[index]
            .session
            .project_id
            .clone()
            .expect("the root session belongs to a project");
        let project_record = inner
            .projects
            .iter_mut()
            .find(|candidate| candidate.id == project)
            .unwrap();
        let root = PathBuf::from(&project_record.root_path);
        fs::write(root.join(".engram-project"), "fixture-ok\n").unwrap();
        let settings = project_record
            .engram
            .as_mut()
            .expect("Engram is enabled for the project");
        settings.binary_path = Some(
            real_engram_control_fixture_path()
                .to_string_lossy()
                .into_owned(),
        );
        (project.clone(), settings.clone(), root)
    };
    let home = root.join("changed-authority-home");
    fs::create_dir_all(&home).unwrap();
    settings.home = Some(home.to_string_lossy().into_owned());
    state
        .update_project_engram_settings(&project, settings)
        .expect("the settings change is accepted");
    assert_plain_pause(&state, &session, "changed authority");
}

/// Criterion 2, Cancel: the cancelled head's retry record lingers until the
/// next tick drops it, but it owns no head, so nothing reports it; the queue
/// is reported exactly as the latch and status say, as before.
#[test]
fn a_cancelled_head_is_not_reported_as_an_automatic_retry() {
    let (state, session, _receiver, _sender) = budget_exhausted_hold();
    let head = with_record(&state, &session, |record| {
        record
            .queued_prompts
            .front()
            .expect("the head is retained")
            .pending_prompt
            .id
            .clone()
    });
    state
        .cancel_queued_prompt(&session, &head)
        .expect("Cancel is accepted");
    let (latched, idle) = with_record(&state, &session, |record| {
        (
            record.orchestrator_auto_dispatch_blocked,
            !matches!(
                record.session.status,
                SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
            ),
        )
    });
    let expected = if latched && idle {
        PAUSED_QUEUE_DISPOSITION
    } else {
        "queuedBehindActiveTurn"
    };
    assert_eq!(state.mailbox_queued_wake_disposition(&session), expected);
    assert_eq!(
        wire_queue_holds(&state, &session),
        (Value::Null, Value::Null)
    );
}

/// Criterion 2, a retained hold with nothing scheduled: a stopped writer is
/// not transient, so the head keeps the explicit Resume/Cancel hold with no
/// retry, reported as a paused queue.
#[test]
fn a_retained_hold_with_nothing_scheduled_keeps_the_paused_queue() {
    let (mut state, session, _receiver, transport) = lost_begin_fixture(false);
    let writer = AcknowledgingWriter::install(
        &mut state,
        transport.log.clone(),
        Some(PersistFenceError::WorkerStopped),
    );
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    assert!(writer.injected());
    writer.finish(&state);
    assert!(
        admission_retry(&state, &session).is_none(),
        "nothing is scheduled"
    );
    assert_plain_pause(&state, &session, "explicit hold");
}

/// Criterion 2, exhaustion: once the admission budget's deadline passes, the
/// head keeps the exhausted Resume/Cancel hold, reported as a paused queue.
#[test]
fn an_exhausted_automatic_retry_keeps_the_paused_queue() {
    let (mut state, session, _receiver, transport) = lost_begin_fixture(false);
    let writer = AcknowledgingWriter::install_with(
        &mut state,
        transport.log.clone(),
        Some(PreparedFence::FailTimes(PersistFenceError::Deadline, 1)),
    );
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    assert!(writer.injected());
    let deadline = with_record(&state, &session, |record| {
        record
            .engram
            .begin_preparation_lineage
            .as_ref()
            .expect("the budget window is recorded")
            .deadline
            .clone()
    });
    let past = chrono::DateTime::parse_from_rfc3339(&deadline)
        .unwrap()
        .with_timezone(&chrono::Utc)
        + chrono::Duration::milliseconds(1);
    state.engram_abort_retry_tick(past);
    writer.finish(&state);
    assert!(
        admission_retry(&state, &session).is_none(),
        "the retry ended"
    );
    assert_plain_pause(&state, &session, "exhausted");
}

/// Criterion 2, a receipted head waiting for an explicit Resume: the paused
/// queue sends it only on a Resume, so its due retry parks and begins nothing
/// (`receipted_head_retry_park`). That is no automatic retry: the queue is
/// reported as paused, as before, both before and after the retry falls due.
#[test]
fn a_receipted_head_parked_for_resume_keeps_the_paused_queue() {
    let (state, session, _receiver, _transport) = parked_after_deadline([]);
    let id = receipted_admission::receipt_the_parked_head(&state, &session);
    abort_retry::await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now());
    assert_plain_pause(&state, &session, "receipted head before its retry is due");
    let lineage = receipted_admission::retry_lineage(&state, &session);
    tick_past_due(&state, &session);
    receipted_admission::assert_parked_for_resume(&state, &session, &id, &lineage);
    assert_plain_pause(&state, &session, "receipted head parked for Resume");
}

/// Criterion 2, a retained source observation met again after a restart: the
/// latch and the hold's preview were saved, the in-memory hold was not, so
/// until the barrier is met the due admission retry is projected as
/// automatic. Meeting it sets the hold, and that alone must count as a change
/// so the caller stamps and publishes the paused queue an explicit Resume
/// lifts.
#[test]
fn a_source_observation_hold_met_again_after_a_restart_counts_as_a_change() {
    let (state, session, _receiver, _sender) = budget_exhausted_hold();
    let (first, restarted_hold, again, held) = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        let first = hold_source_observation_admission(record);
        // A restart keeps the latch and the preview and loses the hold.
        record.engram.source_observation_admission_held = false;
        let restarted_hold = engram_automatic_retry_hold(record).map(|hold| hold.kind);
        let again = hold_source_observation_admission(record);
        (first, restarted_hold, again, record.engram.source_observation_admission_held)
    };
    assert!(first, "the first hold changes the latch and the preview");
    assert_eq!(
        restarted_hold.as_deref(),
        Some("automaticRetry"),
        "before the barrier is met again the due retry is projected"
    );
    assert!(again, "setting only the in-memory hold counts as a change");
    assert!(held);
    assert_plain_pause(&state, &session, "source observation hold after a restart");
}
