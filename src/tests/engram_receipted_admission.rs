// Owns the tests of the ownership of a receipted queue head's acknowledged,
// never-promoted write-ahead attempt on an Engram-gated root session: while
// another drain admits the head off-lock, the acknowledgement leaves the
// attempt to that admission; the admission either promotes it, once, or, on
// every exit without a promotion (a Stop or a pause during its evaluation, a
// retry, an unwind), returns that exact attempt to pending durably, keeping
// the queue's holds; a stale guard, a successor attempt and a promoted or
// settled receipt are never touched. Does not own the receipt's other paths
// (src/tests/prompt_delivery.rs) or the ordinary root admission
// (src/tests/engram_root_dispatch.rs). New module, a child of the root
// dispatch tests whose Engram helpers it uses.
use super::*;
use crate::tests::prompt_delivery::{DeliveryWriter, HeldFence};

/// A gated root Codex session, as `root_fixture` builds one, with a
/// transport that answers `steps` in order and may hold any of them.
pub(super) fn gated_root_fixture(
    steps: impl IntoIterator<Item = GatedEngramControlStep>,
) -> (AppState, String, mpsc::Receiver<CodexRuntimeCommand>) {
    let (state, receiver) = test_app_state_with_delegation_codex_runtime("receipted-admission");
    // The clock is chosen before Engram is enabled or any target exists.
    let _ = AppState::select_test_scripted_engram_budget_clock(&state);
    let root = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("receipted-admission-project");
    fs::create_dir_all(&root).unwrap();
    let project = create_test_project(&state, &root, "Receipted admission");
    let session = create_test_project_session(&state, Agent::Codex, &project, &root);
    enable_test_project_engram(&state, &project, &root);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram.context_nudge_pending = false;
    }
    state.install_control_test_transport(GatedEngramControlTransport::new(steps));
    (state, session, receiver)
}

/// Makes the head an automatic retry parked a receipted one, as such a head
/// is once its hand-off was held: its receipt pending, with no Resume.
/// Returns its prompt id.
pub(super) fn receipt_the_parked_head(state: &AppState, session: &str) -> String {
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(session).unwrap();
    let record = inner.session_mut_by_index(index).unwrap();
    assert!(record.orchestrator_auto_dispatch_blocked, "the retry parked the queue");
    let id = record.queued_prompts.front().expect("the parked head").pending_prompt.id.clone();
    let sequence = next_prompt_delivery_sequence(record);
    record.prompt_deliveries.insert(
        id.clone(),
        PromptDelivery {
            federation_operation: None,
            federation_handoff_started: false,
            disposition: PromptDeliveryDisposition::Pending,
            attempt: 1,
            write_ahead: Uuid::new_v4().to_string(),
            sequence,
            message_id: None,
            turn_generation: None,
            attempt_key: None,
            reason: None,
            entry: None,
            write_ahead_acknowledged: false,
            resume: None,
            runtime_write: PromptRuntimeWrite::Untracked,
        },
    );
    sync_prompt_delivery_view(record);
    state.commit_locked(&mut inner).unwrap();
    id
}

/// The session's automatic retry lineage: every retry record and the
/// begin-preparation lineage, with their due times and spent attempts.
pub(super) fn retry_lineage(state: &AppState, session: &str) -> String {
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    format!(
        "{:?} {:?} {:?} {:?}",
        record.engram.bind_retry,
        record.engram.admission_retry,
        record.engram.abort_retry,
        record.engram.begin_preparation_lineage
    )
}

/// The park of a due automatic retry on a receipted head that only an
/// explicit Resume may send: no attempt begun or charged, the lineage kept as
/// it was, the receipt still pending, and the session saying it waits for
/// Resume.
pub(super) fn assert_parked_for_resume(state: &AppState, session: &str, id: &str, lineage_before: &str) {
    assert_eq!(retry_lineage(state, session), lineage_before, "no attempt is charged or rescheduled");
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    assert_eq!(record.prompt_deliveries[id].disposition, PromptDeliveryDisposition::Pending);
    assert!(record.orchestrator_auto_dispatch_blocked, "the pause stays");
    assert!(
        record.session.preview.contains("waiting for Resume"),
        "the session says it waits for Resume, not for an automatic retry: {}",
        record.session.preview
    );
}

fn receipt(state: &AppState, session: &str, prompt_id: &str) -> PromptDelivery {
    let inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(session).unwrap();
    inner.sessions[index].prompt_deliveries[prompt_id].clone()
}

fn with_session<T>(state: &AppState, session: &str, change: impl FnOnce(&mut SessionRecord) -> T) -> T {
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(session).unwrap();
    change(inner.session_mut_by_index(index).unwrap())
}

/// Enqueues a receipted prompt and takes it through its held durability
/// acknowledgement to its write-ahead fence, which it returns unresolved,
/// with the prompt id and the attempt's nonce.
fn receipted_head_written_ahead(
    state: &AppState,
    session: &str,
    writer: &DeliveryWriter,
    text: &str,
) -> (String, Box<PersistFence>, String) {
    let id = {
        let mut inner = state.inner.lock().unwrap();
        let prompt = PendingPrompt {
            engram_interrupted: false,
            is_engram_retained: false,
            attachments: Vec::new(),
            id: inner.next_message_id(),
            timestamp: stamp_now(),
            text: text.to_owned(),
            expanded_text: None,
            source: None,
        };
        let id = prompt.id.clone();
        let index = inner.find_session_index(session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        queue_receipted_orchestrator_prompt_on_record(record, prompt, Vec::new());
        state.commit_locked(&mut inner).unwrap();
        id
    };
    assert!(state.request_queued_prompt_durability(session, &id));
    writer.take(HeldFence::Durability).finish(Ok(()));
    let write_ahead = writer.take(HeldFence::WriteAhead);
    let nonce = receipt(state, session, &id).write_ahead;
    assert_eq!(receipt(state, session, &id).disposition, PromptDeliveryDisposition::HandingOff);
    (id, write_ahead, nonce)
}

/// The attempt the head was left in: acknowledged, not promoted, same nonce.
fn assert_left_to_the_admission(state: &AppState, session: &str, id: &str, nonce: &str) {
    let delivery = receipt(state, session, id);
    assert_eq!(
        delivery.disposition,
        PromptDeliveryDisposition::HandingOff,
        "the head is left to the drain admitting it: {:?}",
        delivery.reason
    );
    assert!(delivery.write_ahead_acknowledged, "the acknowledged attempt is kept");
    assert_eq!(delivery.turn_generation, None, "not promoted");
    assert_eq!(delivery.write_ahead, nonce, "no superseding write");
}

/// The exact attempt returned to pending, durably and once: a fresh nonce,
/// no acknowledgement, no Resume, the head still queued, nothing sent, and
/// a later cleanup of the old nonce changes nothing.
fn assert_returned_to_pending_once(
    state: &AppState,
    session: &str,
    id: &str,
    old_nonce: &str,
    receiver: &mpsc::Receiver<CodexRuntimeCommand>,
) {
    let delivery = receipt(state, session, id);
    assert_eq!(
        delivery.disposition,
        PromptDeliveryDisposition::Pending,
        "the never-sent attempt returns to pending: {:?}",
        delivery.reason
    );
    assert_eq!(delivery.turn_generation, None);
    assert!(!delivery.write_ahead_acknowledged);
    assert!(delivery.resume.is_none(), "no Resume is inherited");
    assert_ne!(delivery.write_ahead, old_nonce, "a superseding write");
    assert!(receiver.try_recv().is_err(), "nothing is sent");
    let fresh = delivery.write_ahead.clone();
    state.return_unpromoted_write_ahead_to_pending(session, id, old_nonce);
    assert_eq!(receipt(state, session, id).write_ahead, fresh, "reset exactly once");
    let reloaded = {
        let inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(session).unwrap();
        PersistedSessionRecord::from_record(&inner.sessions[index])
            .into_record()
            .expect("the row loads")
    };
    assert_eq!(reloaded.prompt_deliveries[id].disposition, PromptDeliveryDisposition::Pending);
    assert_eq!(reloaded.queued_prompts.front().unwrap().pending_prompt.id, id);
}

/// A receipted head whose write-ahead acknowledgement was settled while
/// another drain held in its off-lock Engram evaluation of that head.
struct AcknowledgedWhileAdmitting {
    state: AppState,
    session: String,
    receiver: mpsc::Receiver<CodexRuntimeCommand>,
    id: String,
    nonce: String,
    admitting: std::thread::JoinHandle<Result<Option<TurnDispatch>>>,
    evaluate_gate: EngramControlGate,
    _write_ahead: Box<PersistFence>,
    _writer: DeliveryWriter,
}

/// The acknowledgement of a head's write-ahead record releases the lock
/// before its own drain runs. Another drain can pass the receipt gate in that
/// window and evaluate the head with Engram off-lock (it answers with
/// `evaluation`, then `after`). The acknowledgement's drain then finds that
/// admission in flight and leaves it a wake, and its cleanup leaves the
/// head, acknowledged, to the admitting drain, which is still held in its
/// evaluation when this returns.
fn acknowledged_while_another_drain_admits(
    text: &str,
    evaluation: ScriptedEngramControlResponse,
    after: Vec<GatedEngramControlStep>,
) -> AcknowledgedWhileAdmitting {
    let (evaluate, evaluate_gate) = gated_engram_step("turn_evaluate", evaluation);
    let mut steps = vec![
        immediate_engram_step("session_bind", bind_reply("receipted-token")),
        evaluate,
    ];
    steps.extend(after);
    let (mut state, session, receiver) = gated_root_fixture(steps);
    let writer = DeliveryWriter::attach(&mut state);
    let (id, write_ahead, nonce) = receipted_head_written_ahead(&state, &session, &writer, text);

    // The acknowledgement's first half, under the lock, as the writer's
    // settlement does it (`settle_prompt_delivery_write_ahead`).
    with_session(&state, &session, |record| {
        let delivery = record.prompt_deliveries.get_mut(&id).unwrap();
        delivery.write_ahead_acknowledged = true;
        delivery.resume = None;
    });

    // Another drain passes the receipt gate and holds in its evaluation.
    let admitting = {
        let state = state.clone();
        let session = session.clone();
        std::thread::spawn(move || state.dispatch_next_queued_turn(&session, false))
    };
    evaluate_gate.wait();

    // The acknowledgement's second half: its drain, then its cleanup.
    state.dispatch_after_write_ahead(&session, &id, None);
    state.return_unpromoted_write_ahead_to_pending(&session, &id, &nonce);
    assert_left_to_the_admission(&state, &session, &id, &nonce);
    assert!(receiver.try_recv().is_err(), "nothing is sent before the admission");

    AcknowledgedWhileAdmitting {
        state,
        session,
        receiver,
        id,
        nonce,
        admitting,
        evaluate_gate,
        _write_ahead: write_ahead,
        _writer: writer,
    }
}

fn granted() -> (ScriptedEngramControlResponse, Vec<GatedEngramControlStep>) {
    (
        grant_reply("receipted-grant"),
        vec![immediate_engram_step("turn_begin", begin_reply("receipted-grant"))],
    )
}

/// Were the acknowledgement's cleanup to return the head to pending here, the
/// admitting drain's promotion would refuse the head and abandon its
/// evaluation, and the wake it was left would no longer match its owner, so
/// the idle queue would stay stranded. The admitting drain owns the head: it
/// promotes and sends it, once, and a cleanup of its nonce changes nothing.
#[test]
fn an_acknowledgement_while_another_drain_admits_the_head_leaves_the_head_to_that_drain() {
    let (evaluation, after) = granted();
    let scenario = acknowledged_while_another_drain_admits("admitted by another drain", evaluation, after);
    scenario.evaluate_gate.release();
    let dispatch = scenario
        .admitting
        .join()
        .expect("the admitting drain does not panic")
        .expect("the admitting drain succeeds")
        .expect("the admitting drain promotes the acknowledged head");
    deliver_turn_dispatch(&scenario.state, dispatch).expect("the promoted head reaches the runtime");
    assert!(matches!(scenario.receiver.try_recv(), Ok(CodexRuntimeCommand::Prompt { .. })));
    assert!(scenario.receiver.try_recv().is_err(), "sent once");
    let delivery = receipt(&scenario.state, &scenario.session, &scenario.id);
    assert!(delivery.turn_generation.is_some(), "promoted by the admitting drain");
    scenario
        .state
        .return_unpromoted_write_ahead_to_pending(&scenario.session, &scenario.id, &scenario.nonce);
    let after_cleanup = receipt(&scenario.state, &scenario.session, &scenario.id);
    assert_eq!(after_cleanup.disposition, delivery.disposition, "a promoted attempt is not demoted");
    assert_eq!(after_cleanup.turn_generation, delivery.turn_generation);
}

/// A Stop of the admission while its evaluation is in flight ends it without
/// a promotion, and moves the dispatch generation on: the admitting drain, as
/// it gives the head up, still returns that exact never-sent attempt to
/// pending, once and durably, and the Stop's hold stays.
#[test]
fn a_stop_of_the_admitting_drain_returns_its_unsent_attempt_to_pending() {
    let (evaluation, after) = granted();
    let scenario = acknowledged_while_another_drain_admits("stopped while admitted", evaluation, after);
    assert!(
        scenario
            .state
            .stop_waiting_engram_admission(&scenario.session)
            .expect("the Stop is accepted"),
        "the Stop ends the admission in flight"
    );
    scenario.evaluate_gate.release();
    let promoted = scenario
        .admitting
        .join()
        .expect("the admitting drain does not panic")
        .expect("the admitting drain succeeds");
    assert!(promoted.is_none(), "the stopped admission promotes nothing");
    assert_returned_to_pending_once(
        &scenario.state,
        &scenario.session,
        &scenario.id,
        &scenario.nonce,
        &scenario.receiver,
    );
    assert!(
        with_session(&scenario.state, &scenario.session, |record| record.orchestrator_auto_dispatch_blocked),
        "the Stop's hold stays"
    );
}

/// A pause of the queue during the evaluation makes the admitting drain's
/// revalidation refuse the head: it gives the head up without a promotion
/// and returns the never-sent attempt to pending, and the pause stays.
#[test]
fn a_pause_during_the_admission_returns_its_unsent_attempt_to_pending_and_keeps_the_pause() {
    let (evaluation, after) = granted();
    let scenario = acknowledged_while_another_drain_admits("paused while admitted", evaluation, after);
    {
        let mut inner = scenario.state.inner.lock().unwrap();
        let index = inner.find_session_index(&scenario.session).unwrap();
        inner.session_mut_by_index(index).unwrap().set_auto_dispatch_blocked(true);
        scenario.state.commit_locked(&mut inner).unwrap();
    }
    scenario.evaluate_gate.release();
    let promoted = scenario
        .admitting
        .join()
        .expect("the admitting drain does not panic")
        .expect("the admitting drain succeeds");
    assert!(promoted.is_none(), "a paused queue promotes nothing");
    assert_returned_to_pending_once(
        &scenario.state,
        &scenario.session,
        &scenario.id,
        &scenario.nonce,
        &scenario.receiver,
    );
    assert!(
        with_session(&scenario.state, &scenario.session, |record| record.orchestrator_auto_dispatch_blocked),
        "the pause stays"
    );
}

/// An Engram refusal or Defer is decided at the admitting drain's promotion,
/// so ownership passes to the promoted turn's outcome path: the prompt is
/// never sent, and its receipt is not left handing off unpromoted.
#[test]
fn a_refused_or_deferred_admission_never_sends_and_never_strands_the_attempt() {
    for (label, evaluation) in [
        ("refused", evaluation_refusal_reply("policy_denied")),
        ("deferred", defer_reply("not_ready")),
    ] {
        let scenario = acknowledged_while_another_drain_admits(label, evaluation, Vec::new());
        scenario.evaluate_gate.release();
        let promoted = scenario
            .admitting
            .join()
            .expect("the admitting drain does not panic")
            .expect("the admitting drain succeeds");
        if let Some(dispatch) = promoted {
            let _ = deliver_turn_dispatch(&scenario.state, dispatch);
        }
        assert!(scenario.receiver.try_recv().is_err(), "{label}: nothing is sent");
        let delivery = receipt(&scenario.state, &scenario.session, &scenario.id);
        assert!(
            !(delivery.disposition == PromptDeliveryDisposition::HandingOff
                && delivery.turn_generation.is_none()),
            "{label}: the attempt is promoted or settled, never stranded: {delivery:?}"
        );
    }
}

/// An acknowledged, never-promoted attempt whose admission guard is held by
/// hand: the acknowledgement settled while that guard was in flight, so the
/// attempt was left to it. The transport answers nothing: any Engram request
/// fails the test.
struct HeldGuard {
    state: AppState,
    session: String,
    receiver: mpsc::Receiver<CodexRuntimeCommand>,
    id: String,
    nonce: String,
    wake: Arc<std::sync::atomic::AtomicBool>,
    owner: Option<EngramQueuedAdmissionOwner>,
    writer: DeliveryWriter,
    _write_ahead: Box<PersistFence>,
}

impl HeldGuard {
    fn new(text: &str) -> Self {
        let (mut state, session, receiver) = gated_root_fixture([]);
        let writer = DeliveryWriter::attach(&mut state);
        let (id, write_ahead, nonce) = receipted_head_written_ahead(&state, &session, &writer, text);
        // As `with_queued_engram_admission` takes a guard for the head.
        let (wake, owner) = with_session(&state, &session, |record| {
            let wake = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let owner = EngramQueuedAdmissionOwner::capture(record);
            record.engram.admission_in_progress = Some(wake.clone());
            record.engram.admission_prompt_id = owner.as_ref().map(|owner| owner.prompt_id.clone());
            (wake, owner)
        });
        state.settle_prompt_delivery_write_ahead(&session, &id, &nonce, Ok(()));
        assert_left_to_the_admission(&state, &session, &id, &nonce);
        assert!(
            wake.load(std::sync::atomic::Ordering::Relaxed),
            "the acknowledgement's drain left the admission a wake"
        );
        Self {
            state,
            session,
            receiver,
            id,
            nonce,
            wake,
            owner,
            writer,
            _write_ahead: write_ahead,
        }
    }

    fn guard(&self) -> EngramAdmissionGuard<'_> {
        EngramAdmissionGuard {
            state: &self.state,
            session_id: &self.session,
            wake: self.wake.clone(),
            owner: self.owner.clone(),
        }
    }
}

/// A guard disposed of without a normal return (the admission unwinds)
/// settles its exact attempt as its normal release does: pending, once.
#[test]
fn an_admission_guard_dropped_by_an_unwind_returns_its_unsent_attempt_to_pending() {
    let held = HeldGuard::new("unwound while admitted");
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = held.guard();
        panic!("the admission unwinds");
    }));
    assert!(unwound.is_err());
    assert_returned_to_pending_once(&held.state, &held.session, &held.id, &held.nonce, &held.receiver);
}

/// A release that will retry still relinquishes the attempt first: the
/// attempt returns to pending before the next iteration, which begins a
/// fresh write-ahead for it, so no ownership gap leaves it stranded.
#[test]
fn an_admission_release_that_retries_settles_its_attempt_before_the_next_iteration() {
    let held = HeldGuard::new("retried while admitted");
    let guard = held.guard();
    assert!(guard.release(), "the wake, the owner and an idle queue make this release retry");
    drop(guard);
    let pending = receipt(&held.state, &held.session, &held.id);
    assert_eq!(
        pending.disposition,
        PromptDeliveryDisposition::Pending,
        "settled before relinquishing: {:?}",
        pending.reason
    );
    assert_ne!(pending.write_ahead, held.nonce);

    // The retry's next iteration begins a fresh attempt for the head.
    assert!(held.state.dispatch_next_queued_turn(&held.session, false).unwrap().is_none());
    let next_write_ahead = held.writer.take(HeldFence::WriteAhead);
    let next = receipt(&held.state, &held.session, &held.id);
    assert_eq!(next.disposition, PromptDeliveryDisposition::HandingOff);
    assert!(!next.write_ahead_acknowledged);
    assert_ne!(next.write_ahead, held.nonce);
    assert!(held.receiver.try_recv().is_err(), "nothing is sent before its acknowledgement");

    // A save that fails is no acknowledgement: the attempt returns to
    // pending again, and still nothing is sent.
    let fresh = next.write_ahead.clone();
    held.state.settle_prompt_delivery_write_ahead(
        &held.session,
        &held.id,
        &fresh,
        Err(PersistFenceError::WriteFailed("the disk is full".to_owned())),
    );
    let failed = receipt(&held.state, &held.session, &held.id);
    assert_eq!(failed.disposition, PromptDeliveryDisposition::Pending);
    assert_ne!(failed.write_ahead, fresh);
    assert!(held.receiver.try_recv().is_err(), "no send escapes a failed save");
    drop(next_write_ahead);
}

/// The reset to pending is a settlement in memory until the writer saves it,
/// and the row on disk is what a restart reads. Before it is saved (this
/// writer saves nothing, as one failing to would), a crash leaves the older
/// acknowledged hand-off on disk, which a restart conservatively settles
/// ambiguous and never replays; once the reset row is saved, a restart keeps
/// the prompt pending and queued. The unsaved reset stays identifiable to
/// the writer (the session's mutation stamp moved past it), and no send
/// escapes before a fresh write-ahead is acknowledged.
#[test]
fn an_unsaved_reset_to_pending_is_told_apart_from_a_saved_one_and_lets_no_send_escape() {
    let held = HeldGuard::new("reset, then the save fails");
    let row = |state: &AppState| {
        let inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&held.session).unwrap();
        (
            PersistedSessionRecord::from_record(&inner.sessions[index]),
            inner.sessions[index].mutation_stamp,
        )
    };
    let (row_before_reset, stamp_before_reset) = row(&held.state);

    drop(held.guard());

    // A crash before the reset is saved: the row on disk is the older one.
    let crashed = row_before_reset.into_record().expect("the older row loads");
    assert_eq!(crashed.prompt_deliveries[&held.id].disposition, PromptDeliveryDisposition::Ambiguous);
    assert!(
        !crashed.queued_prompts.iter().any(|queued| queued.pending_prompt.id == held.id),
        "a crash before the save never replays it"
    );

    // The reset row, once saved, reloads pending and queued.
    let (row_after_reset, stamp_after_reset) = row(&held.state);
    assert!(
        stamp_after_reset > stamp_before_reset,
        "the unsaved reset is a change the writer has yet to save"
    );
    let saved = row_after_reset.into_record().expect("the reset row loads");
    assert_eq!(saved.prompt_deliveries[&held.id].disposition, PromptDeliveryDisposition::Pending);
    assert_eq!(saved.queued_prompts.front().unwrap().pending_prompt.id, held.id);

    // Nothing is sent before a fresh write-ahead is acknowledged, and a
    // failed save of that write-ahead sends nothing either.
    assert!(held.state.dispatch_next_queued_turn(&held.session, false).unwrap().is_none());
    let fresh_write_ahead = held.writer.take(HeldFence::WriteAhead);
    let fresh = receipt(&held.state, &held.session, &held.id).write_ahead;
    held.state.settle_prompt_delivery_write_ahead(
        &held.session,
        &held.id,
        &fresh,
        Err(PersistFenceError::WriteFailed("the disk is full".to_owned())),
    );
    assert_eq!(receipt(&held.state, &held.session, &held.id).disposition, PromptDeliveryDisposition::Pending);
    assert!(held.receiver.try_recv().is_err(), "no send escapes a failed save");
    drop(fresh_write_ahead);
}

/// A stale guard, whose admission a successor took over with a new attempt,
/// touches neither the successor's admission nor its attempt.
#[test]
fn a_stale_admission_guard_leaves_a_successor_attempt_untouched() {
    let held = HeldGuard::new("taken over by a successor");
    let successor = Arc::new(std::sync::atomic::AtomicBool::new(false));
    with_session(&held.state, &held.session, |record| {
        record.engram.admission_in_progress = Some(successor.clone());
        record.prompt_deliveries.get_mut(&held.id).unwrap().write_ahead = "successor-nonce".to_owned();
    });
    drop(held.guard());
    let (still_successor, delivery) = with_session(&held.state, &held.session, |record| {
        (
            record
                .engram
                .admission_in_progress
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(active, &successor)),
            record.prompt_deliveries[&held.id].clone(),
        )
    });
    assert!(still_successor, "the successor's admission stays");
    assert_eq!(delivery.write_ahead, "successor-nonce");
    assert_eq!(delivery.disposition, PromptDeliveryDisposition::HandingOff);
    assert!(delivery.write_ahead_acknowledged);
}

/// A guard's release never demotes an attempt that was promoted, or a
/// receipt already settled, since it gave the attempt up.
#[test]
fn an_admission_release_never_demotes_a_promoted_or_settled_receipt() {
    for settled in [false, true] {
        let held = HeldGuard::new(if settled { "accepted" } else { "promoted" });
        with_session(&held.state, &held.session, |record| {
            let turn = record.active_turn_generation.wrapping_add(1).max(1);
            let delivery = record.prompt_deliveries.get_mut(&held.id).unwrap();
            delivery.turn_generation = Some(turn);
            if settled {
                delivery.disposition = PromptDeliveryDisposition::Accepted;
            }
        });
        let before = receipt(&held.state, &held.session, &held.id);
        drop(held.guard());
        let after = receipt(&held.state, &held.session, &held.id);
        assert_eq!(after.disposition, before.disposition, "settled: {settled}");
        assert_eq!(after.turn_generation, before.turn_generation, "settled: {settled}");
        assert_eq!(after.write_ahead, before.write_ahead, "settled: {settled}");
    }
}
