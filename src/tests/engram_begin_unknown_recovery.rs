//! The prepared turn_begin on the production admission path: its durable
//! acknowledgement before the send, the send withheld without one, and the
//! diagnostics a lost begin reply leaves on the retained hold. Does not own the
//! evaluate retry schedule. Registered by engram_admission_retry.rs, whose
//! clock and tick helpers it uses.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[path = "engram_begin_preparation_retry.rs"]
mod begin_preparation_retry;

#[path = "engram_retry_hold_producers.rs"]
mod retry_hold_producers;

/// One durability or wire boundary, in the order the writer acknowledged a
/// fence and the producer received a begin.
#[derive(Clone, Debug)]
enum Boundary {
    Acknowledged(Value),
    Sent(Value),
}

#[derive(Default)]
struct BoundaryLog {
    boundaries: Mutex<Vec<Boundary>>,
    /// A barrier the writer crosses before it resolves the next dispatch
    /// admission fence on the scripted clock: the clock moves by this much
    /// first.
    fence_barrier: Mutex<Option<(EngramBudgetClock, Duration)>>,
    /// How many such barriers the writer crossed.
    fence_barriers_crossed: std::sync::atomic::AtomicUsize,
}

impl BoundaryLog {
    fn push(&self, boundary: Boundary) {
        self.boundaries.lock().unwrap().push(boundary);
    }

    fn snapshot(&self) -> Vec<Boundary> {
        self.boundaries.lock().unwrap().clone()
    }

    /// Arms a barrier that moves `clock` by `by` before the writer resolves
    /// its next fence.
    fn arm_fence_barrier(&self, clock: EngramBudgetClock, by: Duration) {
        *self.fence_barrier.lock().unwrap() = Some((clock, by));
    }

    /// Crosses an armed barrier, if any.
    fn cross_fence_barrier(&self) {
        let barrier = self.fence_barrier.lock().unwrap().take();
        if let Some((clock, by)) = barrier {
            clock.advance(by);
            self.fence_barriers_crossed.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn fence_barriers_crossed(&self) -> usize {
        self.fence_barriers_crossed.load(Ordering::SeqCst)
    }
}

/// Drops one turn_begin reply after the stateful producer applied it, so an
/// exact replay of the same key can return the retained receipt. Models reply
/// loss on the control pipe, not a real sidecar or a real Engram store.
struct LostBeginReply {
    producer: Arc<StatefulEngramControlTransport>,
    lose_next_begin_reply: AtomicBool,
    /// Answers the next turn_begin, before the producer sees it, with
    /// Engram's structured `control_unavailable` error, which disables
    /// control for the session.
    fail_next_begin_with_control_unavailable: AtomicBool,
    /// How many of the next session_bind requests fail at the transport
    /// before reaching the producer, as an outage would.
    fail_binds: std::sync::atomic::AtomicUsize,
    log: Arc<BoundaryLog>,
    /// When the host observes the next applied Begin reply, on the scripted
    /// budget clock, relative to the end of that call's bound: the clock
    /// moves by that much before the reply returns.
    begin_reply_observed: Mutex<Option<(EngramBudgetClock, BeginReplyTiming)>>,
    /// Answers the next applied Begin with a `grant_scope_mismatch` refusal
    /// instead of its receipt, as a refusal that leaves the grant applied.
    refuse_next_begin_as_scope_mismatch: AtomicBool,
    /// Answers the next session status with the begun grant named begun, the
    /// answer that settles a scope-mismatch refusal as applied, returned once
    /// the scripted clock has reached this instant (the fixture's per-call
    /// bound is shorter than a budget, so the read's own bound cannot place
    /// it relative to the deadline).
    status_reports_begun: Mutex<Option<(EngramBudgetClock, std::time::Instant)>>,
}

/// When the host first observes an applied Begin reply, relative to the end
/// of that call's bound (which a head's enforced budget clips to its deadline).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BeginReplyTiming {
    /// One millisecond before it; then the writer crosses a barrier past it
    /// before resolving its next fence, which is after the receipt and
    /// before the prompt's delivery.
    BeforeThenBarrierPast,
    /// Exactly at it.
    At,
    /// One millisecond after it.
    After,
}

impl BeginReplyTiming {
    /// How far the scripted clock moves, for a call bound `timeout`.
    fn advance(self, timeout: Duration) -> Duration {
        let one = Duration::from_millis(1);
        match self {
            Self::BeforeThenBarrierPast => timeout.saturating_sub(one),
            Self::At => timeout,
            Self::After => timeout + one,
        }
    }
}

const BIND_OUTAGE: &str = "control worker unavailable before session_bind";

impl EngramControlTransport for LostBeginReply {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        if matches!(request, EngramControlRequest::SessionBind { .. })
            && self
                .fail_binds
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |left| left.checked_sub(1))
                .is_ok()
        {
            return Err(EngramTransportError::transport(BIND_OUTAGE));
        }
        if matches!(request, EngramControlRequest::TurnBegin { .. }) {
            self.log
                .push(Boundary::Sent(serde_json::to_value(request).unwrap()));
            if self
                .fail_next_begin_with_control_unavailable
                .swap(false, Ordering::SeqCst)
            {
                return Err(EngramTransportError::remote(EngramControlErrorBody {
                    code: "control_unavailable".to_owned(),
                    message: "scripted fatal control-unavailable response".to_owned(),
                }));
            }
        }
        let mut reply = self.producer.request(connection, request, timeout)?;
        if matches!(request, EngramControlRequest::TurnBegin { .. })
            && reply["decision"] == "begin"
            && let Some((clock, timing)) = self.begin_reply_observed.lock().unwrap().take()
        {
            clock.advance(timing.advance(timeout));
            if timing == BeginReplyTiming::BeforeThenBarrierPast {
                self.log.arm_fence_barrier(clock, Duration::from_millis(2));
            }
        }
        if matches!(request, EngramControlRequest::TurnBegin { .. })
            && reply["decision"] == "begin"
            && self.refuse_next_begin_as_scope_mismatch.swap(false, Ordering::SeqCst)
        {
            return Ok(json!({"decision": "refuse", "code": "grant_scope_mismatch"}));
        }
        if matches!(request, EngramControlRequest::SessionStatus { .. })
            && let Some((clock, at)) = self.status_reports_begun.lock().unwrap().take()
        {
            reply["open_grant_state"] = json!("begun");
            clock.advance(at.saturating_duration_since(clock.now()));
        }
        if matches!(request, EngramControlRequest::TurnBegin { .. })
            && reply["decision"] == "begin"
            && self.lose_next_begin_reply.swap(false, Ordering::SeqCst)
        {
            return Err(EngramTransportError::transport(LOST_BEGIN_REPLY));
        }
        Ok(reply)
    }

    fn shutdown_session(&self, session_id: &str) {
        self.producer.shutdown_session(session_id);
    }
}

const LOST_BEGIN_REPLY: &str =
    "control worker reached EOF after applying turn_begin; reply frame missing";

fn lost_begin_fixture(
    lose_reply: bool,
) -> (AppState, String, mpsc::Receiver<CodexRuntimeCommand>, Arc<LostBeginReply>) {
    let (state, session, receiver, _) = root_fixture([]);
    let transport = Arc::new(LostBeginReply {
        producer: StatefulEngramControlTransport::new(),
        lose_next_begin_reply: AtomicBool::new(lose_reply),
        fail_next_begin_with_control_unavailable: AtomicBool::new(false),
        fail_binds:std::sync::atomic::AtomicUsize::new(0),
        log: Arc::default(),
        begin_reply_observed: Mutex::new(None),
        refuse_next_begin_as_scope_mismatch: AtomicBool::new(false),
        status_reports_begun: Mutex::new(None),
    });
    state.install_control_test_transport(transport.clone());
    (state, session, receiver, transport)
}

fn begin_requests(transport: &LostBeginReply) -> Vec<Value> {
    transport
        .producer
        .requests()
        .into_iter()
        .filter(|recorded| recorded.request["operation"] == "turn_begin")
        .map(|recorded| recorded.request)
        .collect()
}

/// The prepared-begin record a fence's admission content carries for the
/// queued head, when it carries one.
fn prepared_begin(content: &Value) -> Option<&Value> {
    content["queue"]["engram_evaluate"]
        .get("prepared_begin")
        .filter(|prepared| prepared.is_object())
}

/// What the writer does with the first admission fence carrying a prepared
/// begin.
enum PreparedFence {
    /// Resolves it with this failure.
    Fail(PersistFenceError),
    /// Resolves each of the first `times` such fences with this failure.
    FailTimes(PersistFenceError, usize),
    /// Acknowledges it, but only after the budget. Once the admission waits
    /// on the scripted clock, the fence's success is recorded without waking
    /// the waiter, then the clock moves to the fence's deadline, which wakes
    /// it: the waiter can only read that success after the budget is spent.
    AcknowledgeLate(EngramBudgetClock),
    /// Lets the admission's attempt wait run out first: once the admission
    /// waits on the scripted clock, the clock moves past that attempt's
    /// 20 second budget, and only then does the fence succeed, well inside
    /// the overall admission budget.
    AcknowledgeAfterAttempt(EngramBudgetClock),
    /// The first such fence misses its deadline; the second (the pending
    /// begin's re-acknowledgement) is acknowledged late, as `AcknowledgeLate`.
    DeadlineThenAcknowledgeLate(EngramBudgetClock),
    /// Resolves each of the first admission fences on the scripted clock that
    /// carry a prepared begin with the next of these steps, in order.
    FailSequence(Vec<FenceStep>),
}

/// One scripted outcome of `PreparedFence::FailSequence`.
#[derive(Clone)]
enum FenceStep {
    /// Resolves the fence with this failure.
    Fail(PersistFenceError),
    /// Acknowledges it in time.
    Acknowledge,
    /// Acknowledges it at its deadline, as `PreparedFence::AcknowledgeLate`.
    AcknowledgeLate(EngramBudgetClock),
    /// Hands the fence, unresolved, to the test, which resolves it after
    /// acting while the admission waits; the writer goes on serving others.
    Hand(mpsc::Sender<Box<PersistFence>>),
}

/// Acknowledges `fence` exactly at its deadline on the scripted `clock`: the
/// success is recorded without waking the waiter, then the clock moves to the
/// deadline, which wakes it, so the waiter reads that success only once the
/// budget is spent.
fn acknowledge_at_deadline(
    fence: &PersistFence,
    clock: &EngramBudgetClock,
    content: Option<Value>,
    log: &BoundaryLog,
) {
    clock.wait_for_scripted_waiter();
    let deadline = fence.completion.deadline;
    if let Some(content) = content {
        log.push(Boundary::Acknowledged(content));
    }
    assert!(fence.completion.resolve_at(Ok(()), clock.now()));
    clock.advance(deadline.saturating_duration_since(clock.now()));
    let hook = fence.on_acknowledged.lock().ok().and_then(|mut slot| slot.take());
    if let Some(hook) = hook {
        hook();
    }
}

/// A durable writer that acknowledges every fence without writing, logging
/// each admission fence it acknowledges. With `fail_prepared`, it resolves the
/// first admission fence carrying a prepared begin with that result instead.
struct AcknowledgingWriter {
    stop: Arc<AtomicBool>,
    injected: Arc<AtomicBool>,
    /// How many prepared-begin fences the writer scripted.
    injections: Arc<std::sync::atomic::AtomicUsize>,
    handle: std::thread::JoinHandle<()>,
}

impl AcknowledgingWriter {
    fn install(
        state: &mut AppState,
        log: Arc<BoundaryLog>,
        fail_prepared: Option<PersistFenceError>,
    ) -> Self {
        Self::install_with(state, log, fail_prepared.map(PreparedFence::Fail))
    }

    fn install_with(
        state: &mut AppState,
        log: Arc<BoundaryLog>,
        prepared_fence: Option<PreparedFence>,
    ) -> Self {
        state.shutdown_persist_blocking();
        let (persist_tx, persist_rx) = mpsc::channel();
        state.persist_tx = persist_tx;
        let stop = Arc::new(AtomicBool::new(false));
        let injected = Arc::new(AtomicBool::new(false));
        let injections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let writer_injections = injections.clone();
        let (writer_stop, writer_injected) = (stop.clone(), injected.clone());
        let mut faults_left = match &prepared_fence {
            Some(PreparedFence::FailTimes(_, times)) => *times,
            Some(PreparedFence::DeadlineThenAcknowledgeLate(_)) => 2,
            Some(PreparedFence::FailSequence(faults)) => faults.len(),
            Some(_) => 1,
            None => 0,
        };
        let handle = std::thread::spawn(move || {
            while let Ok(request) = persist_rx.recv() {
                if writer_stop.load(Ordering::SeqCst) {
                    break;
                }
                let PersistRequest::Fence(fence) = request else {
                    continue;
                };
                // A dispatch's own admission fence, which its dispatch waits
                // on, as opposed to a retry record's acknowledgement, which
                // the tick polls. Both run on the state's budget clock, so the
                // fence's purpose tells them apart, not its clock.
                let dispatch_fence = !fence.retry_acknowledgement;
                // A dispatch makes its admission fence only after the Begin
                // call returned, so an armed barrier lands after the receipt,
                // before delivery.
                if matches!(fence.target, PersistFenceTarget::EngramAdmission { .. })
                    && dispatch_fence
                {
                    log.cross_fence_barrier();
                }
                let content = match &fence.target {
                    PersistFenceTarget::EngramAdmission { content, .. } => Some(content.clone()),
                    _ => None,
                };
                let carries_prepared = content.as_ref().and_then(prepared_begin).is_some();
                // A retry record's acknowledgement fence carries the same
                // admission content, and nobody waits on it: the two-step
                // variant scripts only the dispatch's own preparation fences.
                let scripted_here = !matches!(
                    prepared_fence,
                    Some(PreparedFence::DeadlineThenAcknowledgeLate(_))
                        | Some(PreparedFence::FailSequence(_))
                ) || dispatch_fence;
                if carries_prepared && faults_left > 0 && scripted_here {
                    faults_left -= 1;
                    writer_injected.store(true, Ordering::SeqCst);
                    writer_injections.fetch_add(1, Ordering::SeqCst);
                    match prepared_fence.as_ref().unwrap() {
                        // A writer that stops abandons the fence unfinished.
                        PreparedFence::Fail(PersistFenceError::WorkerStopped) => drop(fence),
                        PreparedFence::Fail(fault) | PreparedFence::FailTimes(fault, _) => {
                            fence.finish(Err(fault.clone()))
                        }
                        PreparedFence::AcknowledgeAfterAttempt(clock) => {
                            clock.wait_for_scripted_waiter();
                            clock.advance(
                                Duration::from_millis(ENGRAM_DISPATCH_BUDGET_MS)
                                    + Duration::from_secs(1),
                            );
                            if let Some(content) = content {
                                log.push(Boundary::Acknowledged(content));
                            }
                            fence.finish(Ok(()));
                        }
                        PreparedFence::DeadlineThenAcknowledgeLate(_) if faults_left == 1 => {
                            fence.finish(Err(PersistFenceError::Deadline))
                        }
                        PreparedFence::FailSequence(faults) => {
                            match faults[faults.len() - faults_left - 1].clone() {
                                FenceStep::Fail(fault) => fence.finish(Err(fault)),
                                FenceStep::Acknowledge => {
                                    if let Some(content) = content {
                                        log.push(Boundary::Acknowledged(content));
                                    }
                                    fence.finish(Ok(()));
                                }
                                FenceStep::AcknowledgeLate(clock) => {
                                    acknowledge_at_deadline(&fence, &clock, content, &log)
                                }
                                FenceStep::Hand(test) => {
                                    test.send(fence).expect("the test takes the fence")
                                }
                            }
                        }
                        PreparedFence::AcknowledgeLate(clock)
                        | PreparedFence::DeadlineThenAcknowledgeLate(clock) => {
                            clock.wait_for_scripted_waiter();
                            let deadline = fence.completion.deadline;
                            if let Some(content) = content {
                                log.push(Boundary::Acknowledged(content));
                            }
                            // Recorded without waking the waiter
                            // (`finish` would notify the clock).
                            assert!(fence.completion.resolve_at(Ok(()), clock.now()));
                            // Exactly to the fence's own deadline: the budget
                            // is spent, and no later operation's is. This
                            // wakes the waiter.
                            clock.advance(deadline.saturating_duration_since(clock.now()));
                            let hook = fence.on_acknowledged.lock().ok().and_then(|mut slot| slot.take());
                            if let Some(hook) = hook {
                                hook();
                            }
                        }
                    }
                    continue;
                }
                if let Some(content) = content {
                    log.push(Boundary::Acknowledged(content));
                }
                fence.finish(Ok(()));
            }
        });
        Self {
            stop,
            injected,
            injections,
            handle,
        }
    }

    fn injected(&self) -> bool {
        self.injected.load(Ordering::SeqCst)
    }

    fn injections(&self) -> usize {
        self.injections.load(Ordering::SeqCst)
    }

    fn finish(self, state: &AppState) {
        self.stop.store(true, Ordering::SeqCst);
        state.persist_tx.send(PersistRequest::Delta).unwrap();
        self.handle.join().unwrap();
    }
}

/// The newest begin card's causal failure, as the durable writer sees it.
fn begin_cause(state: &AppState, session: &str) -> Option<Value> {
    with_record(state, session, |record| {
        record.session.messages.iter().rev().find_map(|message| match message {
            Message::EngramControl { card, .. } => card
                .causal_failure
                .as_ref()
                .filter(|cause| cause.operation == "turn_begin")
                .map(|cause| serde_json::to_value(cause).unwrap()),
            _ => None,
        })
    })
}

#[test]
fn begin_unknown_recovery_healthy_delivery_control() {
    let (state, session, receiver, transport) = lost_begin_fixture(false);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    assert!(transport.producer.grant_state(&session).1.is_some(), "begun");
    assert_eq!(begin_requests(&transport).len(), 1);
    assert_eq!(abort_retry::prompts_received(&receiver), 1);
    let (queued, uncertain) = with_record(&state, &session, |record| {
        (record.queued_prompts.len(), record.engram.uncertain_grant_id.clone())
    });
    assert_eq!(queued, 0);
    assert_eq!(uncertain, None);
}

/// Before the first turn_begin leaves, the queued head's retained evaluate
/// carries the exact begin it is about to send (key string, grant, delivery
/// tokens, expiry basis, prepared phase), and the durable writer has
/// acknowledged that content: the acknowledgement precedes the send.
#[test]
fn begin_unknown_recovery_prepared_begin_is_durable_before_send() {
    let (mut state, session, receiver, transport) = lost_begin_fixture(false);
    let writer = AcknowledgingWriter::install(&mut state, transport.log.clone(), None);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    writer.finish(&state);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "healthy delivery");

    let log = transport.log.snapshot();
    let sent_at = log
        .iter()
        .position(|boundary| matches!(boundary, Boundary::Sent(_)))
        .expect("one begin reached the producer");
    let Boundary::Sent(sent) = &log[sent_at] else {
        unreachable!()
    };
    let acknowledged = log[..sent_at]
        .iter()
        .rev()
        .find_map(|boundary| match boundary {
            Boundary::Acknowledged(content) => {
                prepared_begin(content).map(|prepared| (content, prepared))
            }
            Boundary::Sent(_) => None,
        });
    let Some((content, prepared)) = acknowledged else {
        panic!("the prepared begin must be durably acknowledged before the send: {log:?}");
    };
    assert_eq!(prepared["idempotency_key"], sent["idempotency_key"]);
    assert!(prepared["idempotency_key"].is_string());
    assert_eq!(prepared["grant_id"], sent["grant_id"]);
    assert_eq!(prepared["delivery_tokens"], sent["delivery_tokens"]);
    assert_eq!(prepared["phase"], "prepared");
    let issued = prepared["issued_no_later_than"]
        .as_str()
        .expect("the grant's expiry basis is the local time it had been issued by");
    assert!(chrono::DateTime::parse_from_rfc3339(issued).is_ok());
    let evaluate = &content["queue"]["engram_evaluate"];
    assert_eq!(evaluate["connection"]["session_id"], session.as_str());
    assert!(evaluate["settings"].is_object(), "project/store authority");
    assert_eq!(evaluate["request"]["operation"], "turn_evaluate");
    assert!(
        content["queue"]["pending_prompt"]["id"].is_string(),
        "head identity"
    );
}

/// A failed or ambiguous acknowledgement of the prepared begin withholds the
/// send; nothing can have begun. A failed write proves the record absent: it
/// is withdrawn, leaving no prepared record that would later read as a
/// possibly sent begin, and the head is settled for an automatic fresh
/// admission. A wait that ran out proves nothing about the write: the exact
/// prepared begin is kept as pending and the head parks for its admission
/// retry, with no fresh admission or orphan repair racing it. A stopped
/// writer is not transient and keeps the local-persistence hold that names
/// the unsent begin, scheduled for nothing.
#[test]
fn begin_unknown_recovery_unacknowledged_preparation_withholds_the_send() {
    #[derive(PartialEq)]
    enum Outcome {
        FreshAdmission,
        Pending,
        Held,
    }
    for (label, fault, outcome) in [
        (
            "write failure",
            PersistFenceError::WriteFailed("controlled prepared-begin write failure".to_owned()),
            Outcome::FreshAdmission,
        ),
        ("ambiguous deadline", PersistFenceError::Deadline, Outcome::Pending),
        ("stopped writer", PersistFenceError::WorkerStopped, Outcome::Held),
    ] {
        let (mut state, session, receiver, transport) = lost_begin_fixture(false);
        let writer = AcknowledgingWriter::install(&mut state, transport.log.clone(), Some(fault));
        let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
        let injected = writer.injected();
        writer.finish(&state);
        assert!(
            injected,
            "{label}: the prepared-begin fence is requested before the send"
        );
        assert!(begin_requests(&transport).is_empty(), "{label}: send withheld");
        assert!(receiver.try_recv().is_err(), "{label}: no provider handoff");
        let (retained, uncertain, retry, abort) = with_record(&state, &session, |record| {
            (
                record.queued_prompts.len(),
                record.engram.uncertain_grant_id.clone(),
                record.engram.admission_retry.clone(),
                record.engram.abort_retry.clone(),
            )
        });
        assert_eq!(retained, 1, "{label}: the head is retained");
        assert_eq!(uncertain, None, "{label}: nothing can have begun");
        assert_eq!(
            retry.map(|retry| retry.code),
            (outcome == Outcome::Pending).then(|| ENGRAM_BEGIN_PREPARATION_PENDING_CODE.to_owned()),
            "{label}: only a pending preparation parks for its admission retry"
        );
        assert_eq!(
            abort.map(|abort| abort.reason),
            (outcome == Outcome::FreshAdmission).then_some(EngramAbortReason::BeginPreparation),
            "{label}: only a failed write is settled for an automatic fresh admission"
        );
        let cause = begin_cause(&state, &session).expect("the hold names the unsent begin");
        assert_eq!(cause["failureClass"], "local_state", "{label}");
        assert_eq!(cause["originalCode"], "begin_preparation_unacknowledged");
        assert_eq!(cause["remoteApplication"], "not_started", "{label}");
        let (prepared, rebind_required) = with_record(&state, &session, |record| {
            (
                record.queued_prompts[0]
                    .engram_evaluate
                    .as_ref()
                    .and_then(|evaluate| evaluate.prepared_begin.clone()),
                record.engram.rebind_required,
            )
        });
        if outcome == Outcome::Pending {
            let prepared = prepared.expect("the pending begin is kept exactly");
            assert_eq!(
                Some(prepared.grant_id),
                transport.producer.grant_state(&session).0,
                "{label}: for the issued grant"
            );
            assert!(!rebind_required, "{label}: no orphan repair races the pending begin");
        } else {
            assert_eq!(prepared, None, "{label}: the withheld begin leaves no prepared record");
            assert!(
                !transport.log.snapshot().iter().any(|boundary| matches!(
                    boundary,
                    Boundary::Acknowledged(content) if prepared_begin(content).is_some()
                )),
                "{label}: no acknowledged content carries the withheld begin"
            );
        }
    }
}

/// `tick_at` just past the due time of the retry record that holds the head,
/// an admission retry's or an abort retry's, or an hour on with neither.
fn tick_past_any_due(state: &AppState, session: &str) {
    let due = with_record(state, session, |record| {
        record
            .engram
            .admission_retry
            .as_ref()
            .map(|retry| retry.due_at.clone())
            .or_else(|| record.engram.abort_retry.as_ref().map(|retry| retry.due_at.clone()))
    });
    let at = due.map_or_else(
        || chrono::Utc::now() + chrono::Duration::hours(1),
        |due| {
            chrono::DateTime::parse_from_rfc3339(&due)
                .unwrap()
                .with_timezone(&chrono::Utc)
                + chrono::Duration::milliseconds(1)
        },
    );
    tick_at(state, session, at);
}

/// A prepared-begin fence that misses its deadline, as one did under build
/// load, withholds the send; nothing can have begun, and the timeout proves
/// nothing about the write. Once the writer recovers, the retained head's
/// admission retry acknowledges that exact prepared begin again and sends it
/// when due: no second evaluate and no rebind race it, and the prompt reaches
/// the provider exactly once, with exactly that one begin and no Resume.
#[test]
fn begin_preparation_deadline_is_readmitted_without_resume() {
    let (mut state, session, receiver, transport) = lost_begin_fixture(false);
    let writer = AcknowledgingWriter::install(
        &mut state,
        transport.log.clone(),
        Some(PersistFenceError::Deadline),
    );
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    assert!(writer.injected(), "the prepared-begin fence missed its deadline");
    assert!(begin_requests(&transport).is_empty(), "the first send is withheld");
    assert!(receiver.try_recv().is_err(), "no provider handoff yet");

    // The writer acknowledges every later fence: the load has passed.
    tick_past_any_due(&state, &session);
    tick_past_any_due(&state, &session);
    writer.finish(&state);

    assert_eq!(
        abort_retry::prompts_received(&receiver),
        1,
        "the retained prompt is delivered once, without Resume"
    );
    assert_eq!(begin_requests(&transport).len(), 1, "exactly one begin is sent");
    let (queued, retry, uncertain) = with_record(&state, &session, |record| {
        (
            record.queued_prompts.len(),
            record.engram.admission_retry.clone(),
            record.engram.uncertain_grant_id.clone(),
        )
    });
    assert_eq!(queued, 0, "the head was delivered");
    assert!(retry.is_none(), "the retry ended with the delivery");
    assert_eq!(uncertain, None);
    let operations: Vec<String> = transport
        .producer
        .requests()
        .iter()
        .map(|recorded| recorded.request["operation"].as_str().unwrap_or_default().to_owned())
        .collect();
    let first_evaluate = operations
        .iter()
        .position(|operation| operation == "turn_evaluate")
        .expect("the evaluate");
    assert_eq!(
        operations.iter().filter(|operation| *operation == "turn_evaluate").count(),
        1,
        "no fresh evaluate races the pending preparation: {operations:?}"
    );
    assert!(
        !operations[first_evaluate..].iter().any(|operation| operation == "session_bind"),
        "no rebind races it either: {operations:?}"
    );
    let prepared = transport
        .log
        .snapshot()
        .iter()
        .find_map(|boundary| match boundary {
            Boundary::Acknowledged(content) => prepared_begin(content).cloned(),
            Boundary::Sent(_) => None,
        })
        .expect("the pending begin was acknowledged again");
    let begins = begin_requests(&transport);
    assert_eq!(begins[0]["idempotency_key"], prepared["idempotency_key"], "that exact begin");
    assert_eq!(begins[0]["grant_id"], prepared["grant_id"]);
    assert_eq!(begins[0]["delivery_tokens"], prepared["delivery_tokens"]);
}

/// A prepared-begin fence that succeeds only after the dispatch budget is
/// classified by what happened: the prepared begin is durable, so it is kept,
/// never withdrawn as unacknowledged, and the begin then sent is exactly that
/// prepared begin. The prompt reaches the provider once.
#[test]
fn begin_preparation_acknowledged_late_keeps_the_prepared_begin() {
    let (mut state, session, receiver, transport) = lost_begin_fixture(false);
    let clock = state.engram_budget_clock();
    let writer = AcknowledgingWriter::install_with(
        &mut state,
        transport.log.clone(),
        Some(PreparedFence::AcknowledgeLate(clock)),
    );
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    assert!(writer.injected(), "the prepared-begin fence was acknowledged late");
    for _ in 0..3 {
        tick_past_any_due(&state, &session);
    }
    writer.finish(&state);

    let causes: Vec<Value> = with_record(&state, &session, |record| {
        record
            .session
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::EngramControl { card, .. } => card.causal_failure.as_ref().map(|cause| serde_json::to_value(cause).unwrap()),
                _ => None,
            })
            .collect()
    });
    assert!(
        !causes.iter().any(|cause| cause["originalCode"] == ENGRAM_BEGIN_PREPARATION_CODE),
        "a late acknowledgement is not an unacknowledged preparation: {causes:?}"
    );
    let log = transport.log.snapshot();
    let first_sent = log
        .iter()
        .position(|boundary| matches!(boundary, Boundary::Sent(_)))
        .expect("the prepared begin is sent");
    let Boundary::Sent(sent) = &log[first_sent] else {
        unreachable!()
    };
    let prepared = log[..first_sent]
        .iter()
        .rev()
        .find_map(|boundary| match boundary {
            Boundary::Acknowledged(content) => prepared_begin(content).cloned(),
            Boundary::Sent(_) => None,
        })
        .expect("the late-acknowledged prepared begin precedes the send");
    assert_eq!(prepared["idempotency_key"], sent["idempotency_key"], "the exact prepared begin");
    assert_eq!(prepared["grant_id"], sent["grant_id"]);
    assert_eq!(prepared["delivery_tokens"], sent["delivery_tokens"]);
    // The late path was taken: with the budget clock at the fence's deadline,
    // the admission fence after that begin is late too, so its delivery is
    // withheld and its closed grant admitted afresh. A timely acknowledgement
    // would have delivered after the first begin alone.
    let begins = begin_requests(&transport);
    assert_eq!(begins.len(), 2, "the late-acknowledged begin, then the fresh admission's: {begins:?}");
    assert_eq!(begins[0]["idempotency_key"], prepared["idempotency_key"]);
    assert_ne!(begins[1]["grant_id"], begins[0]["grant_id"]);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered once");
}

/// After a failed write the re-admission is a fresh one, and the issued
/// grant of the withheld begin is settled before it: the rebind that expires
/// that grant comes before the fresh evaluate, the fresh evaluate has a new
/// key, and the begun grant is the fresh one. There are never two live
/// grants. (A missed wait keeps its exact begin instead:
/// `begin_preparation_deadline_is_readmitted_without_resume`.)
#[test]
fn begin_preparation_readmission_rebinds_before_its_fresh_evaluate() {
    {
        let fault =
            PersistFenceError::WriteFailed("controlled prepared-begin write failure".to_owned());
        let (mut state, session, receiver, transport) = lost_begin_fixture(false);
        let writer = AcknowledgingWriter::install(&mut state, transport.log.clone(), Some(fault.clone()));
        let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
        let (withheld_grant, _) = transport.producer.grant_state(&session);
        let withheld_grant = withheld_grant.expect("the withheld begin's grant was issued");
        let retry = with_record(&state, &session, |record| {
            assert_eq!(record.queued_prompts.len(), 1, "{fault:?}: the prompt is retained");
            assert!(record.queued_prompts[0].engram_evaluate.is_none(), "{fault:?}: only the stale evaluate is dropped");
            record.engram.abort_retry.clone()
        })
        .expect("the head is settled for an automatic fresh admission");
        assert_eq!(retry.reason, EngramAbortReason::BeginPreparation);
        assert_eq!(retry.settled_grant_id, None, "no grant was begun");
        tick_past_any_due(&state, &session);
        writer.finish(&state);
        assert_eq!(abort_retry::prompts_received(&receiver), 1, "{fault:?}: delivered once");

        let operations: Vec<(String, Value)> = transport
            .producer
            .requests()
            .into_iter()
            .map(|recorded| (recorded.request["operation"].as_str().unwrap_or_default().to_owned(), recorded.request))
            .collect();
        let evaluates: Vec<usize> = operations
            .iter()
            .enumerate()
            .filter(|(_, (operation, _))| operation == "turn_evaluate")
            .map(|(position, _)| position)
            .collect();
        assert_eq!(evaluates.len(), 2, "{fault:?}: one withheld and one fresh evaluate: {operations:?}");
        assert!(
            operations[evaluates[0]..evaluates[1]].iter().any(|(operation, _)| operation == "session_bind"),
            "{fault:?}: the rebind settles the issued grant before the fresh evaluate: {operations:?}"
        );
        assert_ne!(
            operations[evaluates[0]].1["idempotency_key"], operations[evaluates[1]].1["idempotency_key"],
            "{fault:?}: the fresh evaluate is a new operation"
        );
        let begins = begin_requests(&transport);
        assert_eq!(begins.len(), 1, "{fault:?}: exactly one begin");
        assert_ne!(begins[0]["grant_id"], withheld_grant.as_str(), "{fault:?}: the withheld grant is never begun");
    }
}

/// The begin-unknown hold keeps the original raw transport error kind and
/// message and the attempt key. A transport that captured no control process
/// says so explicitly instead of inventing process evidence.
#[test]
fn begin_unknown_recovery_hold_records_raw_error_and_unavailable_sidecar() {
    let (state, session, receiver, transport) = lost_begin_fixture(true);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    assert!(receiver.try_recv().is_err());
    let key = begin_requests(&transport)[0]["idempotency_key"].clone();
    let cause = begin_cause(&state, &session).expect("the begin-unknown hold has a cause");
    assert_eq!(cause["failureClass"], "transport");
    assert_eq!(cause["message"], LOST_BEGIN_REPLY);
    assert_eq!(cause["attemptId"], key);
    assert_eq!(cause["remoteApplication"], "unknown");
    assert_eq!(
        cause["controlProcess"],
        json!({"identity": "unavailable", "exitState": "unavailable"}),
        "uncaptured sidecar evidence is explicitly unavailable"
    );
}
