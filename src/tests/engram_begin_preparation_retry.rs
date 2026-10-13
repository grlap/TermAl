//! The bounds of the automatic recovery after a prepared begin was not
//! durably acknowledged: the 240 second admission budget, started when the
//! head is accepted, enforced from its first prepared-begin hold whichever
//! retry holds it and kept across a restart; a re-admission that fails again;
//! the holds it never passes (control disabled, a Stop, a superseded owner,
//! an operator's pause); a restart that rebuilds it; a saved hold re-admitted
//! after a restart only when it is dated and inside its budget; a pending
//! prepared begin acknowledged after one attempt's wait; the window a Stop,
//! a pause or control disabled ends but keeps until an accepted Resume opens
//! one new window; a restarted pending begin, read as possibly sent, still
//! bounded by its original deadline; a stale owner's Resume; a receipted
//! head's release by Resume before its hand-off record (prompt_delivery.rs);
//! and the recoveries the budget leaves alone. Does not own the settlement, the late
//! acknowledgement or the fresh admission's order
//! (src/tests/engram_begin_unknown_recovery.rs, whose fixtures it uses). New
//! module.
use super::*;

/// The session parked after a prepared begin's write failed, so it was
/// withdrawn and the head settled for a fresh admission, its abort record
/// acknowledged, with the writer still acknowledging.
fn settled_after_preparation_write_failure() -> (
    AppState,
    String,
    mpsc::Receiver<CodexRuntimeCommand>,
    Arc<LostBeginReply>,
    AcknowledgingWriter,
) {
    settled_after(Some(PreparedFence::Fail(write_failure())))
}

fn write_failure() -> PersistFenceError {
    PersistFenceError::WriteFailed("controlled prepared-begin write failure".to_owned())
}

fn settled_after(
    prepared_fence: Option<PreparedFence>,
) -> (
    AppState,
    String,
    mpsc::Receiver<CodexRuntimeCommand>,
    Arc<LostBeginReply>,
    AcknowledgingWriter,
) {
    let (mut state, session, receiver, transport) = lost_begin_fixture(false);
    let writer =
        AcknowledgingWriter::install_with(&mut state, transport.log.clone(), prepared_fence);
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    assert!(writer.injected());
    abort_retry::await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now());
    let retry = abort_record(&state, &session).expect("the head is settled for a fresh admission");
    assert_eq!(retry.reason, EngramAbortReason::BeginPreparation);
    assert!(retry.acknowledged, "the settlement is acknowledged");
    assert!(
        lineage(&state, &session).is_some(),
        "the first hold is recorded"
    );
    (state, session, receiver, transport, writer)
}

fn abort_record(state: &AppState, session: &str) -> Option<EngramAbortRetry> {
    with_record(state, session, |record| record.engram.abort_retry.clone())
}

fn lineage(state: &AppState, session: &str) -> Option<EngramBeginPreparationLineage> {
    with_record(state, session, |record| {
        record.engram.begin_preparation_lineage.clone()
    })
}

fn rfc3339(at: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(at)
        .unwrap()
        .with_timezone(&chrono::Utc)
}

/// The moment just past the head's saved budget deadline.
fn past_the_window(lineage: &EngramBeginPreparationLineage) -> chrono::DateTime<chrono::Utc> {
    assert_eq!(
        rfc3339(&lineage.deadline),
        rfc3339(&lineage.admitted_at) + chrono::Duration::seconds(ENGRAM_ADMISSION_BUDGET_SECONDS),
        "the deadline is the acceptance plus the budget"
    );
    rfc3339(&lineage.deadline) + chrono::Duration::milliseconds(1)
}

/// Asserts the head's window ended but is kept as it was: the same start,
/// deadline and first hold as `window`, so nothing that follows reads the
/// head as having no budget.
fn assert_ended_window(
    state: &AppState,
    session: &str,
    window: &EngramBeginPreparationLineage,
    label: &str,
) {
    let ended = lineage(state, session).unwrap_or_else(|| panic!("{label}: the window is kept"));
    assert!(ended.ended, "{label}: the window ended");
    assert_eq!(
        (&ended.admitted_at, &ended.deadline, &ended.held_since),
        (&window.admitted_at, &window.deadline, &window.held_since),
        "{label}: the old window's basis is kept"
    );
}

fn exhausted_preview(attempts: u32) -> String {
    exhausted_preview_naming(attempts, "begin_preparation_unacknowledged")
}

/// The exhausted preview whose last cause is `cause`, for a head that keeps
/// no prepared begin.
fn exhausted_preview_naming(attempts: u32, cause: &str) -> String {
    format!(
        "Engram: automatic re-admission exhausted after {attempts} attempts; last cause \
         {cause}. Original prompt retained; resume to retry or cancel."
    )
}

/// The exhausted preview of a head that keeps this host's pending begin.
fn exhausted_pending_preview(attempts: u32) -> String {
    format!(
        "Engram: automatic re-admission exhausted after {attempts} attempts; last cause \
         begin_preparation_pending. The prepared begin was never sent and is kept; resume to \
         send that same begin or cancel."
    )
}

/// Asserts the head keeps the resumable explicit hold with nothing sent and
/// nothing scheduled: retained, waiting, not interrupted, the queue paused,
/// with `preview`.
fn assert_resumable_hold(
    state: &AppState,
    session: &str,
    receiver: &mpsc::Receiver<CodexRuntimeCommand>,
    preview: &str,
    label: &str,
) {
    with_record(state, session, |record| {
        assert!(
            record.engram.abort_retry.is_none(),
            "{label}: no abort record"
        );
        assert!(
            record.engram.admission_retry.is_none(),
            "{label}: no admission retry"
        );
        assert!(record.engram.bind_retry.is_none(), "{label}: no bind retry");
        assert_eq!(
            record.queued_prompts.len(),
            1,
            "{label}: the prompt is retained"
        );
        let head = &record.queued_prompts[0];
        assert!(
            head.engram_waiting && !head.engram_interrupted,
            "{label}: resumable"
        );
        assert!(
            record.orchestrator_auto_dispatch_blocked,
            "{label}: the queue is paused"
        );
        assert_eq!(record.session.preview, preview, "{label}");
    });
    assert_eq!(
        abort_retry::prompts_received(receiver),
        0,
        "{label}: nothing delivered"
    );
}

/// Past its time bound the re-admission is exhausted: the head shows the
/// Resume/Cancel hold with its reason and is never admitted automatically;
/// an explicit Resume still admits it.
#[test]
fn begin_preparation_retry_is_exhausted_after_its_time_bound() {
    let (state, session, receiver, transport, writer) = settled_after_preparation_write_failure();
    let first = lineage(&state, &session).unwrap();
    state.engram_abort_retry_tick(past_the_window(&first));
    let sent = transport.producer.requests().len();
    state.engram_abort_retry_tick(past_the_window(&first) + chrono::Duration::hours(1));
    assert_eq!(transport.producer.requests().len(), sent, "no control call");
    assert_resumable_hold(
        &state,
        &session,
        &receiver,
        &exhausted_preview(first.attempts),
        "exhausted",
    );
    assert_ended_window(&state, &session, &first, "exhausted");

    let _ = state.resume_session_queue(&session);
    writer.finish(&state);
    assert_eq!(
        abort_retry::prompts_received(&receiver),
        1,
        "Resume admits the held head"
    );
}

/// The bound holds across a change of retry kind: the fresh admission's
/// rebind fails in an outage, so another retry takes the head over, and past
/// the window from the FIRST hold the re-admission still ends, with no
/// further control call.
#[test]
fn begin_preparation_retry_is_bounded_across_a_change_of_retry_kind() {
    let (state, session, receiver, transport, writer) = settled_after_preparation_write_failure();
    let first = lineage(&state, &session).unwrap();
    transport.fail_binds.store(usize::MAX, Ordering::SeqCst);
    tick_past_any_due(&state, &session);
    let (kind, held) = with_record(&state, &session, |record| {
        (
            engram_retry_kind(record),
            record.engram.begin_preparation_lineage.clone(),
        )
    });
    assert!(
        matches!(
            kind,
            Some(EngramRetryKind::Admission | EngramRetryKind::Bind)
        ),
        "the failed rebind is held by another retry: {kind:?}"
    );
    let held = held.expect("the first hold stays with the head");
    assert_eq!(
        held.held_since, first.held_since,
        "the window is not restarted"
    );
    assert_eq!(held.attempts, 1, "one automatic attempt");

    state.engram_abort_retry_tick(past_the_window(&first));
    let sent = transport.producer.requests().len();
    state.engram_abort_retry_tick(past_the_window(&first) + chrono::Duration::hours(1));
    writer.finish(&state);
    assert_eq!(
        transport.producer.requests().len(),
        sent,
        "no control call after the bound"
    );
    // C4's 'last cause': the failed rebind's own code, not the class of the
    // first hold.
    let cause = with_record(&state, &session, engram_newest_card_code)
        .expect("the failed rebind's card names its code");
    assert_ne!(cause, "begin_preparation_unacknowledged", "the actual last cause");
    assert_resumable_hold(
        &state,
        &session,
        &receiver,
        &exhausted_preview_naming(1, &cause),
        "another retry kind",
    );
}

/// A re-admission that fails the same way again is settled again: the
/// window keeps the first hold, and the exhausted preview reports the
/// attempts made.
#[test]
fn a_begin_preparation_retry_that_fails_again_keeps_its_first_hold() {
    let (state, session, receiver, _transport, writer) = settled_after(Some(
        PreparedFence::FailTimes(write_failure(), 2),
    ));
    let first = lineage(&state, &session).unwrap();
    tick_past_any_due(&state, &session);
    let retry = abort_record(&state, &session).expect("settled again");
    assert_eq!(retry.reason, EngramAbortReason::BeginPreparation);
    assert_eq!(
        retry.attempts, 2,
        "the abort record counts both withheld begins"
    );
    let again = lineage(&state, &session).unwrap();
    assert_eq!(again.held_since, first.held_since, "the first hold is kept");
    assert_eq!(again.attempts, 1, "one automatic attempt started");

    abort_retry::await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(past_the_window(&first));
    writer.finish(&state);
    assert_resumable_hold(
        &state,
        &session,
        &receiver,
        &exhausted_preview(1),
        "failed again",
    );
}

/// The bound counts from the saved first hold: a restart rebuilds the
/// acknowledged record and the lineage, the rebuilt record is due before the
/// bound, and the tick ends it after the bound.
#[test]
fn begin_preparation_retry_survives_a_restart_with_its_time_bound() {
    let (state, session, _receiver, _transport, writer) = settled_after_preparation_write_failure();
    let saved = with_record(&state, &session, |record| {
        serde_json::to_string(&PersistedSessionRecord::from_record(record)).unwrap()
    });
    let authority = {
        let inner = state.inner.lock().unwrap();
        AppState::engram_binding_target_for_session_shape_locked(&inner, &session, true)
            .unwrap()
            .map(|target| engram_abort_authority(&target))
            .unwrap()
    };
    writer.finish(&state);
    let first = lineage(&state, &session).unwrap();
    let load = || {
        serde_json::from_str::<PersistedSessionRecord>(&saved)
            .unwrap()
            .into_record()
            .unwrap()
    };

    let mut loaded = load();
    assert_eq!(
        loaded.engram.begin_preparation_lineage.as_ref(),
        Some(&first),
        "the first hold is saved"
    );
    let rebuilt = loaded
        .engram
        .abort_retry
        .clone()
        .expect("the acknowledged record is rebuilt");
    let due = rfc3339(&rebuilt.due_at) + chrono::Duration::milliseconds(1);
    assert_eq!(
        engram_begin_preparation_lineage_step(&mut loaded, due),
        None,
        "inside its bound"
    );
    assert_eq!(
        engram_abort_retry_step(&mut loaded, Some(&authority), due),
        EngramAbortRetryStep::Due,
        "due before its bound"
    );

    let mut loaded = load();
    assert_eq!(
        engram_begin_preparation_lineage_step(&mut loaded, past_the_window(&first)),
        Some(EngramAbortRetryStep::Dropped),
        "exhausted after its bound, restart or not"
    );
    assert_eq!(loaded.session.preview, exhausted_preview(first.attempts));
    assert!(loaded.engram.abort_retry.is_none());
}

/// Control disabled, a Stop, a superseded owner and an operator's pause each
/// end the re-admission with nothing sent, each in its own hold: control
/// disabled and an operator's pause in the resumable hold every park shows,
/// a Stop in its stopped hold, and a superseded owner left to the path that
/// moved the head on.
#[test]
fn begin_preparation_retry_never_passes_a_hold() {
    type Hold = fn(&AppState, &str);
    let holds: [(&str, Hold); 4] = [
        ("control disabled", |state, session| {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(session).unwrap();
            inner.sessions[index].engram.disabled_reason = Some("protocol_error".to_owned());
        }),
        ("Stop", |state, session| {
            state
                .request_stop_session(session)
                .expect("Stop of a pending retry succeeds");
        }),
        ("superseded owner", |state, session| {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(session).unwrap();
            let engram = &mut inner.sessions[index].engram;
            engram.dispatch_generation = engram.dispatch_generation.saturating_add(1);
        }),
        ("operator pause", |state, session| {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(session).unwrap();
            inner.sessions[index].engram.operator_paused = true;
        }),
    ];
    for (label, hold) in holds {
        let (state, session, receiver, transport, writer) = settled_after_preparation_write_failure();
        let retry = abort_record(&state, &session).unwrap();
        let window = lineage(&state, &session).unwrap();
        let sent = transport.producer.requests().len();
        hold(&state, &session);
        state.engram_abort_retry_tick(rfc3339(&retry.due_at) + chrono::Duration::milliseconds(1));
        state.engram_abort_retry_tick(rfc3339(&retry.due_at) + chrono::Duration::seconds(10));
        writer.finish(&state);
        assert_eq!(
            transport.producer.requests().len(),
            sent,
            "{label}: no control call"
        );
        // C4: these holds do not reset the clock. The window ends but keeps
        // its basis; a superseded owner leaves it to the path that moved on.
        if label == "superseded owner" {
            assert_eq!(lineage(&state, &session), Some(window.clone()), "{label}: unchanged");
        } else {
            assert_ended_window(&state, &session, &window, label);
        }
        match label {
            "control disabled" | "operator pause" => assert_resumable_hold(
                &state,
                &session,
                &receiver,
                ENGRAM_ADMISSION_HELD_PREVIEW,
                label,
            ),
            _ => {
                with_record(&state, &session, |record| {
                    assert!(
                        record.engram.abort_retry.is_none(),
                        "{label}: no abort record"
                    );
                    assert_eq!(record.queued_prompts.len(), 1, "{label}: retained");
                    assert!(record.queued_prompts[0].engram_interrupted, "{label}: held");
                    if label == "Stop" {
                        assert!(
                            record.session.preview.contains("automatic retry stopped"),
                            "{label}"
                        );
                    }
                });
                assert_eq!(
                    abort_retry::prompts_received(&receiver),
                    0,
                    "{label}: nothing delivered"
                );
            }
        }
    }
}

/// A stopped writer keeps its hold in the host that met it. After a restart,
/// once the new host's settlement is acknowledged, that saved hold is
/// admitted afresh: nothing was sent, and its first hold dates it.
#[test]
fn a_stopped_writers_hold_is_readmitted_after_a_restart() {
    let (mut state, session, receiver, transport) = lost_begin_fixture(false);
    let writer = AcknowledgingWriter::install(
        &mut state,
        transport.log.clone(),
        Some(PersistFenceError::WorkerStopped),
    );
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    assert!(writer.injected());
    assert!(
        abort_record(&state, &session).is_none(),
        "the same host keeps the hold"
    );
    assert!(lineage(&state, &session).is_some(), "the hold is dated");
    assert!(with_record(
        &state,
        &session,
        engram_begin_preparation_boot_hold
    ));

    // The new host's boot pass settles the saved hold, unacknowledged: the
    // head stays held until the writer acknowledges the settlement.
    {
        let mut inner = state.inner.lock().unwrap();
        assert!(state.readmit_engram_begin_preparation_holds_on_boot_locked(&mut inner));
        let index = inner.find_session_index(&session).unwrap();
        let record = &inner.sessions[index];
        let retry = record.engram.abort_retry.as_ref().expect("settled at boot");
        assert_eq!(retry.reason, EngramAbortReason::BeginPreparation);
        assert!(
            !retry.acknowledged,
            "not before the boot persistence is acknowledged"
        );
        assert!(record.queued_prompts[0].engram_interrupted);
        assert!(
            record.queued_prompts[0].engram_evaluate.is_none(),
            "the stale evaluate is dropped"
        );
    }
    state.engram_abort_retry_tick(chrono::Utc::now());
    abort_retry::await_settlement_acknowledgement(&state, &session);
    tick_past_any_due(&state, &session);
    writer.finish(&state);
    assert_eq!(
        abort_retry::prompts_received(&receiver),
        1,
        "admitted afresh once"
    );
    assert_eq!(begin_requests(&transport).len(), 1, "exactly one begin");
}

/// A saved hold that cannot be dated (saved by an earlier build, with no
/// first hold) or whose bound has passed is never re-admitted at boot: it
/// keeps its hold, whatever its age.
#[test]
fn a_saved_hold_undated_or_past_its_bound_keeps_its_hold_at_boot() {
    for label in ["earlier build", "past its bound"] {
        let (mut state, session, receiver, transport) = lost_begin_fixture(false);
        let writer = AcknowledgingWriter::install(
            &mut state,
            transport.log.clone(),
            Some(PersistFenceError::WorkerStopped),
        );
        let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let engram = &mut inner.sessions[index].engram;
            if label == "earlier build" {
                engram.begin_preparation_lineage = None;
            } else {
                // Accepted more than the budget ago: the downtime counted.
                let accepted = chrono::Utc::now()
                    - chrono::Duration::seconds(ENGRAM_ADMISSION_BUDGET_SECONDS + 1);
                let lineage = engram.begin_preparation_lineage.as_mut().unwrap();
                let held = lineage.held_since.clone();
                *lineage = EngramBeginPreparationLineage::accepted(lineage.prompt_id.clone(), accepted);
                lineage.held_since = held;
            }
            let sent = transport.producer.requests().len();
            assert!(
                !state.readmit_engram_begin_preparation_holds_on_boot_locked(&mut inner),
                "{label}: nothing settled"
            );
            let record = &inner.sessions[index];
            assert!(record.engram.abort_retry.is_none(), "{label}");
            assert!(
                record.queued_prompts[0].engram_interrupted,
                "{label}: the hold is kept"
            );
            assert!(
                record.queued_prompts[0].engram_evaluate.is_some(),
                "{label}: untouched"
            );
            assert_eq!(transport.producer.requests().len(), sent, "{label}");
        }
        writer.finish(&state);
        assert_eq!(
            abort_retry::prompts_received(&receiver),
            0,
            "{label}: nothing delivered"
        );
    }
}

/// The Reconcile hold a prepared begin's failure leaves (the field shape:
/// begin_preparation_unacknowledged, dispatch withheld, repair armed, the
/// head interrupted), here from a stopped writer that this host cannot wait
/// out.
fn held_after_stopped_writer() -> (
    AppState,
    String,
    mpsc::Receiver<CodexRuntimeCommand>,
    Arc<LostBeginReply>,
    AcknowledgingWriter,
) {
    let (mut state, session, receiver, transport) = lost_begin_fixture(false);
    let writer = AcknowledgingWriter::install(
        &mut state,
        transport.log.clone(),
        Some(PersistFenceError::WorkerStopped),
    );
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    assert!(writer.injected());
    with_record(&state, &session, |record| {
        assert!(
            engram_begin_preparation_held_head(record),
            "the field shape"
        );
        assert!(record.queued_prompts[0].engram_interrupted);
    });
    (state, session, receiver, transport, writer)
}

/// Greg's live case: a Resume on the held begin is not a silent no-op. It
/// admits the head afresh (the rebind first, then a fresh evaluate) and the
/// provider receives the prompt once; Resume pressed again starts nothing
/// and sends nothing twice.
#[test]
fn resume_on_a_begin_preparation_hold_admits_it_afresh_once() {
    let (state, session, receiver, transport, writer) = held_after_stopped_writer();
    state
        .resume_session_queue(&session)
        .expect("Resume admits the held head");
    state
        .resume_session_queue(&session)
        .expect("a repeated Resume is harmless");
    state.resume_session_queue(&session).expect("and again");
    writer.finish(&state);
    assert_eq!(
        abort_retry::prompts_received(&receiver),
        1,
        "delivered once"
    );
    assert_eq!(begin_requests(&transport).len(), 1, "exactly one begin");
    let operations: Vec<String> = transport
        .producer
        .requests()
        .into_iter()
        .map(|recorded| {
            recorded.request["operation"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    let evaluates: Vec<usize> = operations
        .iter()
        .enumerate()
        .filter(|(_, operation)| *operation == "turn_evaluate")
        .map(|(position, _)| position)
        .collect();
    assert_eq!(
        evaluates.len(),
        2,
        "the withheld evaluate and Resume's fresh one: {operations:?}"
    );
    assert!(
        operations[evaluates[0]..evaluates[1]]
            .iter()
            .any(|operation| operation == "session_bind"),
        "the rebind precedes the fresh evaluate: {operations:?}"
    );
    with_record(&state, &session, |record| {
        assert!(record.queued_prompts.is_empty())
    });
}

/// When a Resume cannot release the held begin it says why, and the hold is
/// unchanged: control disabled, or an automatic re-admission whose
/// settlement is not yet saved.
#[test]
fn resume_on_a_begin_preparation_hold_reports_why_it_cannot() {
    let (state, session, receiver, transport, writer) = held_after_stopped_writer();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram.disabled_reason = Some("protocol_error".to_owned());
    }
    let sent = transport.producer.requests().len();
    let Err(error) = state.resume_session_queue(&session) else {
        panic!("control disabled: Resume must be refused");
    };
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error.message.contains("control is disabled"),
        "{}",
        error.message
    );
    with_record(&state, &session, |record| {
        assert!(
            record.queued_prompts[0].engram_interrupted,
            "the hold is unchanged"
        );
        assert!(record.queued_prompts[0].engram_evaluate.is_some());
    });
    assert_eq!(transport.producer.requests().len(), sent, "no control call");
    writer.finish(&state);
    assert_eq!(abort_retry::prompts_received(&receiver), 0);

    // Settled for its automatic re-admission, not yet acknowledged.
    let (mut state, session, receiver, transport) = lost_begin_fixture(false);
    let writer = AcknowledgingWriter::install(&mut state, transport.log.clone(), Some(write_failure()));
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    assert!(with_record(&state, &session, |record| !record
        .engram
        .abort_retry_acknowledged));
    let Err(error) = state.resume_session_queue(&session) else {
        panic!("settlement not yet saved: Resume must be refused");
    };
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(
        error.message.contains("waiting for its settlement"),
        "{}",
        error.message
    );
    writer.finish(&state);
    assert_eq!(abort_retry::prompts_received(&receiver), 0);
}

/// The withheld begin's card carries the latency split: the retained
/// evaluate's and the prepared begin's durability waits beside the total.
#[test]
fn a_withheld_begin_card_carries_the_latency_split() {
    let (state, session, _receiver, _transport, writer) = settled_after_preparation_write_failure();
    writer.finish(&state);
    let latency = with_record(&state, &session, |record| {
        record
            .session
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::EngramControl { card, .. }
                    if card.refusal_code.as_deref() == Some(ENGRAM_BEGIN_PREPARATION_CODE) =>
                {
                    Some(card.latency_ms.clone())
                }
                _ => None,
            })
    })
    .expect("the withheld begin's card");
    assert!(latency.evaluate.is_some(), "{latency:?}");
    assert!(
        latency.admission_fence.is_some(),
        "the retained evaluate's fence: {latency:?}"
    );
    assert!(
        latency.preparation_fence.is_some(),
        "the prepared begin's fence: {latency:?}"
    );
    let json = serde_json::to_value(&latency).unwrap();
    assert!(
        json.get("preparationFence").is_some(),
        "on the wire: {json}"
    );
}

/// C3 (fail-first): a prepared-begin acknowledgement that arrives after one
/// attempt's 20 second wait, but well inside the overall admission budget, is
/// a durable success. The exact prepared begin (same key, grant and delivery
/// tokens) is kept and sent; no fresh evaluate races the pending exact
/// preparation, and the prompt is delivered once.
#[test]
fn a_prepared_begin_acknowledged_after_its_attempt_wait_is_kept_and_sent() {
    let (mut state, session, receiver, transport) = lost_begin_fixture(false);
    let clock = state.engram_budget_clock();
    let writer = AcknowledgingWriter::install_with(
        &mut state,
        transport.log.clone(),
        Some(PreparedFence::AcknowledgeAfterAttempt(clock)),
    );
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    assert!(writer.injected(), "the prepared begin was acknowledged after its attempt wait");
    for _ in 0..3 {
        tick_past_any_due(&state, &session);
    }
    writer.finish(&state);

    let prepared = transport
        .log
        .snapshot()
        .iter()
        .find_map(|boundary| match boundary {
            Boundary::Acknowledged(content) => prepared_begin(content).cloned(),
            Boundary::Sent(_) => None,
        })
        .expect("the prepared begin was acknowledged");
    // A replay of the retained evaluate keeps its key; a fresh evaluate
    // would bring a new one.
    let evaluate_keys: std::collections::BTreeSet<String> = transport
        .producer
        .requests()
        .iter()
        .filter(|recorded| recorded.request["operation"] == "turn_evaluate")
        .map(|recorded| recorded.request["idempotency_key"].to_string())
        .collect();
    assert_eq!(
        evaluate_keys.len(),
        1,
        "no fresh evaluate races the pending exact preparation: {evaluate_keys:?}"
    );
    let begins = begin_requests(&transport);
    assert_eq!(begins.len(), 1, "exactly one begin: {begins:?}");
    assert_eq!(begins[0]["idempotency_key"], prepared["idempotency_key"], "the exact prepared begin");
    assert_eq!(begins[0]["grant_id"], prepared["grant_id"]);
    assert_eq!(begins[0]["delivery_tokens"], prepared["delivery_tokens"]);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered once");
}

/// C4 (fail-first): the automatic recovery of one accepted admission is
/// bounded at 240 seconds from that acceptance. Past it the head shows the
/// Resume/Cancel hold and no automatic send starts, though an attempt is due.
#[test]
fn begin_preparation_recovery_is_exhausted_240_seconds_after_acceptance() {
    let (state, session, receiver, transport, writer) = settled_after_preparation_write_failure();
    let sent = transport.producer.requests().len();
    // From the saved acceptance itself, so fixture setup time cannot move it.
    let budget = lineage(&state, &session).expect("the budget started at acceptance");
    let accepted = rfc3339(&budget.admitted_at);
    assert_eq!(
        rfc3339(&budget.deadline),
        accepted + chrono::Duration::seconds(240),
        "the budget is 240 seconds from acceptance"
    );
    let past = accepted + chrono::Duration::seconds(240) + chrono::Duration::seconds(1);
    state.engram_abort_retry_tick(past);
    state.engram_abort_retry_tick(past + chrono::Duration::seconds(30));
    writer.finish(&state);
    assert_eq!(transport.producer.requests().len(), sent, "no automatic send after the budget");
    assert_eq!(abort_retry::prompts_received(&receiver), 0, "nothing delivered");
    with_record(&state, &session, |record| {
        assert!(
            record.session.preview.starts_with("Engram: automatic re-admission exhausted after"),
            "{}",
            record.session.preview
        );
        let head = &record.queued_prompts[0];
        assert!(head.engram_waiting && !head.engram_interrupted, "the resumable hold");
    });
}

/// The operations the producer received, in order.
fn operations(transport: &LostBeginReply) -> Vec<(String, Value)> {
    transport
        .producer
        .requests()
        .into_iter()
        .map(|recorded| {
            (
                recorded.request["operation"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                recorded.request,
            )
        })
        .collect()
}

/// A prepared begin whose wait ran out is kept as pending, so the head parks
/// for its admission retry with no fresh admission armed. When that retry is
/// due only after the budget's deadline, no automatic send starts, and the
/// head shows the exhausted Resume/Cancel hold. The pending begin is kept as
/// it is (its write may still be unresolved, so nothing proves it absent):
/// its marker, grant, key and delivery tokens stay, with no rebind armed to
/// expire its grant. Resume then sends that same begin, with no fresh
/// evaluate or grant, and the prompt is delivered once.
#[test]
fn a_pending_prepared_begin_at_the_budget_deadline_is_kept_for_resume() {
    let (mut state, session, receiver, transport) = lost_begin_fixture(false);
    let writer = AcknowledgingWriter::install_with(
        &mut state,
        transport.log.clone(),
        Some(PreparedFence::FailTimes(PersistFenceError::Deadline, 1)),
    );
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    assert!(writer.injected());
    let (withheld_grant, _) = transport.producer.grant_state(&session);
    let withheld_grant = withheld_grant.expect("the withheld begin's grant was issued");
    let first = with_record(&state, &session, |record| {
        let retry = record.engram.admission_retry.clone().expect("the pending begin's retry");
        assert_eq!(retry.code, ENGRAM_BEGIN_PREPARATION_PENDING_CODE);
        assert!(record.engram.abort_retry.is_none(), "no fresh admission is settled");
        assert!(!record.engram.rebind_required, "no orphan repair is armed");
        let prepared = engram_begin_preparation_pending(record).expect("the begin is pending");
        assert_eq!(prepared.grant_id, withheld_grant);
        assert!(
            record.session.preview.contains("awaits its durable acknowledgement")
                && record.session.preview.contains("Automatic recovery ends at"),
            "waiting, next retry and deadline are named: {}",
            record.session.preview
        );
        record.engram.begin_preparation_lineage.clone().unwrap()
    });
    let pending = with_record(&state, &session, |record| {
        engram_begin_preparation_pending(record).cloned().unwrap()
    });
    assert!(first.enforced());
    assert!(begin_requests(&transport).is_empty(), "nothing sent");

    let sent = transport.producer.requests().len();
    state.engram_abort_retry_tick(past_the_window(&first));
    assert_eq!(transport.producer.requests().len(), sent, "no automatic send at the deadline");
    assert_resumable_hold(
        &state,
        &session,
        &receiver,
        &exhausted_pending_preview(first.attempts),
        "pending at the deadline",
    );
    with_record(&state, &session, |record| {
        let kept = engram_begin_preparation_pending(record)
            .expect("the pending begin is kept, never withdrawn");
        assert_eq!(kept.idempotency_key, pending.idempotency_key, "the same key");
        assert_eq!(kept.grant_id, withheld_grant, "the same grant");
        assert_eq!(kept.delivery_tokens, pending.delivery_tokens, "the same delivery tokens");
        assert!(!record.engram.rebind_required, "no rebind is armed to expire its grant");
    });
    assert_ended_window(&state, &session, &first, "pending at the deadline");

    state.resume_session_queue(&session).expect("Resume sends the kept begin");
    writer.finish(&state);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered once");
    let evaluates = operations(&transport)
        .iter()
        .filter(|(operation, _)| operation == "turn_evaluate")
        .count();
    assert_eq!(evaluates, 1, "no fresh evaluate after the deadline");
    let begins = begin_requests(&transport);
    assert_eq!(begins.len(), 1, "exactly one begin: {begins:?}");
    assert_eq!(begins[0]["idempotency_key"], pending.idempotency_key.as_str(), "the same begin");
    assert_eq!(begins[0]["grant_id"], withheld_grant.as_str(), "its grant was kept");
}

/// The budget is enforced only on the prepared-begin recovery: a head held
/// by another recovery (here a bind outage, with no prepared begin withheld)
/// keeps its budget unenforced and is still retried and delivered past 240
/// seconds after its acceptance.
#[test]
fn the_admission_budget_leaves_other_recoveries_unchanged() {
    let (state, session, receiver, transport) = lost_begin_fixture(false);
    let accepted = chrono::Utc::now();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram.rebind_required = true;
    }
    transport.fail_binds.store(1, Ordering::SeqCst);
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    let (kind, budget) = with_record(&state, &session, |record| {
        (engram_retry_kind(record), record.engram.begin_preparation_lineage.clone())
    });
    assert!(kind.is_some(), "the bind outage is held by a retry");
    let budget = budget.expect("the budget started at acceptance");
    assert!(!budget.enforced(), "no prepared begin was withheld");
    assert!(rfc3339(&budget.admitted_at) >= accepted - chrono::Duration::seconds(1));

    let late = accepted + chrono::Duration::seconds(ENGRAM_ADMISSION_BUDGET_SECONDS + 60);
    for step in 0..3 {
        let due = with_record(&state, &session, |record| {
            record
                .engram
                .admission_retry
                .as_ref()
                .map(|retry| retry.due_at.clone())
                .or_else(|| record.engram.bind_retry.as_ref().map(|retry| retry.due_at.clone()))
        });
        let at = due.map_or(late, |due| (rfc3339(&due) + chrono::Duration::milliseconds(1)).max(late));
        tick_at(&state, &session, at + chrono::Duration::seconds(step));
    }
    assert_eq!(
        abort_retry::prompts_received(&receiver),
        1,
        "retried and delivered past the budget: {:?}",
        operations(&transport).iter().map(|(operation, _)| operation.clone()).collect::<Vec<_>>()
    );
}

/// A Resume made for an owner, as a delegation's Resume is, on the held
/// begin is not a silent no-op either: the release moves the head to a new
/// dispatch generation, and the Resume dispatches for that same head's
/// re-captured owner, so the prompt is delivered once.
#[test]
fn a_resume_for_an_owner_on_a_begin_preparation_hold_admits_it() {
    let (state, session, receiver, transport, writer) = held_after_stopped_writer();
    let owner = with_record(&state, &session, |record| {
        EngramQueuedAdmissionOwner::capture(record).expect("the held head's owner")
    });
    let outcome = state
        .resume_local_session_queue(&session, Some(owner))
        .expect("Resume admits the held head");
    assert!(
        !matches!(outcome, Some(TurnDispatchDeliveryOutcome::Superseded) | None),
        "the owner's Resume dispatches: {outcome:?}"
    );
    writer.finish(&state);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered once");
    assert_eq!(begin_requests(&transport).len(), 1, "exactly one begin");
}

/// Each attempt is clipped to what is left of an enforced budget, and only
/// an applied begin is spared: the steps completing its turn are neither
/// clipped nor ended by it. A begin possibly sent is not applied: its exact
/// replay is clipped and ends at the deadline, its grant kept.
#[test]
fn the_admission_budget_clips_attempts_and_spares_only_an_applied_begin() {
    let (state, session, _receiver, _transport, writer) = settled_after_preparation_write_failure();
    let left = chrono::Duration::seconds(5);
    // One captured wall-clock instant, so the expectation does not depend on
    // how much real time passes between the statements.
    let wall = rfc3339(&chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let lineage = inner.sessions[index].engram.begin_preparation_lineage.as_mut().unwrap();
        lineage.deadline = (wall + left).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    }
    let clock = state.engram_budget_clock();
    {
        let inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let now = clock.now();
        let budget = engram_admission_budget_deadline_at(&inner.sessions[index], &clock, wall)
            .expect("the budget is enforced");
        assert_eq!(budget, now + Duration::from_secs(5), "exactly what is left");
        let target = AppState::engram_binding_target_for_session_shape_locked(&inner, &session, true)
            .unwrap()
            .expect("the target");
        assert!(
            target.dispatch_deadline(now) <= now + Duration::from_secs(5),
            "an attempt never outlives the budget"
        );
        assert!(
            target.admission_started_at.is_none()
                && target.root_operation_deadline(now) <= now + Duration::from_secs(5),
            "nor does a standalone operation, a boot reconciliation's bind"
        );
        // This target was captured before any begin, with the budget
        // enforced. The steps completing a begin it then applies use the
        // per-attempt bound alone, past what was left of the budget.
        assert!(
            target.applied_begin_deadline(now) > now + Duration::from_secs(5),
            "a begin applied through a pre-begin target is not clipped after it"
        );
    }
    assert!(
        state.engram_session_holds_admission_retry(&session),
        "the settled head's boot reconciliation runs under the shared cap"
    );

    let past = chrono::Utc::now() + left + chrono::Duration::seconds(1);
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&session).unwrap();
    let record = &mut inner.sessions[index];
    // An applied begin was the last automatic send: the steps completing its
    // turn are neither clipped nor ended by the budget.
    record.engram.active_grant_id = Some("grant-begun".to_owned());
    assert_eq!(
        engram_admission_budget_deadline(record, &clock),
        None,
        "an applied begin's turn is not clipped"
    );
    assert_eq!(
        engram_begin_preparation_lineage_step(record, past),
        None,
        "nor ended by the budget"
    );
    record.engram.active_grant_id = None;

    // A begin possibly sent is not an applied one: its exact replay stays
    // inside what is left of the budget and ends at the deadline.
    record.engram.uncertain_grant_id = Some("grant-possibly-begun".to_owned());
    assert!(
        engram_admission_budget_deadline(record, &clock)
            .is_some_and(|budget| budget <= clock.now() + Duration::from_secs(5)),
        "a possibly sent begin's replay is clipped"
    );
    assert_eq!(
        engram_begin_preparation_lineage_step(record, past),
        Some(EngramAbortRetryStep::Dropped),
        "and ended by the budget"
    );
    assert_eq!(
        record.engram.uncertain_grant_id.as_deref(),
        Some("grant-possibly-begun"),
        "the possibly sent grant is kept for its exact replay"
    );
    assert!(record.engram.begin_preparation_lineage.as_ref().is_some_and(|lineage| lineage.ended));
    drop(inner);
    writer.finish(&state);
}

/// Under the production dispatch budget: a pending begin's re-acknowledgement
/// that arrives late, at its clipped deadline, leaves the begin no time to be
/// transmitted. Never sent, it stays the budget's pending begin (not a
/// possibly begun one whose exact replay the budget could not end), and past
/// the budget no automatic send starts.
#[test]
fn a_late_acknowledged_pending_begin_with_no_send_budget_stays_pending_within_the_budget() {
    let (mut state, session, receiver, transport) = lost_begin_fixture(false);
    // The production 20 second attempt budget, not the fixture's deadlock
    // guard: the late acknowledgement then spends the whole attempt.
    state.inner.lock().unwrap().test_engram_dispatch_budget = None;
    let clock = state.engram_budget_clock();
    let writer = AcknowledgingWriter::install_with(
        &mut state,
        transport.log.clone(),
        Some(PreparedFence::DeadlineThenAcknowledgeLate(clock)),
    );
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    let first = with_record(&state, &session, |record| {
        assert_eq!(
            record.engram.admission_retry.as_ref().map(|retry| retry.code.clone()),
            Some(ENGRAM_BEGIN_PREPARATION_PENDING_CODE.to_owned()),
            "the missed wait keeps the begin pending"
        );
        record.engram.begin_preparation_lineage.clone().unwrap()
    });
    // Only until the writer scripted the late re-acknowledgement (its second
    // scripted fence): a later attempt would be acknowledged in time and
    // send the begin.
    for _ in 0..3 {
        if writer.injections() == 2 {
            break;
        }
        tick_past_any_due(&state, &session);
    }
    assert_eq!(writer.injections(), 2, "the pending begin was acknowledged late");
    assert!(begin_requests(&transport).is_empty(), "no begin was transmitted");
    with_record(&state, &session, |record| {
        assert_eq!(record.engram.uncertain_grant_id, None, "never sent, so never possibly begun");
        assert!(
            engram_begin_preparation_pending(record).is_some(),
            "the exact begin stays pending"
        );
        assert!(engram_admission_budget_enforced(record).is_some(), "inside its budget");
    });

    state.engram_abort_retry_tick(past_the_window(&first));
    state.engram_abort_retry_tick(past_the_window(&first) + chrono::Duration::seconds(60));
    writer.finish(&state);
    assert!(begin_requests(&transport).is_empty(), "no automatic send after the budget");
    assert_eq!(abort_retry::prompts_received(&receiver), 0, "nothing delivered");
    with_record(&state, &session, |record| {
        assert!(
            record.session.preview.starts_with("Engram: automatic re-admission exhausted after"),
            "{}",
            record.session.preview
        );
    });
}

/// Under the production dispatch budget, the FIRST preparation of a begin
/// acknowledged late, at its wait's deadline, leaves the begin no time to be
/// transmitted. Proven unsent, it is the budget's pending begin from then on,
/// never a possibly begun one: its hold enforces the budget counted from
/// acceptance, and past it no automatic send starts.
#[test]
fn a_first_late_acknowledged_preparation_with_no_send_budget_is_pending_under_the_budget() {
    let (mut state, session, receiver, transport) = lost_begin_fixture(false);
    // The production 20 second attempt budget, not the fixture's deadlock
    // guard: the late acknowledgement then spends the whole attempt.
    state.inner.lock().unwrap().test_engram_dispatch_budget = None;
    let clock = state.engram_budget_clock();
    let writer = AcknowledgingWriter::install_with(
        &mut state,
        transport.log.clone(),
        Some(PreparedFence::AcknowledgeLate(clock)),
    );
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    assert_eq!(writer.injections(), 1, "the first preparation was acknowledged late");
    assert!(begin_requests(&transport).is_empty(), "no begin was transmitted");
    let budget = with_record(&state, &session, |record| {
        assert_eq!(record.engram.uncertain_grant_id, None, "never sent, so never possibly begun");
        assert!(!record.engram.rebind_required, "no rebind would expire its grant");
        assert_eq!(record.engram.next_bind_retry_at, None, "no transport failure counted");
        assert!(engram_begin_preparation_pending(record).is_some(), "the exact begin stays pending");
        assert_eq!(
            record.engram.admission_retry.as_ref().map(|retry| retry.code.clone()),
            Some(ENGRAM_BEGIN_PREPARATION_PENDING_CODE.to_owned()),
            "its own admission retry, not a begin-unknown replay"
        );
        engram_admission_budget_enforced(record)
            .cloned()
            .expect("the hold enforces the budget from acceptance")
    });

    state.engram_abort_retry_tick(past_the_window(&budget));
    state.engram_abort_retry_tick(past_the_window(&budget) + chrono::Duration::seconds(60));
    writer.finish(&state);
    assert!(begin_requests(&transport).is_empty(), "no automatic send after the budget");
    assert_eq!(abort_retry::prompts_received(&receiver), 0, "nothing delivered");
    with_record(&state, &session, |record| {
        assert!(
            record.session.preview.starts_with("Engram: automatic re-admission exhausted after"),
            "{}",
            record.session.preview
        );
        assert_eq!(record.engram.uncertain_grant_id, None);
        assert!(
            engram_begin_preparation_pending(record).is_some(),
            "the exact begin is kept past the deadline, never withdrawn"
        );
    });
}

/// A Stop ends the automatic recovery of a never-sent begin; the operator's
/// Resume then admits the head, never a silent no-op. This holds after a
/// failed write's settlement (no retained evaluate left) and for a begin kept
/// pending after a missed wait. The prompt reaches the provider once, with
/// one begin.
#[test]
fn stop_then_resume_admits_a_never_sent_begin_preparation_head() {
    for label in ["failed write", "pending begin"] {
        let (state, session, receiver, transport, writer) = if label == "failed write" {
            settled_after_preparation_write_failure()
        } else {
            let (mut state, session, receiver, transport) = lost_begin_fixture(false);
            let writer = AcknowledgingWriter::install_with(
                &mut state,
                transport.log.clone(),
                Some(PreparedFence::FailTimes(PersistFenceError::Deadline, 1)),
            );
            let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
            with_record(&state, &session, |record| {
                assert!(engram_begin_preparation_pending(record).is_some(), "{label}: pending");
            });
            (state, session, receiver, transport, writer)
        };
        state
            .request_stop_session(&session)
            .unwrap_or_else(|_| panic!("{label}: Stop of the automatic recovery succeeds"));
        with_record(&state, &session, |record| {
            assert!(record.engram.abort_retry.is_none(), "{label}: no abort retry after Stop");
            assert!(record.engram.admission_retry.is_none(), "{label}: no admission retry after Stop");
            assert_eq!(record.queued_prompts.len(), 1, "{label}: retained");
        });
        assert!(begin_requests(&transport).is_empty(), "{label}: nothing sent before Resume");

        state
            .resume_session_queue(&session)
            .unwrap_or_else(|_| panic!("{label}: Resume admits the stopped head"));
        writer.finish(&state);
        assert_eq!(abort_retry::prompts_received(&receiver), 1, "{label}: delivered once");
        assert_eq!(begin_requests(&transport).len(), 1, "{label}: exactly one begin");
        with_record(&state, &session, |record| {
            assert!(record.queued_prompts.is_empty(), "{label}: the head was delivered");
        });
    }
}

/// A prepared begin saved before a restart may have been sent: loading drops
/// its pending marker, and the boot pass leaves a paused head as it is. When
/// the operator's Resume then admits that head and its preparation, which
/// rederives the same key and so replays the saved record, meets a failed
/// write, the record is kept, never withdrawn, and its grant is never armed
/// for the orphan rebind. A later Resume says why it stays held.
#[test]
fn a_replayed_possibly_sent_prepared_begin_is_never_withdrawn_on_a_failed_write() {
    let (mut state, session, receiver, transport) = lost_begin_fixture(false);
    let writer = AcknowledgingWriter::install_with(
        &mut state,
        transport.log.clone(),
        Some(PreparedFence::FailSequence(vec![
            // The first preparation misses its wait: pending.
            FenceStep::Fail(PersistFenceError::Deadline),
            // After Resume, the retained evaluate's fence (carrying the
            // saved prepared begin) is acknowledged...
            FenceStep::Acknowledge,
            // ...and the replayed preparation's write fails.
            FenceStep::Fail(write_failure()),
        ])),
    );
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    let saved = with_record(&state, &session, |record| {
        engram_begin_preparation_pending(record)
            .cloned()
            .expect("the first preparation is pending")
    });
    // What a restart leaves: the loaded pending marker dropped (as
    // `PersistedSessionRecord::into_record` does), the paused head skipped
    // by the boot pass, nothing scheduled.
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        record.engram.begin_preparation_lineage.as_mut().unwrap().pending_grant_id = None;
        clear_engram_admission_retry(record);
        record.engram.operator_paused = true;
        record.set_auto_dispatch_blocked(true);
    }

    let _ = state.resume_session_queue(&session);
    assert_eq!(writer.injections(), 3, "the replayed preparation met the failed write");
    assert!(begin_requests(&transport).is_empty(), "nothing was sent");
    with_record(&state, &session, |record| {
        let kept = record.queued_prompts[0]
            .engram_evaluate
            .as_ref()
            .and_then(|evaluate| evaluate.prepared_begin.clone())
            .expect("the possibly sent prepared begin is never withdrawn");
        assert_eq!(kept.idempotency_key, saved.idempotency_key, "the same saved begin");
        assert_eq!(kept.grant_id, saved.grant_id);
        assert!(!record.engram.rebind_required, "its grant is not armed for the orphan rebind");
        assert!(record.engram.abort_retry.is_none(), "no fresh admission is settled");
    });

    let Err(error) = state.resume_session_queue(&session) else {
        panic!("a possibly sent prepared begin must not be released silently");
    };
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("may have been sent"), "{}", error.message);
    writer.finish(&state);
    assert_eq!(abort_retry::prompts_received(&receiver), 0, "nothing delivered");
}

/// A pending begin made by a late acknowledgement whose begin then failed
/// before transmission carries that transport failure as its card's cause.
/// A Stop that interrupts it and a Resume still release it: Resume is never
/// a silent no-op for this host's pending begin, and the prompt reaches the
/// provider once.
#[test]
fn stop_then_resume_admits_a_late_acknowledged_pending_begin() {
    let (mut state, session, receiver, transport) = lost_begin_fixture(false);
    state.inner.lock().unwrap().test_engram_dispatch_budget = None;
    let clock = state.engram_budget_clock();
    let writer = AcknowledgingWriter::install_with(
        &mut state,
        transport.log.clone(),
        Some(PreparedFence::AcknowledgeLate(clock)),
    );
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    with_record(&state, &session, |record| {
        assert!(engram_begin_preparation_pending(record).is_some(), "pending");
        assert!(
            !engram_begin_preparation_newest_cause(record),
            "its card names the transport failure, not the withheld preparation"
        );
        // Nothing reached Engram: no transport failure is counted, so no
        // bind backoff, and no rebind would expire the pending begin's grant.
        assert!(!record.engram.rebind_required, "no rebind armed");
        assert_eq!(record.engram.next_bind_retry_at, None, "no bind backoff");
        assert_eq!(record.engram.consecutive_transport_failures, 0, "no circuit count");
    });
    assert!(
        state.stop_waiting_engram_admission(&session).unwrap_or(false),
        "the Stop interrupts the waiting head"
    );
    let prepared = with_record(&state, &session, |record| {
        assert!(record.queued_prompts[0].engram_interrupted, "interrupted by the Stop");
        assert_eq!(
            engram_begin_preparation_resume(record),
            EngramBeginPreparationResume::ReleasablePending
        );
        engram_begin_preparation_pending(record).cloned().unwrap()
    });
    state
        .resume_session_queue(&session)
        .unwrap_or_else(|_| panic!("Resume admits the stopped head"));
    writer.finish(&state);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered once");
    let begins = begin_requests(&transport);
    assert_eq!(begins.len(), 1, "exactly one begin");
    // C11(a): an acknowledged prepared begin uses its exact replay.
    assert_eq!(begins[0]["idempotency_key"], prepared.idempotency_key.as_str(), "the exact begin");
    assert_eq!(begins[0]["grant_id"], prepared.grant_id.as_str(), "its grant was kept");
    assert_eq!(begins[0]["delivery_tokens"], json!(prepared.delivery_tokens));
}

/// A pending begin acknowledged late whose begin then failed before it was
/// sent carries that transport failure as its card's cause. Stopped, then
/// read back by a restart (which drops the pending marker), it is a prepared
/// begin that may have been sent: Resume is never a silent no-op for it, but
/// says why it stays held.
#[test]
fn resume_of_a_restarted_interrupted_prepared_begin_says_it_may_have_been_sent() {
    let (mut state, session, receiver, transport) = lost_begin_fixture(false);
    state.inner.lock().unwrap().test_engram_dispatch_budget = None;
    let clock = state.engram_budget_clock();
    let writer = AcknowledgingWriter::install_with(
        &mut state,
        transport.log.clone(),
        Some(PreparedFence::AcknowledgeLate(clock)),
    );
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    assert!(state.stop_waiting_engram_admission(&session).unwrap_or(false));
    // What a restart leaves: the loaded pending marker dropped.
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        record.engram.begin_preparation_lineage.as_mut().unwrap().pending_grant_id = None;
        assert!(!engram_begin_preparation_newest_cause(record), "a transport cause");
        assert_eq!(
            engram_begin_preparation_resume(record),
            EngramBeginPreparationResume::PossiblySent,
            "a kept prepared begin decides, whatever the card's cause"
        );
    }
    let Err(error) = state.resume_session_queue(&session) else {
        panic!("Resume must say why the possibly sent begin stays held");
    };
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("may have been sent"), "{}", error.message);
    writer.finish(&state);
    assert!(begin_requests(&transport).is_empty(), "nothing sent");
    assert_eq!(abort_retry::prompts_received(&receiver), 0, "nothing delivered");
}

/// A replayed prepared begin that may have been sent (its pending marker
/// dropped by a restart) is never marked as this host's pending begin. That
/// holds whether its re-acknowledgement misses the wait or arrives late and
/// leaves its begin no time to be sent, so neither the budget's exhaustion
/// nor Stop and Resume can withdraw it as never sent. After a late
/// acknowledgement, the pre-send failure leaves it possibly begun, for its
/// exact replay.
#[test]
fn a_replayed_possibly_sent_prepared_begin_is_never_marked_pending() {
    for label in ["missed wait", "late acknowledgement"] {
        let (mut state, session, _receiver, transport) = lost_begin_fixture(false);
        let clock = state.engram_budget_clock();
        if label == "late acknowledgement" {
            // The production attempt budget: a late acknowledgement spends it.
            state.inner.lock().unwrap().test_engram_dispatch_budget = None;
        }
        let replayed = if label == "missed wait" {
            FenceStep::Fail(PersistFenceError::Deadline)
        } else {
            FenceStep::AcknowledgeLate(clock)
        };
        let writer = AcknowledgingWriter::install_with(
            &mut state,
            transport.log.clone(),
            Some(PreparedFence::FailSequence(vec![
                FenceStep::Fail(PersistFenceError::Deadline),
                FenceStep::Acknowledge,
                replayed,
            ])),
        );
        let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
        let saved = with_record(&state, &session, |record| {
            engram_begin_preparation_pending(record)
                .cloned()
                .unwrap_or_else(|| panic!("{label}: the first preparation is pending"))
        });
        // What a restart leaves (see the failed-write variant).
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            record.engram.begin_preparation_lineage.as_mut().unwrap().pending_grant_id = None;
            clear_engram_admission_retry(record);
            record.engram.operator_paused = true;
            record.set_auto_dispatch_blocked(true);
        }

        let _ = state.resume_session_queue(&session);
        assert_eq!(writer.injections(), 3, "{label}: the replayed preparation was scripted");
        assert!(begin_requests(&transport).is_empty(), "{label}: nothing was sent");
        with_record(&state, &session, |record| {
            assert!(
                engram_begin_preparation_pending(record).is_none(),
                "{label}: a possibly sent replay is never marked pending"
            );
            let kept = record.queued_prompts[0]
                .engram_evaluate
                .as_ref()
                .and_then(|evaluate| evaluate.prepared_begin.clone())
                .unwrap_or_else(|| panic!("{label}: never withdrawn"));
            assert_eq!(kept.idempotency_key, saved.idempotency_key, "{label}: the same begin");
            if label == "missed wait" {
                assert!(!record.engram.rebind_required, "{label}: its grant is not armed for a rebind");
            } else {
                // The begin-unknown route: its retained authorization is
                // restored, not expired, before the exact replay.
                assert_eq!(
                    record.engram.uncertain_grant_id.as_deref(),
                    Some(saved.grant_id.as_str()),
                    "{label}: left possibly begun, for its exact replay"
                );
            }
        });
        writer.finish(&state);
    }
}

/// C4: a Cancel ends the automatic recovery at once, for the fresh admission
/// after a failed write and for this host's pending begin alike. The prompt
/// is removed, no retry record is left, and later ticks past any due time
/// send nothing.
#[test]
fn cancel_ends_the_begin_preparation_recovery_immediately() {
    for label in ["failed write", "pending begin"] {
        let (state, session, receiver, transport, writer) = if label == "failed write" {
            settled_after_preparation_write_failure()
        } else {
            let (mut state, session, receiver, transport) = lost_begin_fixture(false);
            let writer = AcknowledgingWriter::install_with(
                &mut state,
                transport.log.clone(),
                Some(PreparedFence::FailTimes(PersistFenceError::Deadline, 1)),
            );
            let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
            (state, session, receiver, transport, writer)
        };
        let prompt_id =
            with_record(&state, &session, |record| record.queued_prompts[0].pending_prompt.id.clone());
        let sent = transport.producer.requests().len();
        if state.cancel_queued_prompt(&session, &prompt_id).is_err() {
            panic!("{label}: Cancel of the recovering prompt succeeds");
        }
        with_record(&state, &session, |record| {
            assert!(record.queued_prompts.is_empty(), "{label}: removed");
        });
        let later = chrono::Utc::now() + chrono::Duration::seconds(30);
        state.engram_abort_retry_tick(later);
        state.engram_abort_retry_tick(later + chrono::Duration::seconds(60));
        with_record(&state, &session, |record| {
            // A record naming the removed head is dropped by the tick.
            assert!(record.engram.abort_retry.is_none(), "{label}: no abort retry left");
            assert!(record.engram.admission_retry.is_none(), "{label}: no admission retry left");
        });
        writer.finish(&state);
        assert_eq!(transport.producer.requests().len(), sent, "{label}: no control call after Cancel");
        assert!(begin_requests(&transport).is_empty(), "{label}: nothing begun");
        assert_eq!(abort_retry::prompts_received(&receiver), 0, "{label}: nothing delivered");
    }
}

/// C4: the budget is spent at EXACTLY 240 seconds from acceptance. Just
/// before that instant it still runs; at it, the tick ends the recovery with
/// no automatic send, though an attempt is due.
#[test]
fn begin_preparation_recovery_is_exhausted_at_exactly_240_seconds() {
    let (state, session, receiver, transport, writer) = settled_after_preparation_write_failure();
    let budget = lineage(&state, &session).expect("the budget");
    let accepted = rfc3339(&budget.admitted_at);
    let deadline = rfc3339(&budget.deadline);
    assert_eq!(deadline, accepted + chrono::Duration::seconds(ENGRAM_ADMISSION_BUDGET_SECONDS));
    assert!(!budget.exhausted_at(deadline - chrono::Duration::milliseconds(1)), "running just before");
    assert!(budget.exhausted_at(deadline), "spent at exactly 240 s");
    let sent = transport.producer.requests().len();
    state.engram_abort_retry_tick(deadline);
    writer.finish(&state);
    assert_eq!(transport.producer.requests().len(), sent, "no automatic send at the deadline");
    assert_eq!(abort_retry::prompts_received(&receiver), 0, "nothing delivered");
    with_record(&state, &session, |record| {
        assert!(
            record.session.preview.starts_with("Engram: automatic re-admission exhausted after"),
            "{}",
            record.session.preview
        );
    });
}

/// C6: a restart near the deadline keeps only the time remaining. The saved
/// record reloaded one second before the deadline is still due for its
/// attempt; reloaded at the deadline it is exhausted. A stopped writer's
/// saved hold is readmitted at boot while time remains, and kept once the
/// saved deadline has passed.
#[test]
fn a_restart_near_the_deadline_keeps_only_the_remaining_budget() {
    let (state, session, _receiver, _transport, writer) = settled_after_preparation_write_failure();
    let saved = with_record(&state, &session, |record| {
        serde_json::to_string(&PersistedSessionRecord::from_record(record)).unwrap()
    });
    let authority = {
        let inner = state.inner.lock().unwrap();
        AppState::engram_binding_target_for_session_shape_locked(&inner, &session, true)
            .unwrap()
            .map(|target| engram_abort_authority(&target))
            .unwrap()
    };
    writer.finish(&state);
    let load = || {
        serde_json::from_str::<PersistedSessionRecord>(&saved)
            .unwrap()
            .into_record()
            .unwrap()
    };
    let deadline = rfc3339(&load().engram.begin_preparation_lineage.unwrap().deadline);
    let near = deadline - chrono::Duration::seconds(1);
    let mut loaded = load();
    assert_eq!(engram_begin_preparation_lineage_step(&mut loaded, near), None, "time remains");
    assert_eq!(
        engram_abort_retry_step(&mut loaded, Some(&authority), near),
        EngramAbortRetryStep::Due,
        "its attempt still starts one second before the deadline"
    );
    let mut loaded = load();
    assert_eq!(
        engram_begin_preparation_lineage_step(&mut loaded, deadline),
        Some(EngramAbortRetryStep::Dropped),
        "no new budget after the restart: exhausted at the original deadline"
    );

    // A stopped writer's saved hold, near its deadline: readmitted at boot
    // while time remains.
    let (mut state, session, _receiver, transport) = lost_begin_fixture(false);
    let writer = AcknowledgingWriter::install(
        &mut state,
        transport.log.clone(),
        Some(PersistFenceError::WorkerStopped),
    );
    // On the pinned clock, so boot measures the saved deadline on it too.
    let clock = pinned_clock(&state);
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let lineage = inner.sessions[index].engram.begin_preparation_lineage.as_mut().unwrap();
        lineage.deadline = (clock.wall_now() + chrono::Duration::seconds(5))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        assert!(
            state.readmit_engram_begin_preparation_holds_on_boot_locked(&mut inner),
            "readmitted while its saved budget has time left"
        );
    }
    writer.finish(&state);
}

/// C4 and C11(a): a Stop ends the window but keeps its basis; a Resume that
/// is refused (control disabled) keeps it too. The explicit accepted Resume
/// opens one new window in the write that releases the head, and the head
/// stays in prepared-begin recovery, so the new window is enforced. A
/// duplicate Resume keeps that window, and the prompt is delivered once.
#[test]
fn only_an_accepted_resume_opens_a_new_window_after_a_stop() {
    let (state, session, receiver, transport, writer) = settled_after_preparation_write_failure();
    let first = lineage(&state, &session).unwrap();
    state
        .request_stop_session(&session)
        .expect("Stop of the automatic recovery succeeds");
    assert_ended_window(&state, &session, &first, "after Stop");

    let set_disabled = |disabled: Option<&str>| {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram.disabled_reason = disabled.map(str::to_owned);
    };
    set_disabled(Some("protocol_error"));
    assert!(
        state
            .release_engram_begin_preparation_hold_for_resume(&session, None)
            .is_err(),
        "control disabled refuses the Resume"
    );
    assert_ended_window(&state, &session, &first, "after a refused Resume");
    set_disabled(None);

    let resumed_at = rfc3339(&chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    state
        .release_engram_begin_preparation_hold_for_resume(&session, None)
        .expect("the accepted Resume releases the hold");
    let opened = lineage(&state, &session).expect("one new window");
    assert!(!opened.ended, "the new window runs");
    assert!(rfc3339(&opened.admitted_at) >= resumed_at, "it starts at the Resume");
    assert_eq!(
        rfc3339(&opened.deadline),
        rfc3339(&opened.admitted_at) + chrono::Duration::seconds(ENGRAM_ADMISSION_BUDGET_SECONDS),
        "a full budget"
    );
    assert_eq!(opened.held_since, first.held_since, "still in prepared-begin recovery");
    assert!(opened.enforced(), "so the new window is enforced");
    assert_eq!(opened.attempts, 0, "its attempts are counted afresh");

    state
        .release_engram_begin_preparation_hold_for_resume(&session, None)
        .expect("a duplicate Resume is answered");
    assert_eq!(
        lineage(&state, &session),
        Some(opened),
        "a duplicate Resume neither resets nor replaces the window"
    );

    state
        .resume_session_queue(&session)
        .expect("Resume admits the released head");
    writer.finish(&state);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered once");
    assert_eq!(begin_requests(&transport).len(), 1, "exactly one begin");
}

/// The restart: the saved record replaces the live one, as a reload does,
/// then boot preparation runs.
fn restart_host(state: &AppState, session: &str) {
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(session).unwrap();
        let saved = PersistedSessionRecord::from_record(&inner.sessions[index]);
        inner.sessions[index] = saved.into_record().unwrap();
        inner.recover_interrupted_sessions();
    }
    state.prepare_engram_sessions_for_boot_recovery().unwrap();
}

/// C3, C4 and C6: a restart reads this host's pending prepared begin as
/// possibly sent (its pending marker is dropped on load), and its original
/// deadline still bounds it: possibly sent is not an applied begin. Before
/// the deadline its exact replay sends that same begin. At the deadline no
/// automatic send starts: the head ends in the explicit hold with its
/// prepared begin and grant kept, never withdrawn and never replaced by a
/// fresh grant, and an explicit Resume replays that same begin.
#[test]
fn a_restarted_pending_begin_keeps_its_original_deadline() {
    for label in ["before the deadline", "at the deadline"] {
        let (mut state, session, receiver, transport) = lost_begin_fixture(false);
        let writer = AcknowledgingWriter::install_with(
            &mut state,
            transport.log.clone(),
            Some(PreparedFence::FailTimes(PersistFenceError::Deadline, 1)),
        );
        let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
        let (pending, window) = with_record(&state, &session, |record| {
            (
                engram_begin_preparation_pending(record)
                    .cloned()
                    .unwrap_or_else(|| panic!("{label}: the begin is pending")),
                record.engram.begin_preparation_lineage.clone().unwrap(),
            )
        });
        assert!(begin_requests(&transport).is_empty(), "{label}: nothing sent before the restart");

        restart_host(&state, &session);
        with_record(&state, &session, |record| {
            assert_eq!(
                record.engram.uncertain_grant_id.as_deref(),
                Some(pending.grant_id.as_str()),
                "{label}: read as possibly sent"
            );
            let saved = record.engram.begin_preparation_lineage.as_ref().unwrap();
            assert_eq!(
                (&saved.deadline, &saved.held_since),
                (&window.deadline, &window.held_since),
                "{label}: the original deadline"
            );
            assert!(
                engram_admission_budget_enforced(record).is_some(),
                "{label}: possibly sent is still bounded"
            );
        });
        let same_begin = |label: &str| {
            let begins = begin_requests(&transport);
            assert_eq!(begins.len(), 1, "{label}: exactly one begin: {begins:?}");
            assert_eq!(begins[0]["idempotency_key"], pending.idempotency_key.as_str(), "{label}");
            assert_eq!(begins[0]["grant_id"], pending.grant_id.as_str(), "{label}");
            let evaluates = operations(&transport)
                .iter()
                .filter(|(operation, _)| operation == "turn_evaluate")
                .count();
            assert_eq!(evaluates, 1, "{label}: no fresh grant");
        };

        if label == "before the deadline" {
            for _ in 0..4 {
                tick_past_any_due(&state, &session);
            }
            writer.finish(&state);
            assert_eq!(abort_retry::prompts_received(&receiver), 1, "{label}: delivered once");
            same_begin(label);
            continue;
        }

        let deadline = rfc3339(&window.deadline);
        let sent = transport.producer.requests().len();
        state.engram_abort_retry_tick(deadline);
        state.engram_abort_retry_tick(deadline + chrono::Duration::hours(1));
        assert_eq!(
            transport.producer.requests().len(),
            sent,
            "{label}: no automatic send at or after the deadline"
        );
        with_record(&state, &session, |record| {
            let kept = record.queued_prompts[0]
                .engram_evaluate
                .as_ref()
                .and_then(|evaluate| evaluate.prepared_begin.clone())
                .unwrap_or_else(|| panic!("{label}: the prepared begin is never withdrawn"));
            assert_eq!(kept.idempotency_key, pending.idempotency_key, "{label}");
            assert_eq!(kept.grant_id, pending.grant_id, "{label}");
            assert_eq!(
                record.engram.uncertain_grant_id.as_deref(),
                Some(pending.grant_id.as_str()),
                "{label}: still possibly sent"
            );
            assert!(record.engram.admission_retry.is_none(), "{label}: nothing scheduled");
            assert!(record.orchestrator_auto_dispatch_blocked, "{label}: the explicit hold");
            assert!(
                record.session.preview.contains("may have been sent"),
                "{label}: {}",
                record.session.preview
            );
        });
        assert_ended_window(&state, &session, &window, label);
        assert!(begin_requests(&transport).is_empty(), "{label}: nothing sent");

        state
            .resume_session_queue(&session)
            .unwrap_or_else(|_| panic!("{label}: Resume reconciles the held begin"));
        writer.finish(&state);
        assert_eq!(abort_retry::prompts_received(&receiver), 1, "{label}: delivered once");
        same_begin(label);
    }
}

/// C11(a): a Resume bound to an owner that a successor has since replaced
/// (the held head cancelled, which exposes the next prompt) is answered at
/// the owner-scoped Resume boundary a delegation's Resume calls, which types
/// a promotion-free answer as `Superseded`: nothing is promoted, the
/// successor is neither drained nor altered, and nothing is sent.
#[test]
fn a_stale_owner_resume_is_superseded_and_leaves_the_successor_untouched() {
    let (state, session, receiver, transport, writer) = held_after_stopped_writer();
    let stale = with_record(&state, &session, |record| {
        EngramQueuedAdmissionOwner::capture(record).expect("the held head's owner")
    });
    queue_test_engram_prompt(
        &state,
        &session,
        "Successor prompt",
        QueuedPromptSource::User,
        None,
    );
    state
        .cancel_queued_prompt(&session, &stale.prompt_id)
        .expect("Cancel of the held head succeeds");
    let successor = |record: &SessionRecord| {
        let head = record.queued_prompts.front().expect("the successor is queued");
        (
            record.queued_prompts.len(),
            head.pending_prompt.id.clone(),
            head.engram_evaluate.is_some(),
            head.engram_waiting,
            head.engram_interrupted,
            record.engram.dispatch_generation,
            record.orchestrator_auto_dispatch_blocked,
            record.engram.begin_preparation_lineage.clone(),
        )
    };
    let before = with_record(&state, &session, successor);
    assert_ne!(before.1, stale.prompt_id, "the successor is the head now");
    let sent = transport.producer.requests().len();

    let outcome = state
        .resume_local_session_queue(&session, Some(stale))
        .expect("the stale Resume is answered, not failed");
    assert!(
        matches!(outcome, None | Some(TurnDispatchDeliveryOutcome::Superseded)),
        "nothing is promoted for the stale owner: {outcome:?}"
    );
    assert_eq!(with_record(&state, &session, successor), before, "the successor is untouched");
    assert_eq!(transport.producer.requests().len(), sent, "no control call");
    writer.finish(&state);
    assert_eq!(abort_retry::prompts_received(&receiver), 0, "nothing delivered");
}

/// C4 as clarified for this item: a head that never entered prepared-begin
/// recovery keeps its existing behavior. Its budget starts at acceptance but
/// is never enforced, so a begin whose reply was lost keeps its exact replay
/// unclipped past the 240 second deadline, and that replay delivers the
/// prompt once with the same begin and no fresh grant.
#[test]
fn a_head_that_never_entered_prepared_begin_recovery_keeps_its_replay_unbounded() {
    let (state, session, receiver, transport) = lost_begin_fixture(true);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    assert!(receiver.try_recv().is_err(), "the lost reply withholds delivery");
    let first = begin_requests(&transport)[0].clone();
    let window = with_record(&state, &session, |record| {
        assert_eq!(
            record.engram.uncertain_grant_id.as_deref(),
            first["grant_id"].as_str(),
            "the begin's outcome is unknown"
        );
        assert!(
            engram_admission_budget_enforced(record).is_none(),
            "no prepared begin was withheld"
        );
        record
            .engram
            .begin_preparation_lineage
            .clone()
            .expect("the budget started at acceptance")
    });
    assert!(!window.enforced());

    let late = past_the_window(&window) + chrono::Duration::seconds(60);
    for step in 0..3 {
        tick_at(&state, &session, late + chrono::Duration::seconds(step));
    }
    assert_eq!(
        abort_retry::prompts_received(&receiver),
        1,
        "replayed and delivered past the budget"
    );
    let begins = begin_requests(&transport);
    assert!(begins.len() >= 2, "the begin was replayed: {begins:?}");
    assert!(
        begins.iter().all(|begin| begin["idempotency_key"] == first["idempotency_key"]),
        "every begin is the same begin: {begins:?}"
    );
    assert_eq!(
        operations(&transport)
            .iter()
            .filter(|(operation, _)| operation == "turn_evaluate")
            .count(),
        1,
        "no fresh grant"
    );
}

/// C4: control disabled ends the window of this host's pending begin, and a
/// restart, which does not save control disabled, gives no automatic
/// advancement back: the boot pass marks the saved begin possibly sent but
/// schedules nothing for a head with an ended window, so nothing is sent
/// until Resume. Resume then replays that same begin, and the prompt is
/// delivered once.
#[test]
fn a_restart_after_control_disabled_sends_nothing_until_resume() {
    let (mut state, session, receiver, transport) = lost_begin_fixture(false);
    let writer = AcknowledgingWriter::install_with(
        &mut state,
        transport.log.clone(),
        Some(PreparedFence::FailTimes(PersistFenceError::Deadline, 1)),
    );
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    let (pending, window) = with_record(&state, &session, |record| {
        (
            engram_begin_preparation_pending(record).cloned().expect("the begin is pending"),
            record.engram.begin_preparation_lineage.clone().unwrap(),
        )
    });
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram.disabled_reason = Some("protocol_error".to_owned());
    }
    tick_past_any_due(&state, &session);
    assert_ended_window(&state, &session, &window, "control disabled");

    restart_host(&state, &session);
    with_record(&state, &session, |record| {
        assert_eq!(record.engram.disabled_reason, None, "control disabled is not saved");
        assert!(record.engram.admission_retry.is_none(), "the boot pass schedules nothing");
        assert_eq!(
            record.engram.uncertain_grant_id.as_deref(),
            Some(pending.grant_id.as_str()),
            "the saved begin is read as possibly sent, for its exact replay on Resume"
        );
    });
    assert_ended_window(&state, &session, &window, "after the restart");
    for _ in 0..3 {
        tick_past_any_due(&state, &session);
    }
    assert!(begin_requests(&transport).is_empty(), "nothing sent without Resume");
    assert_eq!(abort_retry::prompts_received(&receiver), 0, "nothing delivered");

    state
        .resume_session_queue(&session)
        .expect("Resume moves the held head on");
    writer.finish(&state);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered once");
    let begins = begin_requests(&transport);
    assert_eq!(begins.len(), 1, "exactly one begin: {begins:?}");
    assert_eq!(begins[0]["idempotency_key"], pending.idempotency_key.as_str(), "the same begin");
    assert_eq!(begins[0]["grant_id"], pending.grant_id.as_str(), "its grant was kept");
}

/// C4: a fatal control response met by the pending begin's own retry, not a
/// disable set beside an idle scheduled retry, disables control and ends the
/// window in the same transition. Disabling removes the retry record the tick
/// would have ended the window through, and a restart does not save control
/// disabled, so only the ended window keeps the boot pass from replaying the
/// possibly sent begin automatically. Resume then replays that same begin,
/// and the prompt is delivered once.
#[test]
fn a_fatal_control_response_during_a_pending_begin_retry_stays_held_after_restart() {
    let (mut state, session, receiver, transport) = lost_begin_fixture(false);
    let writer = AcknowledgingWriter::install_with(
        &mut state,
        transport.log.clone(),
        Some(PreparedFence::FailTimes(PersistFenceError::Deadline, 1)),
    );
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    let (pending, window) = with_record(&state, &session, |record| {
        (
            engram_begin_preparation_pending(record).cloned().expect("the begin is pending"),
            record.engram.begin_preparation_lineage.clone().unwrap(),
        )
    });
    let sent_begins = || {
        transport
            .log
            .snapshot()
            .into_iter()
            .filter_map(|boundary| match boundary {
                Boundary::Sent(begin) => Some(begin),
                Boundary::Acknowledged(_) => None,
            })
            .collect::<Vec<_>>()
    };
    transport
        .fail_next_begin_with_control_unavailable
        .store(true, Ordering::SeqCst);
    tick_past_any_due(&state, &session);
    assert_eq!(sent_begins().len(), 1, "the retry sent the pending begin and met the fatal reply");
    with_record(&state, &session, |record| {
        assert_eq!(record.engram.disabled_reason.as_deref(), Some("control_unavailable"));
        assert!(record.engram.admission_retry.is_none(), "disabling removed the retry");
    });
    assert_ended_window(&state, &session, &window, "fatal control response");
    assert_eq!(abort_retry::prompts_received(&receiver), 0, "the prompt is still withheld");

    restart_host(&state, &session);
    with_record(&state, &session, |record| {
        assert_eq!(record.engram.disabled_reason, None, "control disabled is not saved");
        assert!(record.engram.admission_retry.is_none(), "the boot pass schedules nothing");
        assert_eq!(
            record.engram.uncertain_grant_id.as_deref(),
            Some(pending.grant_id.as_str()),
            "the saved begin is read as possibly sent, for its exact replay on Resume"
        );
    });
    assert_ended_window(&state, &session, &window, "after the restart");
    for _ in 0..3 {
        tick_past_any_due(&state, &session);
    }
    assert_eq!(sent_begins().len(), 1, "nothing sent again without Resume");
    assert_eq!(abort_retry::prompts_received(&receiver), 0, "nothing delivered");

    state
        .resume_session_queue(&session)
        .expect("Resume moves the held head on");
    writer.finish(&state);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered once");
    let sent = sent_begins();
    assert_eq!(sent.len(), 2, "the fatal retry, then Resume's replay: {sent:?}");
    assert!(
        sent.iter().all(|begin| {
            begin["idempotency_key"] == pending.idempotency_key.as_str()
                && begin["grant_id"] == pending.grant_id.as_str()
        }),
        "both are the same begin: {sent:?}"
    );
}

/// C4 and the window rule: a project's Engram settings change keeps the
/// window of a head in prepared-begin recovery, ended with its start,
/// deadline and first hold, so the change grants it no new window. A head
/// that never entered that recovery has its budget dropped with the rest of
/// the session's Engram state, as before.
#[test]
fn a_settings_change_keeps_an_ended_window_and_drops_any_other_budget() {
    let (state, session, _receiver, _transport, writer) = settled_after_preparation_write_failure();
    let window = lineage(&state, &session).unwrap();
    assert!(window.enforced(), "the head is in prepared-begin recovery");
    let record = with_record(&state, &session, SessionRecord::clone);
    writer.finish(&state);

    let mut enforced = record.clone();
    let kept = take_engram_admission_budget_for_settings_reset(&mut enforced)
        .expect("the window of a head in prepared-begin recovery is kept");
    assert!(kept.ended, "and ended");
    assert_eq!(
        (&kept.admitted_at, &kept.deadline, &kept.held_since),
        (&window.admitted_at, &window.deadline, &window.held_since),
        "with its basis"
    );

    let mut never_entered = record;
    if let Some(lineage) = never_entered.engram.begin_preparation_lineage.as_mut() {
        lineage.held_since = None;
    }
    assert_eq!(
        take_engram_admission_budget_for_settings_reset(&mut never_entered),
        None,
        "a budget never enforced is dropped, as before"
    );
}

/// The state's scripted budget clock with its wall reading pinned, before any
/// retry is scheduled: every due time, tick time and deadline of the test
/// then derives from that one scripted clock, not from real time.
fn pinned_clock(state: &AppState) -> EngramBudgetClock {
    let clock = state.engram_budget_clock();
    clock.pin_wall();
    clock
}

/// Sets `session`'s window deadline to `deadline` (a wall time on `clock`,
/// whose wall reading `pinned_clock` pinned, saved at millisecond precision).
/// Returns the deadline instant the host measures on the scripted clock: it
/// depends on that clock alone, not on how much real time passes.
fn pin_window_deadline(
    state: &AppState,
    session: &str,
    clock: &EngramBudgetClock,
    deadline: impl FnOnce(chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc>,
) -> std::time::Instant {
    let saved = deadline(clock.wall_now()).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let saved_at = rfc3339(&saved);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(session).unwrap();
        inner.sessions[index].engram.begin_preparation_lineage.as_mut().unwrap().deadline = saved;
    }
    clock.now() + (saved_at - clock.wall_now()).to_std().expect("the deadline is ahead")
}

/// This host's pending begin, its window cut to five seconds (less than a
/// Begin's least call bound, so the deadline bounds the next Begin call
/// exactly), then its admission retry run with the applied Begin reply
/// first observed at `timing` relative to that deadline.
fn pending_begin_replied(
    timing: BeginReplyTiming,
) -> (
    AppState,
    String,
    mpsc::Receiver<CodexRuntimeCommand>,
    Arc<LostBeginReply>,
    AcknowledgingWriter,
    EngramPreparedBegin,
) {
    let (mut state, session, receiver, transport) = lost_begin_fixture(false);
    let writer = AcknowledgingWriter::install_with(
        &mut state,
        transport.log.clone(),
        Some(PreparedFence::FailTimes(PersistFenceError::Deadline, 1)),
    );
    let clock = pinned_clock(&state);
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    let pending = with_record(&state, &session, |record| {
        engram_begin_preparation_pending(record).cloned().expect("the begin is pending")
    });
    pin_window_deadline(&state, &session, &clock, |now| now + chrono::Duration::seconds(5));
    *transport.begin_reply_observed.lock().unwrap() = Some((clock, timing));
    tick_past_any_due(&state, &session);
    assert!(
        transport.begin_reply_observed.lock().unwrap().is_none(),
        "{timing:?}: the pending begin was sent and applied"
    );
    (state, session, receiver, transport, writer, pending)
}

/// C4 (revision 11): receipt time is when the host first observed the
/// complete matching Begin response, on the operation budget clock. A
/// receipt observed strictly before the deadline ends the budget even when
/// the local steps after it cross the deadline: here the writer crosses a
/// barrier past the deadline before resolving the next fence, the
/// admission's durability fence before the provider handoff, and the prompt
/// is still delivered once.
#[test]
fn an_applied_begin_received_before_the_deadline_is_delivered_once() {
    let (state, session, receiver, transport, writer, pending) =
        pending_begin_replied(BeginReplyTiming::BeforeThenBarrierPast);
    assert_eq!(
        transport.log.fence_barriers_crossed(),
        1,
        "local processing crossed the deadline after the receipt"
    );
    writer.finish(&state);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered once");
    let begins = begin_requests(&transport);
    assert_eq!(begins.len(), 1, "one begin: {begins:?}");
    assert_eq!(begins[0]["idempotency_key"], pending.idempotency_key.as_str());
    with_record(&state, &session, |record| {
        assert!(record.queued_prompts.is_empty(), "the head was delivered");
        assert_eq!(record.engram.uncertain_grant_id, None);
    });
}

/// C4 (revision 11) and C3: a matching applied-Begin receipt first observed
/// exactly at the deadline, or after it, is kept as the actual outcome
/// ("applied, first observed after the deadline", on the card and on the
/// hold) but revives no automatic advancement: nothing is delivered, and the
/// tick ends the recovery in the explicit hold with the prepared begin and
/// its grant kept. Resume then replays that same begin, whose idempotent
/// receipt delivers the prompt exactly once overall.
#[test]
fn an_applied_begin_first_received_at_or_after_the_deadline_is_held() {
    for timing in [BeginReplyTiming::At, BeginReplyTiming::After] {
        let (state, session, receiver, transport, writer, pending) = pending_begin_replied(timing);
        assert_eq!(
            abort_retry::prompts_received(&receiver),
            0,
            "{timing:?}: no automatic delivery"
        );
        with_record(&state, &session, |record| {
            assert_eq!(
                record.engram.active_grant_id, None,
                "{timing:?}: the late receipt does not make the grant active"
            );
            assert_eq!(
                record.engram.uncertain_grant_id.as_deref(),
                Some(pending.grant_id.as_str()),
                "{timing:?}: kept for its exact replay"
            );
            assert_eq!(
                engram_newest_card_code(record).as_deref(),
                Some(ENGRAM_BEGIN_APPLIED_AFTER_DEADLINE_CODE),
                "{timing:?}: the card names the observed outcome"
            );
            let lineage = record.engram.begin_preparation_lineage.as_ref().unwrap();
            assert_eq!(
                lineage.applied_after_deadline.as_deref(),
                Some(pending.grant_id.as_str()),
                "{timing:?}: the hold records the applied grant"
            );
        });
        let cause = begin_cause(&state, &session).expect("the card's cause");
        assert_eq!(cause["originalCode"], ENGRAM_BEGIN_APPLIED_AFTER_DEADLINE_CODE, "{timing:?}");
        assert!(
            cause["message"].as_str().is_some_and(|message| message.contains("applied")),
            "{timing:?}: {cause}"
        );

        // Any tick ends the recovery: the receipt spent the budget on the
        // operation clock.
        let sent = transport.producer.requests().len();
        let wall = state.engram_budget_clock().wall_now();
        state.engram_abort_retry_tick(wall);
        state.engram_abort_retry_tick(wall + chrono::Duration::hours(1));
        assert_eq!(transport.producer.requests().len(), sent, "{timing:?}: no automatic send");
        with_record(&state, &session, |record| {
            assert!(record.engram.admission_retry.is_none(), "{timing:?}: nothing scheduled");
            assert!(record.orchestrator_auto_dispatch_blocked, "{timing:?}: the explicit hold");
            assert!(
                record.session.preview.contains("first observed after the deadline"),
                "{timing:?}: {}",
                record.session.preview
            );
            let kept = record.queued_prompts[0]
                .engram_evaluate
                .as_ref()
                .and_then(|evaluate| evaluate.prepared_begin.clone())
                .expect("the prepared begin is kept");
            assert_eq!(kept.idempotency_key, pending.idempotency_key, "{timing:?}");
        });
        assert_eq!(abort_retry::prompts_received(&receiver), 0, "{timing:?}: still nothing");

        state
            .resume_session_queue(&session)
            .unwrap_or_else(|_| panic!("{timing:?}: Resume replays the applied begin"));
        // Resume again once the prompt was delivered: nothing more is sent.
        state
            .resume_session_queue(&session)
            .unwrap_or_else(|_| panic!("{timing:?}: a repeated Resume is answered"));
        writer.finish(&state);
        assert_eq!(
            abort_retry::prompts_received(&receiver),
            1,
            "{timing:?}: at most one delivery overall"
        );
        with_record(&state, &session, |record| {
            assert!(record.queued_prompts.is_empty(), "{timing:?}: the head was delivered");
        });
        let begins = begin_requests(&transport);
        assert_eq!(begins.len(), 2, "{timing:?}: the begin and its exact replay: {begins:?}");
        assert!(
            begins
                .iter()
                .all(|begin| begin["idempotency_key"] == pending.idempotency_key.as_str()),
            "{timing:?}: the same begin each time: {begins:?}"
        );
        assert_eq!(
            operations(&transport)
                .iter()
                .filter(|(operation, _)| operation == "turn_evaluate")
                .count(),
            1,
            "{timing:?}: no fresh evaluate or grant"
        );
    }
}

/// The status-read route: a replay of a possibly sent begin is refused
/// `grant_scope_mismatch`, and only the session status that names its grant
/// begun settles it as applied. Its receipt time is when the host observed
/// that status response, as the read returned. With the clock's wall reading
/// pinned, the host's deadline instant is exact, so the read is placed
/// exactly: a read observed just before the deadline is timely even when
/// the host's own processing after it (the owner check, which may wait on
/// the state lock) crosses the deadline, and the prompt is delivered once;
/// a read observed exactly at the deadline, or after it, keeps the applied
/// outcome and records it, but delivers nothing. And when the owner check
/// before the read crosses the deadline, the read is not started at all.
#[test]
fn a_status_read_settling_an_applied_begin_at_or_after_the_deadline_is_held() {
    for timing in [
        Some(BeginReplyTiming::BeforeThenBarrierPast),
        Some(BeginReplyTiming::At),
        Some(BeginReplyTiming::After),
        // The owner check before the read crosses the deadline.
        None,
    ] {
        let (mut state, session, receiver, transport) = lost_begin_fixture(false);
        let writer = AcknowledgingWriter::install_with(
            &mut state,
            transport.log.clone(),
            Some(PreparedFence::FailTimes(PersistFenceError::Deadline, 1)),
        );
        let clock = pinned_clock(&state);
        let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
        let pending = with_record(&state, &session, |record| {
            engram_begin_preparation_pending(record).cloned().expect("the begin is pending")
        });
        // Its re-acknowledged begin is applied, but the reply is lost: the
        // grant is possibly begun, for its exact replay.
        transport.lose_next_begin_reply.store(true, Ordering::SeqCst);
        tick_past_any_due(&state, &session);
        assert_eq!(
            with_record(&state, &session, |record| record.engram.uncertain_grant_id.clone()),
            Some(pending.grant_id.clone()),
            "{timing:?}: possibly begun"
        );
        // The deadline five seconds past the replay's due time, read from
        // its retained record (scheduled on the pinned clock): the replay
        // starts inside the budget.
        let due = with_record(&state, &session, |record| {
            rfc3339(&record.engram.admission_retry.as_ref().expect("the replay").due_at)
        });
        let deadline =
            pin_window_deadline(&state, &session, &clock, |_| due + chrono::Duration::seconds(5));
        let one = Duration::from_millis(1);
        let observed_at = match timing {
            Some(BeginReplyTiming::BeforeThenBarrierPast) | None => deadline - one,
            Some(BeginReplyTiming::At) => deadline,
            Some(BeginReplyTiming::After) => deadline + one,
        };
        let barrier = clock.clone();
        if timing == Some(BeginReplyTiming::BeforeThenBarrierPast) {
            // The host's processing after the read crosses the deadline.
            state.inner.lock().unwrap().test_engram_authority_ack_boundary =
                Some(Arc::new(move |boundary| {
                    if boundary == "begin_status_observed" {
                        barrier.advance(Duration::from_millis(2));
                    }
                }));
        } else if timing.is_none() {
            // The owner check before the read waits past the deadline.
            state.inner.lock().unwrap().test_engram_authority_ack_boundary =
                Some(Arc::new(move |boundary| {
                    if boundary == "begin_status_owner_checked" {
                        barrier.advance((deadline + one).saturating_duration_since(barrier.now()));
                    }
                }));
        }
        let status_reads = || {
            operations(&transport)
                .iter()
                .filter(|(operation, _)| operation == "session_status")
                .count()
        };
        let reads_before = status_reads();
        transport.refuse_next_begin_as_scope_mismatch.store(true, Ordering::SeqCst);
        *transport.status_reports_begun.lock().unwrap() = Some((clock.clone(), observed_at));
        tick_past_any_due(&state, &session);
        state.inner.lock().unwrap().test_engram_authority_ack_boundary = None;
        if timing.is_none() {
            assert_eq!(status_reads(), reads_before, "no status read starts after the deadline");
            assert!(
                transport.status_reports_begun.lock().unwrap().is_some(),
                "the read was never made"
            );
            writer.finish(&state);
            assert_eq!(abort_retry::prompts_received(&receiver), 0, "nothing delivered");
            continue;
        }
        assert!(
            transport.status_reports_begun.lock().unwrap().is_none(),
            "{timing:?}: the status read settled the refused replay"
        );
        if timing == Some(BeginReplyTiming::BeforeThenBarrierPast) {
            assert!(clock.now() > deadline, "{timing:?}: local processing crossed the deadline");
            writer.finish(&state);
            assert_eq!(
                abort_retry::prompts_received(&receiver),
                1,
                "{timing:?}: a timely status settlement is delivered once"
            );
            with_record(&state, &session, |record| {
                assert!(record.queued_prompts.is_empty(), "{timing:?}: the head was delivered");
            });
            continue;
        }
        assert_eq!(abort_retry::prompts_received(&receiver), 0, "{timing:?}: no delivery");
        with_record(&state, &session, |record| {
            assert_eq!(record.engram.active_grant_id, None, "{timing:?}: not made active");
            assert_eq!(
                record.engram.uncertain_grant_id.as_deref(),
                Some(pending.grant_id.as_str()),
                "{timing:?}: kept for its exact replay"
            );
            assert_eq!(
                engram_newest_card_code(record).as_deref(),
                Some(ENGRAM_BEGIN_APPLIED_AFTER_DEADLINE_CODE),
                "{timing:?}: the card names the observed outcome"
            );
            assert_eq!(
                record
                    .engram
                    .begin_preparation_lineage
                    .as_ref()
                    .and_then(|lineage| lineage.applied_after_deadline.as_deref()),
                Some(pending.grant_id.as_str()),
                "{timing:?}: the hold records the applied grant"
            );
        });
        state.engram_abort_retry_tick(clock.wall_now());
        writer.finish(&state);
        with_record(&state, &session, |record| {
            assert!(record.engram.admission_retry.is_none(), "{timing:?}: nothing scheduled");
            assert!(
                record.session.preview.contains("first observed after the deadline"),
                "{timing:?}: {}",
                record.session.preview
            );
        });
        assert_eq!(abort_retry::prompts_received(&receiver), 0, "{timing:?}: still nothing");
    }
}

/// This host's pending begin whose attempt finds the budget already spent
/// when it is dispatched (the deadline passed after the tick judged it due)
/// is withheld with nothing sent, and its grant is not armed for the orphan
/// repair: no rebind expires the grant its exact begin still needs, and the
/// card does not show a repair.
#[test]
fn a_pending_begin_whose_attempt_finds_the_budget_spent_keeps_its_grant() {
    let (mut state, session, receiver, transport) = lost_begin_fixture(false);
    let writer = AcknowledgingWriter::install_with(
        &mut state,
        transport.log.clone(),
        Some(PreparedFence::FailTimes(PersistFenceError::Deadline, 1)),
    );
    let clock = pinned_clock(&state);
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    let pending = with_record(&state, &session, |record| {
        engram_begin_preparation_pending(record).cloned().expect("the begin is pending")
    });
    let deadline =
        pin_window_deadline(&state, &session, &clock, |now| now + chrono::Duration::seconds(5));
    // The tick runs at the retry's due time, before the saved deadline, but
    // the operation clock has already passed it when the attempt runs.
    clock.advance(deadline.saturating_duration_since(clock.now()) + Duration::from_secs(1));
    let binds_before = operations(&transport)
        .iter()
        .filter(|(operation, _)| operation == "session_bind")
        .count();
    tick_past_any_due(&state, &session);
    writer.finish(&state);
    assert!(begin_requests(&transport).is_empty(), "nothing sent");
    assert_eq!(abort_retry::prompts_received(&receiver), 0, "nothing delivered");
    assert_eq!(
        operations(&transport)
            .iter()
            .filter(|(operation, _)| operation == "session_bind")
            .count(),
        binds_before,
        "no rebind"
    );
    with_record(&state, &session, |record| {
        assert!(!record.engram.rebind_required, "no orphan repair is armed");
        let kept = engram_begin_preparation_pending(record).expect("the pending begin is kept");
        assert_eq!(kept.grant_id, pending.grant_id, "with its grant");
        assert_eq!(kept.idempotency_key, pending.idempotency_key, "and its key");
        let repair_armed = record.session.messages.iter().rev().find_map(|message| match message {
            Message::EngramControl { card, .. } => Some(card.repair_armed),
            _ => None,
        });
        assert_eq!(repair_armed, Some(false), "the card shows no repair");
    });
}

/// This host's pending begin whose re-acknowledgement then fails terminally
/// (the writer stopped) is withdrawn, never sent: its grant is a first
/// begin's issued orphan, so the orphan repair is armed for it, as for a
/// first begin, and the card shows that repair.
#[test]
fn a_pending_begin_withdrawn_after_a_terminal_failure_arms_the_orphan_repair() {
    let (mut state, session, receiver, transport) = lost_begin_fixture(false);
    let writer = AcknowledgingWriter::install_with(
        &mut state,
        transport.log.clone(),
        Some(PreparedFence::FailSequence(vec![
            FenceStep::Fail(PersistFenceError::Deadline),
            FenceStep::Fail(PersistFenceError::WorkerStopped),
        ])),
    );
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    with_record(&state, &session, |record| {
        assert!(engram_begin_preparation_pending(record).is_some(), "the begin is pending");
        assert!(!record.engram.rebind_required, "no repair while it is pending");
    });
    tick_past_any_due(&state, &session);
    assert_eq!(writer.injections(), 2, "the re-acknowledgement met the stopped writer");
    writer.finish(&state);
    assert!(begin_requests(&transport).is_empty(), "nothing sent");
    assert_eq!(abort_retry::prompts_received(&receiver), 0, "nothing delivered");
    with_record(&state, &session, |record| {
        assert!(engram_begin_preparation_pending(record).is_none(), "no longer pending");
        assert!(
            record.queued_prompts[0]
                .engram_evaluate
                .as_ref()
                .is_none_or(|evaluate| evaluate.prepared_begin.is_none()),
            "the never-sent prepared begin is withdrawn"
        );
        assert!(record.engram.rebind_required, "the orphan repair is armed");
        let repair_armed = record.session.messages.iter().rev().find_map(|message| match message {
            Message::EngramControl { card, .. } => Some(card.repair_armed),
            _ => None,
        });
        assert_eq!(repair_armed, Some(true), "the card shows the repair");
    });
}

/// A project's Engram settings change drops a pending prepared begin's
/// marker with the ended window it keeps: that begin, evaluated under the
/// old settings, is no longer this host's pending begin to send, so a Resume
/// of the interrupted head says it may have been sent and keeps the hold,
/// never sending it under the new settings.
#[test]
fn a_settings_change_ends_a_pending_begins_claim_to_be_sent() {
    let (mut state, session, receiver, transport) = lost_begin_fixture(false);
    let writer = AcknowledgingWriter::install_with(
        &mut state,
        transport.log.clone(),
        Some(PreparedFence::FailTimes(PersistFenceError::Deadline, 1)),
    );
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        assert!(engram_begin_preparation_pending(record).is_some(), "the begin is pending");
        // What the settings reset does to the head: its budget is taken
        // through the helper, its retries end, and the head is interrupted.
        let kept = take_engram_admission_budget_for_settings_reset(record)
            .expect("the window of a head in prepared-begin recovery is kept");
        assert!(kept.ended, "ended");
        assert_eq!(kept.pending_grant_id, None, "the pending marker is dropped");
        record.engram.begin_preparation_lineage = Some(kept);
        clear_engram_admission_retry(record);
        record.queued_prompts[0].engram_interrupted = true;
        record.set_auto_dispatch_blocked(true);
        assert_eq!(
            engram_begin_preparation_resume(record),
            EngramBeginPreparationResume::PossiblySent
        );
    }
    let Err(error) = state.resume_session_queue(&session) else {
        panic!("Resume must not send the old settings' prepared begin");
    };
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains("may have been sent"), "{}", error.message);
    writer.finish(&state);
    assert!(begin_requests(&transport).is_empty(), "nothing sent");
    assert_eq!(abort_retry::prompts_received(&receiver), 0, "nothing delivered");
}

/// Cancel on a late-applied hold suppresses delivery and advancement at once
/// and keeps the settlement obligation: the applied grant stays recorded as
/// possibly begun, with the rebind armed so the next bind settles it by a
/// session status before any successor is admitted. The Begin is never
/// relabelled as not started, no further Begin is sent, and the card keeps
/// the applied outcome.
#[test]
fn cancel_on_a_late_applied_hold_keeps_its_settlement_obligation() {
    let (state, session, receiver, transport, writer, pending) =
        pending_begin_replied(BeginReplyTiming::At);
    let wall = state.engram_budget_clock().wall_now();
    state.engram_abort_retry_tick(wall);
    let begins_before = begin_requests(&transport).len();
    let prompt_id =
        with_record(&state, &session, |record| record.queued_prompts[0].pending_prompt.id.clone());
    state
        .cancel_queued_prompt(&session, &prompt_id)
        .expect("Cancel of the held prompt succeeds");
    state.engram_abort_retry_tick(wall + chrono::Duration::hours(1));
    with_record(&state, &session, |record| {
        assert!(record.queued_prompts.is_empty(), "the prompt is removed");
        assert_eq!(
            record.engram.uncertain_grant_id.as_deref(),
            Some(pending.grant_id.as_str()),
            "the applied grant is kept for settlement"
        );
        assert!(record.engram.rebind_required, "the next bind settles it first");
        assert!(record.engram.admission_retry.is_none(), "nothing scheduled");
    });
    let cause = begin_cause(&state, &session).expect("the card's cause");
    assert_eq!(cause["originalCode"], ENGRAM_BEGIN_APPLIED_AFTER_DEADLINE_CODE);
    assert_ne!(cause["remoteApplication"], "not_started", "never relabelled as not started");
    writer.finish(&state);
    assert_eq!(begin_requests(&transport).len(), begins_before, "no further begin");
    assert_eq!(abort_retry::prompts_received(&receiver), 0, "nothing delivered");
}

/// The receipt-time rule both routes use, the Begin reply and the status
/// read that settles a scope mismatch as applied: late at or after the
/// enforced deadline, timely strictly before it, an unobserved receipt never
/// assumed timely, and nothing late without an enforced budget.
#[test]
fn a_receipt_is_late_at_or_after_the_enforced_deadline() {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let one = Duration::from_millis(1);
    assert!(!engram_begin_receipt_after_deadline(Some(deadline), Some(deadline - one)));
    assert!(engram_begin_receipt_after_deadline(Some(deadline), Some(deadline)));
    assert!(engram_begin_receipt_after_deadline(Some(deadline), Some(deadline + one)));
    assert!(engram_begin_receipt_after_deadline(Some(deadline), None));
    assert!(!engram_begin_receipt_after_deadline(None, Some(deadline + one)));
    assert!(!engram_begin_receipt_after_deadline(None, None));
}

/// The retry tick just past the head's due time, each pass after any
/// pending acknowledgement of its retry record has resolved, so how many
/// passes an attempt needs does not depend on the writer's timing.
fn tick_acknowledged_past_due(state: &AppState, session: &str) {
    abort_retry::await_settlement_acknowledgement(state, session);
    tick_past_any_due(state, session);
}

/// C3: a prepared-begin acknowledgement that arrives after its owner was
/// superseded (the head cancelled while its admission waits on that
/// acknowledgement, which exposes the next prompt) changes nothing. No Begin
/// is sent and nothing is delivered; the successor is neither admitted nor
/// altered, and the superseded dispatch records no card in its session. The
/// old operation's outcome stays where the Cancel recorded it: the grant
/// whose begin it had requested, kept for its settlement by the next bind.
/// That settlement records its actual outcome: when the successor is
/// admitted, the status read and Engram's refusal of its checkpoint settle it
/// as never begun before the fresh bind, and the successor is delivered once
/// on a grant of its own. This holds for a first preparation's
/// acknowledgement and for a pending begin's re-acknowledgement on its retry.
#[test]
fn a_prepared_begin_acknowledged_after_owner_supersession_sends_nothing() {
    for label in ["first preparation", "pending re-acknowledgement"] {
        let pending = label == "pending re-acknowledgement";
        let (mut state, session, receiver, transport) = lost_begin_fixture(false);
        let (hand, handed) = mpsc::channel();
        let mut steps = Vec::new();
        if pending {
            steps.push(FenceStep::Fail(PersistFenceError::Deadline));
        }
        steps.push(FenceStep::Hand(hand));
        let writer = AcknowledgingWriter::install_with(
            &mut state,
            transport.log.clone(),
            Some(PreparedFence::FailSequence(steps)),
        );
        if pending {
            let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
            with_record(&state, &session, |record| {
                assert!(
                    engram_begin_preparation_pending(record).is_some(),
                    "{label}: the begin is pending"
                );
            });
        }
        let attempt = {
            let (state, session) = (state.clone(), session.clone());
            let injections = writer.injections.clone();
            std::thread::spawn(move || {
                if pending {
                    // Until the retry's attempt reaches the handed fence.
                    for _ in 0..3 {
                        if injections.load(Ordering::SeqCst) == 2 {
                            break;
                        }
                        tick_acknowledged_past_due(&state, &session);
                    }
                } else {
                    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
                }
            })
        };
        let fence = handed
            .recv_timeout(crate::tests::phase_sync::DEADLOCK_GUARD)
            .unwrap_or_else(|_| panic!("{label}: the admission waits on its prepared begin"));
        let (old_prompt, old_grant) = with_record(&state, &session, |record| {
            let head = record.queued_prompts.front().expect("the old head is retained");
            let prepared = head
                .engram_evaluate
                .as_ref()
                .and_then(|evaluate| evaluate.prepared_begin.clone())
                .unwrap_or_else(|| panic!("{label}: its prepared begin awaits the acknowledgement"));
            (head.pending_prompt.id.clone(), prepared.grant_id)
        });
        queue_test_engram_prompt(
            &state,
            &session,
            "Successor prompt",
            QueuedPromptSource::User,
            None,
        );
        state
            .cancel_queued_prompt(&session, &old_prompt)
            .unwrap_or_else(|_| panic!("{label}: Cancel of the waiting head succeeds"));
        let successor = |record: &SessionRecord| {
            let head = record.queued_prompts.front().expect("the successor is queued");
            (
                record.queued_prompts.len(),
                head.pending_prompt.id.clone(),
                head.engram_evaluate.is_some(),
                head.engram_waiting,
                head.engram_interrupted,
                record.engram.dispatch_generation,
                record.orchestrator_auto_dispatch_blocked,
                record.engram.begin_preparation_lineage.clone(),
                record.engram.admission_retry.is_some(),
                record.engram.abort_retry.is_some(),
                record.engram.rebind_required,
                record.engram.uncertain_grant_id.clone(),
            )
        };
        let cards = |record: &SessionRecord| {
            record
                .session
                .messages
                .iter()
                .filter(|message| matches!(message, Message::EngramControl { .. }))
                .count()
        };
        let superseded = with_record(&state, &session, successor);
        let cards_at_supersession = with_record(&state, &session, cards);
        assert_ne!(superseded.1, old_prompt, "{label}: the successor is the head now");
        // The Cancel records the old operation's outcome where its settlement
        // reads it: the grant whose begin it had requested, kept for a session
        // status read on the next bind before any successor is admitted.
        assert!(superseded.10, "{label}: the next bind settles the old grant first");
        assert_eq!(
            superseded.11.as_deref(),
            Some(old_grant.as_str()),
            "{label}: the old grant is kept for its settlement"
        );

        // The acknowledgement arrives now, in time for its own wait.
        fence.finish(Ok(()));
        attempt.join().unwrap();
        assert_eq!(
            with_record(&state, &session, successor),
            superseded,
            "{label}: the successor is untouched, and the old grant's settlement kept"
        );
        assert_eq!(
            with_record(&state, &session, cards),
            cards_at_supersession,
            "{label}: the superseded operation records no card in the successor's session"
        );
        assert!(begin_requests(&transport).is_empty(), "{label}: no Begin sent");

        // Later ticks start nothing for either head.
        tick_acknowledged_past_due(&state, &session);
        tick_acknowledged_past_due(&state, &session);
        assert!(begin_requests(&transport).is_empty(), "{label}: no Begin for the old head");
        assert_eq!(abort_retry::prompts_received(&receiver), 0, "{label}: nothing delivered");

        // The successor's admission settles the old grant first, as never
        // begun: the status read reports it open, Engram refuses its
        // checkpoint because it was never begun, and the fresh bind expires
        // it. The successor is then admitted on a grant of its own and
        // delivered once.
        let sent_before = operations(&transport).len();
        state
            .resume_session_queue(&session)
            .unwrap_or_else(|_| panic!("{label}: Resume admits the successor"));
        writer.finish(&state);
        let after = operations(&transport).split_off(sent_before);
        let first = |wanted: &dyn Fn(&str, &Value) -> bool| {
            after.iter().position(|(operation, request)| wanted(operation.as_str(), request))
        };
        let status = first(&|operation, _| operation == "session_status");
        let settled = first(&|operation, request| {
            operation == "turn_checkpoint" && request["grant_id"] == old_grant.as_str()
        });
        let rebound = first(&|operation, _| operation == "session_bind");
        let evaluated = first(&|operation, _| operation == "turn_evaluate");
        let begun = first(&|operation, _| operation == "turn_begin");
        let order = [status, settled, rebound, evaluated, begun];
        assert!(
            order.iter().all(Option::is_some) && order.is_sorted(),
            "{label}: status, the old grant's checkpoint, bind, evaluate, then begin: {:?}",
            after.iter().map(|(operation, _)| operation.as_str()).collect::<Vec<_>>()
        );
        let begins = begin_requests(&transport);
        assert_eq!(begins.len(), 1, "{label}: only the successor's Begin: {begins:?}");
        assert_ne!(
            begins[0]["grant_id"],
            old_grant.as_str(),
            "{label}: the old grant was never begun"
        );
        with_record(&state, &session, |record| {
            assert_eq!(record.engram.uncertain_grant_id, None, "{label}: the old grant is settled");
            assert!(!record.engram.rebind_required, "{label}: nothing left to settle");
            assert!(record.queued_prompts.is_empty(), "{label}: the successor was admitted");
        });
        assert_eq!(
            abort_retry::prompts_received(&receiver),
            1,
            "{label}: the successor's prompt is delivered once"
        );
    }
}

/// C4: waiting for the host-wide cap consumes the admission budget but starts
/// no attempt. With every retry slot held, the due attempt of a head in
/// prepared-begin recovery is deferred uncharged (its attempts, due time and
/// window unchanged) and nothing is sent. A slot freed before the deadline
/// lets that same pending begin go out, and the prompt is delivered once. A
/// cap still full at the deadline granted no extra time: the recovery ends in
/// the exhausted Resume/Cancel hold, nothing is sent once slots free, and
/// Resume sends that same begin.
#[test]
fn cap_saturation_consumes_the_admission_budget_and_starts_no_attempt() {
    for label in ["freed before the deadline", "full past the deadline"] {
        let (mut state, session, receiver, transport) = lost_begin_fixture(false);
        let writer = AcknowledgingWriter::install_with(
            &mut state,
            transport.log.clone(),
            Some(PreparedFence::FailTimes(PersistFenceError::Deadline, 1)),
        );
        let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
        let (pending, window, scheduled) = with_record(&state, &session, |record| {
            (
                engram_begin_preparation_pending(record)
                    .cloned()
                    .unwrap_or_else(|| panic!("{label}: the begin is pending")),
                record.engram.begin_preparation_lineage.clone().unwrap(),
                record
                    .engram
                    .admission_retry
                    .clone()
                    .unwrap_or_else(|| panic!("{label}: its admission retry")),
            )
        });
        assert!(window.enforced(), "{label}: the budget is enforced");

        let held = (0..ENGRAM_RETRY_MAX_IN_FLIGHT)
            .map(|_| state.engram_retry_slots.try_acquire().expect("a free slot"))
            .collect::<Vec<_>>();
        for _ in 0..3 {
            tick_acknowledged_past_due(&state, &session);
        }
        assert!(begin_requests(&transport).is_empty(), "{label}: nothing sent while the cap is full");
        with_record(&state, &session, |record| {
            let deferred = record
                .engram
                .admission_retry
                .as_ref()
                .unwrap_or_else(|| panic!("{label}: still scheduled"));
            assert_eq!(deferred.attempts, scheduled.attempts, "{label}: no attempt charged");
            assert_eq!(deferred.due_at, scheduled.due_at, "{label}: its due time does not move");
            assert_eq!(
                record.engram.begin_preparation_lineage.as_ref(),
                Some(&window),
                "{label}: the deferral leaves the window as it was"
            );
        });

        if label == "freed before the deadline" {
            drop(held);
            for _ in 0..3 {
                if !begin_requests(&transport).is_empty() {
                    break;
                }
                tick_acknowledged_past_due(&state, &session);
            }
            writer.finish(&state);
            assert_eq!(abort_retry::prompts_received(&receiver), 1, "{label}: delivered once");
            let begins = begin_requests(&transport);
            assert_eq!(begins.len(), 1, "{label}: exactly one begin: {begins:?}");
            assert_eq!(begins[0]["idempotency_key"], pending.idempotency_key.as_str(), "{label}");
            assert_eq!(begins[0]["grant_id"], pending.grant_id.as_str(), "{label}");
            continue;
        }

        // The cap is still full when the window's deadline passes.
        state.engram_abort_retry_tick(past_the_window(&window));
        assert_resumable_hold(
            &state,
            &session,
            &receiver,
            &exhausted_pending_preview(window.attempts),
            label,
        );
        assert_ended_window(&state, &session, &window, label);
        drop(held);
        for _ in 0..2 {
            tick_acknowledged_past_due(&state, &session);
        }
        assert!(begin_requests(&transport).is_empty(), "{label}: nothing sent once slots free");

        state
            .resume_session_queue(&session)
            .unwrap_or_else(|_| panic!("{label}: Resume sends the kept begin"));
        writer.finish(&state);
        assert_eq!(abort_retry::prompts_received(&receiver), 1, "{label}: delivered once");
        let begins = begin_requests(&transport);
        assert_eq!(begins.len(), 1, "{label}: exactly one begin: {begins:?}");
        assert_eq!(begins[0]["idempotency_key"], pending.idempotency_key.as_str(), "{label}");
    }
}

/// A receipted head of a root session whose admission's prepared begin met a
/// stopped writer: held for Resume. The admission never handed the prompt
/// off, so its receipt is back to pending (prompt_delivery.rs).
fn receipted_held_after_stopped_writer() -> (
    AppState,
    String,
    String,
    mpsc::Receiver<CodexRuntimeCommand>,
    Arc<LostBeginReply>,
    AcknowledgingWriter,
) {
    let (mut state, session, receiver, transport) = lost_begin_fixture(false);
    let writer = AcknowledgingWriter::install(
        &mut state,
        transport.log.clone(),
        Some(PersistFenceError::WorkerStopped),
    );
    let id = {
        let mut inner = state.inner.lock().unwrap();
        let prompt = PendingPrompt {
            engram_interrupted: false,
            is_engram_retained: false,
            attachments: Vec::new(),
            id: inner.next_message_id(),
            timestamp: stamp_now(),
            text: "a receipted root prompt".to_owned(),
            expanded_text: None,
            source: None,
        };
        let id = prompt.id.clone();
        let index = inner.find_session_index(&session).unwrap();
        queue_receipted_orchestrator_prompt_on_record(
            inner.session_mut_by_index(index).unwrap(),
            prompt,
            Vec::new(),
        );
        state.commit_locked(&mut inner).unwrap();
        id
    };
    // Its durability acknowledgement, then its write-ahead's, then the
    // admission, whose prepared begin meets the stopped writer.
    assert!(state.request_queued_prompt_durability(&session, &id));
    // The hold and the dispatch's held return are published from the
    // settlement thread, which signals nothing on completion: the wait names
    // the receipt's observed state, so a receipt left handing off is what a
    // failure reports.
    let held = crate::tests::phase_sync::PollGuard::new();
    loop {
        let (is_held, receipt) = with_record(&state, &session, |record| {
            (
                engram_begin_preparation_held_head(record),
                record
                    .prompt_deliveries
                    .get(&id)
                    .map(|delivery| (delivery.disposition, delivery.turn_generation)),
            )
        });
        if is_held
            && receipt.is_some_and(|(disposition, _)| disposition == PromptDeliveryDisposition::Pending)
        {
            break;
        }
        held.wait(format!(
            "the receipted head held by its prepared begin, its receipt back to pending \
             (held: {is_held}; receipt disposition and turn: {receipt:?})"
        ));
    }
    (state, session, id, receiver, transport, writer)
}

/// A receipted head held by its prepared begin: its receipt is pending again
/// (the admission sent nothing), and an explicit Resume, as for any head,
/// releases the begin-preparation hold first and only then begins the
/// receipt's write-ahead for the released head, which is sent once with one
/// begin. (This hold has no automatic recovery for a Stop to end: the Stop of
/// a retained admission is covered in src/tests/prompt_delivery.rs.)
#[test]
fn a_receipted_head_on_a_begin_preparation_hold_is_released_by_resume_and_sent_once() {
    let (state, session, id, receiver, transport, writer) = receipted_held_after_stopped_writer();
    with_record(&state, &session, |record| {
        let delivery = &record.prompt_deliveries[&id];
        assert_eq!(delivery.turn_generation, None, "never handed off");
        assert!(delivery.resume.is_none());
    });
    assert!(begin_requests(&transport).is_empty(), "nothing sent before Resume");

    state
        .resume_local_session_queue(&session, None)
        .unwrap_or_else(|error| panic!("Resume is accepted: {}", error.message));
    let promoted = crate::tests::phase_sync::PollGuard::new();
    while !with_record(&state, &session, |record| {
        record
            .prompt_deliveries
            .get(&id)
            .is_some_and(|delivery| delivery.turn_generation.is_some())
            && record.queued_prompts.is_empty()
    }) {
        promoted.wait("the released receipted head is promoted and handed off");
    }
    writer.finish(&state);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered once");
    assert_eq!(begin_requests(&transport).len(), 1, "exactly one begin");
}
