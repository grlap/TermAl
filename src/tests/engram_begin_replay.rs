//! The exact begin replay that reconciles a begin-unknown hold on the
//! production admission path: the same durable begin (stored key string,
//! grant and delivery tokens) is replayed on the common retry schedule until
//! Engram gives a definite outcome, with exactly one provider handoff. Owns the
//! replay, its never-begun and ambiguous outcomes, its schedule past the
//! admission budget, the Begin call bound and the joins with Stop, Cancel and
//! Resume. Does not own the prepared-begin acknowledgement before the first send
//! (engram_begin_unknown_recovery.rs) or boot reconstruction. Registered by
//! engram_admission_retry.rs, whose clock and tick helpers it uses.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const LOST_REPLY: &str =
    "control worker reached EOF after applying turn_begin; reply frame missing";
const UNAVAILABLE: &str = "control worker unavailable before the request reached Engram";

/// A structured error reply from Engram, as the control process relays it.
fn structured_error(code: &str) -> EngramTransportError {
    EngramTransportError::remote(EngramControlErrorBody {
        code: code.to_owned(),
        message: format!("Engram answered the begin with {code}"),
    })
}

/// Scripts the control path around a stateful producer: which begins are lost
/// before or after the producer sees them, which replay Engram refuses, what
/// session status reports, and how long a begin takes against its call bound.
#[derive(Default)]
struct ReplayScript {
    /// Applied begin replies lost after the producer applied them.
    lose_after_apply: AtomicUsize,
    /// Begins (only) that fail before reaching the producer.
    unavailable: AtomicUsize,
    /// Every begin and evaluate fails before reaching the producer.
    all_unavailable: AtomicBool,
    /// The next replayed begin (not the first send) is refused with this.
    refuse_replay: Mutex<Option<String>>,
    /// Session status reports this open grant instead of the producer's.
    status_open_grant: Mutex<Option<Option<String>>>,
    /// The producer's status reply also names its open grant's state, as
    /// Engram does (the stateful fixture omits it).
    status_open_grant_state: Mutex<Option<String>>,
    /// The next applied begin's reply is this structured error instead.
    error_after_apply: Mutex<Option<String>>,
    /// The next replayed begin gets this structured error, unsent.
    replay_error: Mutex<Option<String>>,
    /// A producer refusal of a replay arrives shaped as a structured error.
    replay_refusal_as_error: AtomicBool,
    /// A begin takes this long on the budget clock before it replies.
    begin_delay: Mutex<Option<Duration>>,
    /// A replayed begin waits here, once, until the test releases it.
    hold_replay: Mutex<Option<(mpsc::SyncSender<()>, mpsc::Receiver<()>)>>,
    sent_begins: Mutex<Vec<Value>>,
    begin_bounds: Mutex<Vec<Duration>>,
    sent_evaluates: Mutex<Vec<Value>>,
}

struct ScriptedReplay {
    producer: Arc<StatefulEngramControlTransport>,
    clock: EngramBudgetClock,
    script: ReplayScript,
}

impl ScriptedReplay {
    fn begin_fails_before_producer(&self) -> bool {
        self.script.all_unavailable.load(Ordering::SeqCst)
            || self
                .script
                .unavailable
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |left| left.checked_sub(1))
                .is_ok()
    }

    fn begin(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        let replay = {
            let mut sent = self.script.sent_begins.lock().unwrap();
            sent.push(serde_json::to_value(request).unwrap());
            sent.len() > 1
        };
        self.script.begin_bounds.lock().unwrap().push(timeout);
        if self.begin_fails_before_producer() {
            return Err(EngramTransportError::transport(UNAVAILABLE));
        }
        if replay && let Some((entered, release)) = self.script.hold_replay.lock().unwrap().take() {
            entered.send(()).unwrap();
            release.recv().unwrap();
        }
        if replay && let Some(code) = self.script.refuse_replay.lock().unwrap().take() {
            return Ok(json!({ "decision": "refuse", "code": code }));
        }
        if replay && let Some(code) = self.script.replay_error.lock().unwrap().take() {
            return Err(structured_error(&code));
        }
        if let Some(delay) = *self.script.begin_delay.lock().unwrap() {
            // A transport that honours its call bound: a reply slower than
            // the bound never arrives.
            if delay >= timeout {
                self.clock.advance(timeout);
                return Err(EngramTransportError::deadline(format!(
                    "Engram control call exceeded {} ms",
                    timeout.as_millis()
                )));
            }
            self.clock.advance(delay);
        }
        let reply = self.producer.request(connection, request, timeout)?;
        if replay
            && reply["decision"] == "refuse"
            && self.script.replay_refusal_as_error.load(Ordering::SeqCst)
        {
            return Err(structured_error(reply["code"].as_str().unwrap()));
        }
        if reply["decision"] == "begin"
            && let Some(code) = self.script.error_after_apply.lock().unwrap().take()
        {
            return Err(structured_error(&code));
        }
        if reply["decision"] == "begin"
            && self
                .script
                .lose_after_apply
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |left| left.checked_sub(1))
                .is_ok()
        {
            return Err(EngramTransportError::transport(LOST_REPLY));
        }
        Ok(reply)
    }
}

impl EngramControlTransport for ScriptedReplay {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        match request {
            EngramControlRequest::TurnBegin { .. } => self.begin(connection, request, timeout),
            EngramControlRequest::TurnEvaluate { .. } => {
                self.script
                    .sent_evaluates
                    .lock()
                    .unwrap()
                    .push(serde_json::to_value(request).unwrap());
                if self.script.all_unavailable.load(Ordering::SeqCst) {
                    return Err(EngramTransportError::transport(UNAVAILABLE));
                }
                self.producer.request(connection, request, timeout)
            }
            EngramControlRequest::SessionStatus { .. } => {
                let scripted = self.script.status_open_grant.lock().unwrap().clone();
                match scripted {
                    Some(open) => Ok(json!({
                        "phase": if open.is_some() { "turn_open" } else { "ready" },
                        "open_grant_id": open,
                        "confirmed_cursor": 0
                    })),
                    None => {
                        let mut reply = self.producer.request(connection, request, timeout)?;
                        if let Some(state) = self.script.status_open_grant_state.lock().unwrap().clone()
                            && !reply["open_grant_id"].is_null()
                        {
                            reply["open_grant_state"] = json!(state);
                        }
                        Ok(reply)
                    }
                }
            }
            _ => self.producer.request(connection, request, timeout),
        }
    }

    fn shutdown_session(&self, session_id: &str) {
        self.producer.shutdown_session(session_id);
    }
}

fn replay_fixture_with(
    producer: Arc<StatefulEngramControlTransport>,
    script: ReplayScript,
) -> (AppState, String, mpsc::Receiver<CodexRuntimeCommand>, Arc<ScriptedReplay>) {
    let (state, session, receiver, _) = root_fixture([]);
    let transport = Arc::new(ScriptedReplay {
        producer,
        clock: state.engram_budget_clock(),
        script,
    });
    state.install_control_test_transport(transport.clone());
    (state, session, receiver, transport)
}

fn replay_fixture(
    script: ReplayScript,
) -> (AppState, String, mpsc::Receiver<CodexRuntimeCommand>, Arc<ScriptedReplay>) {
    replay_fixture_with(StatefulEngramControlTransport::new(), script)
}

fn begins(transport: &ScriptedReplay) -> Vec<Value> {
    transport.script.sent_begins.lock().unwrap().clone()
}

fn evaluates(transport: &ScriptedReplay) -> Vec<Value> {
    transport.script.sent_evaluates.lock().unwrap().clone()
}

fn uncertain(state: &AppState, session: &str) -> Option<String> {
    with_record(state, session, |record| record.engram.uncertain_grant_id.clone())
}

fn retained(state: &AppState, session: &str) -> usize {
    with_record(state, session, |record| record.queued_prompts.len())
}

/// The newest begin card's causal failure.
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

/// Delivers the root head and checks it stopped as begin-unknown: retained,
/// its grant possibly begun, nothing handed to the provider.
fn deliver_into_begin_unknown(
    state: &AppState,
    session: &str,
    receiver: &mpsc::Receiver<CodexRuntimeCommand>,
    transport: &ScriptedReplay,
) -> Value {
    let _ = deliver_turn_dispatch(state, root_dispatch(state, session, false));
    let first = begins(transport)
        .first()
        .cloned()
        .expect("the first begin was sent");
    assert_eq!(retained(state, session), 1, "the original head is retained");
    assert_eq!(
        uncertain(state, session).as_deref(),
        first["grant_id"].as_str(),
        "begin-unknown"
    );
    assert!(receiver.try_recv().is_err(), "nothing reached the provider yet");
    first
}

fn assert_every_begin_is(transport: &ScriptedReplay, first: &Value) {
    for begin in begins(transport) {
        assert_eq!(begin["idempotency_key"], first["idempotency_key"], "same key");
        assert_eq!(begin["grant_id"], first["grant_id"], "same grant");
        assert_eq!(begin["delivery_tokens"], first["delivery_tokens"], "same tokens");
    }
}

/// C3 applied begin. The producer applied the begin and its reply was lost.
/// With control back and the shared retry clock advanced, and no Resume, new
/// message, wake or restart, the exact begin is replayed with its original
/// key, the retained receipt settles it, and the head reaches the provider
/// exactly once without a fresh evaluate.
#[test]
fn begin_replay_applied_begin_is_delivered_once_without_resume() {
    let script = ReplayScript::default();
    script.lose_after_apply.store(1, Ordering::SeqCst);
    let (state, session, receiver, transport) = replay_fixture(script);
    let first = deliver_into_begin_unknown(&state, &session, &receiver, &transport);

    tick_past_due(&state, &session);
    tick_past_due(&state, &session);

    assert_eq!(begins(&transport).len(), 2, "the exact begin is replayed automatically");
    assert_every_begin_is(&transport, &first);
    assert_eq!(evaluates(&transport).len(), 1, "no fresh evaluate");
    assert_eq!(
        abort_retry::prompts_received(&receiver),
        1,
        "the retained head reaches the provider exactly once"
    );
    assert_eq!(uncertain(&state, &session), None, "settled by the receipt");
    assert_eq!(retained(&state, &session), 0);
}

/// C3 never-begun proof. The first begin never reached Engram, and its replay
/// is refused grant_expired: the grant is proven never begun, so the head
/// proceeds through a fresh ordinary evaluate under a new key, a new grant is
/// begun, and the head reaches the provider exactly once.
#[test]
fn begin_replay_expired_grant_proceeds_through_a_fresh_evaluate() {
    let script = ReplayScript::default();
    script.unavailable.store(1, Ordering::SeqCst);
    let producer = StatefulEngramControlTransport::with_first_begin_refusal("grant_expired");
    let (state, session, receiver, transport) = replay_fixture_with(producer, script);
    let first = deliver_into_begin_unknown(&state, &session, &receiver, &transport);

    for _ in 0..3 {
        tick_past_due(&state, &session);
    }

    let sent = begins(&transport);
    assert!(sent.len() >= 3, "replay, then the fresh grant's begin: {sent:?}");
    assert_eq!(sent[1]["idempotency_key"], first["idempotency_key"], "exact replay");
    let last = sent.last().unwrap();
    assert_ne!(last["grant_id"], first["grant_id"], "the expired grant is never resurrected");
    let keys = evaluates(&transport)
        .iter()
        .map(|evaluate| evaluate["idempotency_key"].clone())
        .collect::<Vec<_>>();
    assert_eq!(keys.len(), 2, "one fresh evaluate: {keys:?}");
    assert_ne!(keys[0], keys[1], "the fresh evaluate has a new key");
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered exactly once");
    assert_eq!(uncertain(&state, &session), None);
}

/// C3 ambiguous shape. A replay refused grant_scope_mismatch proves nothing.
/// With status showing no open grant of ours, the hold stays: no fresh
/// evaluate, no delivery, no further automatic replay, and the hold names the
/// refusal.
#[test]
fn begin_replay_scope_mismatch_keeps_the_hold_without_a_fresh_evaluate() {
    let script = ReplayScript::default();
    script.unavailable.store(1, Ordering::SeqCst);
    *script.refuse_replay.lock().unwrap() = Some("grant_scope_mismatch".to_owned());
    *script.status_open_grant.lock().unwrap() = Some(None);
    let (state, session, receiver, transport) = replay_fixture(script);
    let first = deliver_into_begin_unknown(&state, &session, &receiver, &transport);

    for _ in 0..3 {
        tick_past_due(&state, &session);
    }

    assert_eq!(begins(&transport).len(), 2, "one replay, then the explicit hold");
    assert_every_begin_is(&transport, &first);
    assert_eq!(evaluates(&transport).len(), 1, "no fresh evaluate");
    assert!(receiver.try_recv().is_err(), "no provider handoff");
    assert_eq!(uncertain(&state, &session).as_deref(), first["grant_id"].as_str());
    assert_eq!(retained(&state, &session), 1);
    let retry = with_record(&state, &session, |record| record.engram.admission_retry.clone());
    assert!(retry.is_none(), "an explicit hold, not an automatic replay");
    let cause = begin_cause(&state, &session).expect("the hold names its refusal");
    assert_eq!(cause["originalCode"], "grant_scope_mismatch");
}

/// C3 ambiguous shape settled positively. The begin was applied and its reply
/// lost; the replay is refused grant_scope_mismatch, but producer status shows
/// our grant open and begun: it settles as applied and is delivered once.
#[test]
fn begin_replay_scope_mismatch_with_our_grant_begun_settles_as_applied() {
    let script = ReplayScript::default();
    script.lose_after_apply.store(1, Ordering::SeqCst);
    *script.refuse_replay.lock().unwrap() = Some("grant_scope_mismatch".to_owned());
    *script.status_open_grant_state.lock().unwrap() = Some("begun".to_owned());
    let (state, session, receiver, transport) = replay_fixture(script);
    let first = deliver_into_begin_unknown(&state, &session, &receiver, &transport);

    tick_past_due(&state, &session);
    tick_past_due(&state, &session);

    assert_every_begin_is(&transport, &first);
    assert_eq!(evaluates(&transport).len(), 1, "no fresh evaluate");
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered exactly once");
    assert_eq!(uncertain(&state, &session), None);
}

/// C3 ambiguous shape, our grant only issued. Status names an issued grant
/// open too, so a matching id in the issued state proves nothing: the replay
/// refused grant_scope_mismatch stays held, with no delivery and no evaluate.
#[test]
fn begin_replay_scope_mismatch_with_our_grant_only_issued_keeps_the_hold() {
    let script = ReplayScript::default();
    script.unavailable.store(1, Ordering::SeqCst);
    *script.refuse_replay.lock().unwrap() = Some("grant_scope_mismatch".to_owned());
    *script.status_open_grant_state.lock().unwrap() = Some("issued".to_owned());
    let (state, session, receiver, transport) = replay_fixture(script);
    let first = deliver_into_begin_unknown(&state, &session, &receiver, &transport);

    for _ in 0..3 {
        tick_past_due(&state, &session);
    }

    assert_eq!(begins(&transport).len(), 2, "one replay, then the explicit hold");
    assert_eq!(evaluates(&transport).len(), 1, "no fresh evaluate");
    assert!(receiver.try_recv().is_err(), "an issued grant is never delivered");
    assert_eq!(uncertain(&state, &session).as_deref(), first["grant_id"].as_str());
    assert_eq!(retained(&state, &session), 1);
}

/// C3 and C4. A structured error reply to the begin (here a storage error
/// after Engram applied it) leaves the outcome unknown like a lost reply: the
/// exact replay is scheduled, settles on the retained receipt and delivers once.
#[test]
fn begin_replay_structured_error_reply_is_replayed() {
    let script = ReplayScript::default();
    *script.error_after_apply.lock().unwrap() = Some("storage_error".to_owned());
    let (state, session, receiver, transport) = replay_fixture(script);
    let first = deliver_into_begin_unknown(&state, &session, &receiver, &transport);
    let retry = with_record(&state, &session, |record| record.engram.admission_retry.clone())
        .expect("a structured error reply schedules the exact replay");
    assert_eq!(retry.code, ENGRAM_BEGIN_REPLAY_CODE);

    tick_past_due(&state, &session);
    tick_past_due(&state, &session);

    assert_every_begin_is(&transport, &first);
    assert_eq!(evaluates(&transport).len(), 1);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered exactly once");
}

/// C4. A replay that itself gets a structured error reply keeps the schedule:
/// the next replay, with the same key, settles and delivers once.
#[test]
fn begin_replay_structured_error_on_a_replay_keeps_the_schedule() {
    let script = ReplayScript::default();
    script.lose_after_apply.store(1, Ordering::SeqCst);
    *script.replay_error.lock().unwrap() = Some("storage_error".to_owned());
    let (state, session, receiver, transport) = replay_fixture(script);
    let first = deliver_into_begin_unknown(&state, &session, &receiver, &transport);

    for _ in 0..4 {
        tick_past_due(&state, &session);
    }

    assert!(begins(&transport).len() >= 3, "the failed replay was followed by another");
    assert_every_begin_is(&transport, &first);
    assert_eq!(evaluates(&transport).len(), 1);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered exactly once");
}

/// C3. An error-shaped stale_fence answering a replay is the same never-begun
/// proof as the refusal: the head proceeds through a fresh evaluate under a new
/// key and a new grant, and is delivered once. The producer refuses (and so
/// expires the grant) itself; only the reply's shape is an error.
#[test]
fn begin_replay_stale_fence_error_proceeds_through_a_fresh_evaluate() {
    let script = ReplayScript::default();
    script.unavailable.store(1, Ordering::SeqCst);
    script.replay_refusal_as_error.store(true, Ordering::SeqCst);
    let producer = StatefulEngramControlTransport::with_first_begin_refusal("stale_fence");
    let (state, session, receiver, transport) = replay_fixture_with(producer, script);
    let first = deliver_into_begin_unknown(&state, &session, &receiver, &transport);

    for _ in 0..3 {
        tick_past_due(&state, &session);
    }

    let keys = evaluates(&transport)
        .iter()
        .map(|evaluate| evaluate["idempotency_key"].clone())
        .collect::<Vec<_>>();
    assert_eq!(keys.len(), 2, "one fresh evaluate: {keys:?}");
    assert_ne!(keys[0], keys[1]);
    let last = begins(&transport).last().cloned().unwrap();
    assert_ne!(
        last["grant_id"], first["grant_id"],
        "the old grant is never resurrected: {:?}",
        transport
            .producer
            .requests()
            .iter()
            .map(|recorded| (
                recorded.request["operation"].clone(),
                recorded.request["grant_id"].clone(),
                recorded.request["idempotency_key"].clone()
            ))
            .collect::<Vec<_>>()
    );
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered exactly once");
    assert_eq!(uncertain(&state, &session), None);
}

/// C3. A replay refused with a code that proves nothing, here delta_required
/// (which heals a first send through a fresh evaluate), holds the head with
/// its possibly begun grant instead: no fresh evaluate, no delivery.
#[test]
fn begin_replay_non_proving_refusal_holds_without_a_fresh_evaluate() {
    let script = ReplayScript::default();
    script.unavailable.store(1, Ordering::SeqCst);
    *script.refuse_replay.lock().unwrap() = Some("delta_required".to_owned());
    let (state, session, receiver, transport) = replay_fixture(script);
    let first = deliver_into_begin_unknown(&state, &session, &receiver, &transport);

    for _ in 0..3 {
        tick_past_due(&state, &session);
    }

    assert_eq!(begins(&transport).len(), 2, "one replay, then the explicit hold");
    assert_eq!(evaluates(&transport).len(), 1, "no fresh evaluate");
    assert!(receiver.try_recv().is_err());
    assert_eq!(uncertain(&state, &session).as_deref(), first["grant_id"].as_str());
    assert_eq!(retained(&state, &session), 1, "the head is held, not rejected");
    let cause = begin_cause(&state, &session).expect("the hold names its refusal");
    assert_eq!(cause["originalCode"], "delta_required");
}

/// C3. A replay refused task_unbound proves the grant never began, and the
/// refusal-healing route does not take it: the refusal ends the head as a
/// first send's refusal does, with no fresh evaluate and no delivery.
#[test]
fn begin_replay_never_begun_refusal_without_healing_ends_the_head() {
    let script = ReplayScript::default();
    script.unavailable.store(1, Ordering::SeqCst);
    *script.refuse_replay.lock().unwrap() = Some("task_unbound".to_owned());
    let (state, session, receiver, transport) = replay_fixture(script);
    deliver_into_begin_unknown(&state, &session, &receiver, &transport);

    for _ in 0..3 {
        tick_past_due(&state, &session);
    }

    assert_eq!(begins(&transport).len(), 2);
    assert_eq!(evaluates(&transport).len(), 1, "no fresh evaluate");
    assert!(receiver.try_recv().is_err(), "no provider handoff");
    assert_eq!(uncertain(&state, &session), None, "proven never begun");
    assert_eq!(retained(&state, &session), 0, "the refusal ends the head");
    let repair_armed = with_record(&state, &session, |record| {
        record.session.messages.iter().rev().find_map(|message| match message {
            Message::EngramControl { card, .. } => Some(card.repair_armed),
            _ => None,
        })
    });
    assert_eq!(
        repair_armed,
        Some(true),
        "the possibly still issued grant gets the orphan repair a first send's refusal arms"
    );
}

/// C3 and C6. A structured error saying the routing token no longer names a
/// bound session can never be answered under that token, and a rebind could
/// expire the grant: on the first send or on a replay it ends in an explicit
/// hold naming that error, never in replays resent forever with a dead token.
#[test]
fn begin_replay_invalidated_routing_token_ends_in_an_explicit_hold() {
    for on_replay in [false, true] {
        let script = ReplayScript::default();
        if on_replay {
            script.lose_after_apply.store(1, Ordering::SeqCst);
            *script.replay_error.lock().unwrap() = Some("control_session_not_bound".to_owned());
        } else {
            *script.error_after_apply.lock().unwrap() =
                Some("control_session_not_bound".to_owned());
        }
        let (state, session, receiver, transport) = replay_fixture(script);
        let first = deliver_into_begin_unknown(&state, &session, &receiver, &transport);

        for _ in 0..4 {
            tick_past_due(&state, &session);
        }

        let expected = if on_replay { 2 } else { 1 };
        assert_eq!(begins(&transport).len(), expected, "on_replay={on_replay}: no more replays");
        assert!(receiver.try_recv().is_err(), "on_replay={on_replay}");
        assert_eq!(retained(&state, &session), 1, "on_replay={on_replay}: held");
        assert_eq!(
            uncertain(&state, &session).as_deref(),
            first["grant_id"].as_str(),
            "on_replay={on_replay}: still possibly begun"
        );
        let retry = with_record(&state, &session, |record| record.engram.admission_retry.clone());
        assert!(retry.is_none(), "on_replay={on_replay}: an explicit hold");
        let cause = begin_cause(&state, &session).expect("the hold names its error");
        assert_eq!(cause["originalCode"], "control_session_not_bound", "on_replay={on_replay}");
    }
}

/// C2 on a replay. When the replay's durable re-acknowledgement fails, the
/// begin is not resent, and its prepared record is kept unchanged (with its
/// original expiry basis), because that begin may have been applied before.
#[test]
fn begin_replay_failed_reacknowledgement_keeps_the_prepared_begin() {
    let script = ReplayScript::default();
    script.lose_after_apply.store(1, Ordering::SeqCst);
    let (mut state, session, receiver, transport) = replay_fixture(script);
    deliver_into_begin_unknown(&state, &session, &receiver, &transport);
    let prepared = with_record(&state, &session, |record| {
        record.queued_prompts[0]
            .engram_evaluate
            .as_ref()
            .and_then(|evaluate| evaluate.prepared_begin.clone())
    })
    .expect("the prepared begin stands");
    // The synchronous store becomes unusable: every later save fails.
    let unusable = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("replay-store-is-directory");
    fs::create_dir_all(&unusable).unwrap();
    state.persistence_path = Arc::new(unusable);

    tick_past_due(&state, &session);
    tick_past_due(&state, &session);

    assert_eq!(begins(&transport).len(), 1, "no begin without its acknowledgement");
    assert!(receiver.try_recv().is_err());
    let kept = with_record(&state, &session, |record| {
        record.queued_prompts[0]
            .engram_evaluate
            .as_ref()
            .and_then(|evaluate| evaluate.prepared_begin.clone())
    });
    assert_eq!(kept, Some(prepared), "kept unchanged, expiry basis included");
    assert_eq!(uncertain(&state, &session).as_deref(), Some(kept.unwrap().grant_id.as_str()));
}

/// C4 and C6. Control stays unavailable well past the original 20 s admission
/// budget: the same begin keeps its schedule (no deadline turns it into a hold
/// that needs Resume, no new grant or evaluate), the preview names begin
/// reconciliation with its attempt and next due time, and when control
/// returns the head is delivered once.
#[test]
fn begin_replay_keeps_its_schedule_past_the_admission_budget() {
    let script = ReplayScript::default();
    script.lose_after_apply.store(1, Ordering::SeqCst);
    let (state, session, receiver, transport) = replay_fixture(script);
    let first = deliver_into_begin_unknown(&state, &session, &receiver, &transport);
    transport.script.all_unavailable.store(true, Ordering::SeqCst);

    for attempt in 1..=4 {
        let retry = with_record(&state, &session, |record| record.engram.admission_retry.clone())
            .unwrap_or_else(|| panic!("attempt {attempt}: the replay stays scheduled"));
        let preview = with_record(&state, &session, |record| record.session.preview.clone());
        assert!(preview.contains("begin reconciliation"), "{preview}");
        assert!(preview.contains(&format!("attempt {}", retry.attempts)), "{preview}");
        assert!(preview.contains(&retry.due_at), "{preview}");
        // One tick acknowledges the record, the next runs its due attempt.
        tick_past_due(&state, &session);
        tick_past_due(&state, &session);
    }
    let retry = with_record(&state, &session, |record| record.engram.admission_retry.clone())
        .expect("still scheduled after the budget");
    let held = chrono::DateTime::parse_from_rfc3339(&retry.held_since).unwrap();
    let due = chrono::DateTime::parse_from_rfc3339(&retry.due_at).unwrap();
    assert!(
        due - held > chrono::Duration::seconds(20),
        "the schedule runs past the original budget: {} .. {}",
        retry.held_since,
        retry.due_at
    );
    assert_eq!(uncertain(&state, &session).as_deref(), first["grant_id"].as_str());
    assert!(receiver.try_recv().is_err());

    transport.script.all_unavailable.store(false, Ordering::SeqCst);
    tick_past_due(&state, &session);
    tick_past_due(&state, &session);

    assert_every_begin_is(&transport, &first);
    assert_eq!(evaluates(&transport).len(), 1, "no new grant or evaluate");
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered exactly once");
}

/// C4 cap deferral. A due replay the host-wide cap defers keeps its attempt
/// index and first-held time and sends nothing; once a slot is free it
/// replays the same begin and delivers once.
#[test]
fn begin_replay_cap_deferral_charges_nothing() {
    let script = ReplayScript::default();
    script.lose_after_apply.store(1, Ordering::SeqCst);
    let (state, session, receiver, transport) = replay_fixture(script);
    let first = deliver_into_begin_unknown(&state, &session, &receiver, &transport);
    let before = with_record(&state, &session, |record| record.engram.admission_retry.clone())
        .expect("the replay is scheduled");

    // Every slot is taken before the replay is due, so each tick (its
    // acknowledgement, then its due attempt) finds the cap full.
    let held = (0..ENGRAM_RETRY_MAX_IN_FLIGHT)
        .map(|_| state.engram_retry_slots.try_acquire().expect("a free slot"))
        .collect::<Vec<_>>();
    tick_past_due(&state, &session);
    tick_past_due(&state, &session);
    let deferred = with_record(&state, &session, |record| record.engram.admission_retry.clone())
        .expect("still scheduled");
    assert_eq!(begins(&transport).len(), 1, "nothing sent while the cap is full");
    assert_eq!(deferred.attempts, before.attempts, "no attempt charged");
    assert_eq!(deferred.held_since, before.held_since, "first-held time kept");
    assert_eq!(deferred.due_at, before.due_at, "its due time does not move");

    drop(held);
    tick_past_due(&state, &session);
    tick_past_due(&state, &session);
    assert_every_begin_is(&transport, &first);
    assert_eq!(begins(&transport).len(), 2);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered exactly once");
}

/// C4 call bound. A synchronous Begin uses max(configured call bound,
/// 10,000 ms), capped at 20,000 ms: configured values below, equal to and
/// above 10 s, and above the cap.
#[test]
fn begin_replay_begin_call_bound_is_at_least_ten_seconds() {
    for (configured, expected) in [
        (5_000, 10_000),
        (10_000, 10_000),
        (15_000, 15_000),
        (25_000, 20_000),
    ] {
        let (state, session, receiver, transport) = replay_fixture(ReplayScript::default());
        set_call_bound(&state, &session, configured);
        deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
        let bounds = transport.script.begin_bounds.lock().unwrap().clone();
        assert_eq!(
            bounds,
            vec![Duration::from_millis(expected)],
            "configured {configured} ms"
        );
        assert_eq!(abort_retry::prompts_received(&receiver), 1);
    }
}

/// C4 delayed success. With a 5 s configured bound, a coherent begin reply
/// after 2 s and one just under 10 s both arrive within the effective Begin
/// bound and the head is delivered once, with no hold.
#[test]
fn begin_replay_delayed_begin_success_within_the_bound_delivers() {
    for delay in [2_000, 9_900] {
        let script = ReplayScript::default();
        *script.begin_delay.lock().unwrap() = Some(Duration::from_millis(delay));
        let (state, session, receiver, transport) = replay_fixture(script);
        set_call_bound(&state, &session, 5_000);
        deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
        assert_eq!(begins(&transport).len(), 1, "delay {delay} ms");
        assert_eq!(abort_retry::prompts_received(&receiver), 1, "delay {delay} ms");
        assert_eq!(uncertain(&state, &session), None, "delay {delay} ms");
    }
}

fn set_call_bound(state: &AppState, session: &str, millis: u64) {
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(session).unwrap();
    let project_id = inner.sessions[index].session.project_id.clone().unwrap();
    inner
        .projects
        .iter_mut()
        .find(|project| project.id == project_id)
        .unwrap()
        .engram
        .as_mut()
        .unwrap()
        .deadline_ms = Some(millis);
}

/// C4 and C5. A Stop ends the replay immediately: no replay is sent and the
/// prompt is not delivered.
#[test]
fn begin_replay_stop_ends_the_replay() {
    let script = ReplayScript::default();
    script.lose_after_apply.store(1, Ordering::SeqCst);
    let (state, session, receiver, transport) = replay_fixture(script);
    deliver_into_begin_unknown(&state, &session, &receiver, &transport);

    state.request_stop_session(&session).unwrap();
    tick_past_due(&state, &session);
    tick_past_due(&state, &session);

    assert_eq!(begins(&transport).len(), 1, "no replay after Stop");
    assert!(receiver.try_recv().is_err(), "no provider handoff");
}

/// C4 and C5. Cancelling the head ends the replay immediately.
#[test]
fn begin_replay_cancel_ends_the_replay() {
    let script = ReplayScript::default();
    script.lose_after_apply.store(1, Ordering::SeqCst);
    let (state, session, receiver, transport) = replay_fixture(script);
    deliver_into_begin_unknown(&state, &session, &receiver, &transport);
    let head = with_record(&state, &session, |record| {
        record.queued_prompts[0].pending_prompt.id.clone()
    });

    state.cancel_queued_prompt(&session, &head).unwrap();
    tick_past_due(&state, &session);
    tick_past_due(&state, &session);

    assert_eq!(begins(&transport).len(), 1, "no replay after Cancel");
    assert!(receiver.try_recv().is_err(), "no provider handoff");
}

/// C5. A Stop while the replay is in flight: its late receipt cannot deliver
/// the head, and nothing replays it afterwards.
#[test]
fn begin_replay_late_receipt_after_stop_cannot_deliver() {
    let script = ReplayScript::default();
    script.lose_after_apply.store(1, Ordering::SeqCst);
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let (state, session, receiver, transport) = replay_fixture(script);
    deliver_into_begin_unknown(&state, &session, &receiver, &transport);
    *transport.script.hold_replay.lock().unwrap() = Some((entered_tx, release_rx));

    std::thread::scope(|scope| {
        let ticker = scope.spawn(|| tick_past_due(&state, &session));
        // The tick either holds the replay here or ends without sending one.
        let guard = crate::tests::phase_sync::PollGuard::new();
        let replay_in_flight = loop {
            match entered_rx.try_recv() {
                Ok(()) => break true,
                Err(_) if ticker.is_finished() => break entered_rx.try_recv().is_ok(),
                Err(_) => guard.wait("the tick sends its replay or ends"),
            }
        };
        if replay_in_flight {
            state.request_stop_session(&session).unwrap();
            release_tx.send(()).unwrap();
        }
        ticker.join().unwrap();
        assert!(replay_in_flight, "the replay was sent and held in flight");
    });
    tick_past_due(&state, &session);

    assert_eq!(begins(&transport).len(), 2, "one replay, none after Stop");
    assert!(receiver.try_recv().is_err(), "the late receipt delivers nothing");
}

/// C3 warm rebind. A Resume whose status read shows no open grant must not
/// clear the prepared-begin hold on status alone, and never sends a fresh
/// evaluate for it; only the producer's answer to the exact begin settles it.
#[test]
fn begin_replay_resume_never_clears_the_hold_on_status_alone() {
    let script = ReplayScript::default();
    script.lose_after_apply.store(1, Ordering::SeqCst);
    *script.status_open_grant.lock().unwrap() = Some(None);
    let (state, session, receiver, transport) = replay_fixture(script);
    let first = deliver_into_begin_unknown(&state, &session, &receiver, &transport);
    transport.script.all_unavailable.store(true, Ordering::SeqCst);

    let _ = state.resume_session_queue(&session);

    assert_eq!(
        uncertain(&state, &session).as_deref(),
        first["grant_id"].as_str(),
        "status alone never settles a prepared begin"
    );
    assert_eq!(evaluates(&transport).len(), 1, "no fresh evaluate");
    assert_every_begin_is(&transport, &first);
    assert!(receiver.try_recv().is_err());
}
