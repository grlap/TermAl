// Progress witnesses for automatic recovery of an original checkpoint's
// result, each driven by the real retry tick on the fixture's scripted clock:
// - a held Unknown settles exactly and durably within the first retry delay;
// - Busy and lost replies follow the bounded fast series, then the slower
//   cadence without a lifetime stop; an orphan later recovers; a restart
//   during the cooldown does not renew the fast series; two held originals
//   share the tick one call at a time in due order; read-only probe failures
//   consume no send attempt; a full attempt history is held with a visible
//   capacity reason and no send; an Accepted reply whose accounting write
//   failed finishes locally without another send;
// - an independent root progresses while one root's Unknown is held, and the
//   held root resumes after settlement.
// It does not own the recovery fixture helpers (the parent module,
// engram_checkpoint_result_recovery.rs), the races between triggers arriving
// together (the concurrent-trigger recovery tests), interrupted
// acknowledgement or owner drift.

use super::*;

/// How often production's retry tick runs (`ENGRAM_RETRY_TICK`, which the
/// test build does not compile in).
fn production_tick() -> chrono::Duration {
    chrono::Duration::seconds(2)
}

/// The concurred fast series, in seconds before jitter, for durable attempt
/// counts 1 to 7; from the fast limit on, the slow interval applies.
const FAST_SERIES_SECONDS: [i64; 7] = [2, 5, 10, 20, 30, 60, 60];

/// The fixture's scripted Engram clock, its wall reading pinned, so a tick's
/// `now` and every budget the recovery reads move together and only when the
/// test moves them.
fn pinned_clock(turn: &CheckedTurn) -> EngramBudgetClock {
    let clock = turn.state.engram_budget_clock();
    clock.pin_wall();
    clock
}

/// One automatic retry tick at `at`, after moving the scripted clock there.
fn tick_at(turn: &CheckedTurn, clock: &EngramBudgetClock, at: chrono::DateTime<chrono::Utc>) {
    let wall = clock.wall_now();
    if at > wall {
        clock.advance((at - wall).to_std().expect("a forward step"));
    }
    turn.state.engram_abort_retry_tick(at);
}

/// The jittered delay the recovery schedules after `count` durable attempts.
fn retry_delay(original: &EngramCheckpointOperation, count: usize) -> chrono::Duration {
    checkpoint_result_retry_delay(&original.original.id, count as _)
}

fn slow_retry() -> chrono::Duration {
    chrono::Duration::seconds(ENGRAM_CHECKPOINT_SLOW_RETRY_SECONDS as i64)
}

/// Within `base` and `base` plus the existing maximum jitter.
fn assert_jittered(delay: chrono::Duration, base: chrono::Duration, what: &str) {
    let max = base * (100 + ENGRAM_ABORT_RETRY_MAX_JITTER_PERCENT as i32) / 100;
    assert!(delay >= base && delay <= max, "{what}: {delay} outside [{base}, {max}]");
}

/// Ticks one millisecond before `due`, at `due`, and at `due` again: nothing
/// is sent early, exactly one recovery call is sent on time, and the tick
/// that follows it at the same time sends no second call.
fn send_when_due(turn: &CheckedTurn, clock: &EngramBudgetClock, due: chrono::DateTime<chrono::Utc>, sent: usize) {
    tick_at(turn, clock, due - chrono::Duration::milliseconds(1));
    assert_eq!(checkpoints(turn).len(), sent, "nothing is sent before its due time");
    tick_at(turn, clock, due);
    assert_eq!(checkpoints(turn).len(), sent + 1, "one call is sent at its due time");
    tick_at(turn, clock, due);
    assert_eq!(checkpoints(turn).len(), sent + 1, "one call per due time");
}

/// The held original as the live ledger holds it now.
fn canonical(turn: &CheckedTurn, original: &EngramCheckpointOperation) -> EngramCheckpointOperation {
    turn.state.inner.lock().unwrap().engram_session_obligations.checkpoint_operations.iter()
        .find(|operation| operation.original.id == original.original.id).cloned()
        .expect("the original duty is still held")
}

/// Drives the fast series from `start`, the tick that first saw the held
/// Unknown, until the durable attempts reach the fast limit. Each send is
/// checked against the concurred schedule. Returns the time of the last send.
fn exhaust_fast_series(turn: &CheckedTurn, clock: &EngramBudgetClock, original: &EngramCheckpointOperation,
    start: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc> {
    assert_eq!(ENGRAM_CHECKPOINT_FAST_ATTEMPT_LIMIT, FAST_SERIES_SECONDS.len() + 1);
    let mut at = start;
    for count in 1..ENGRAM_CHECKPOINT_FAST_ATTEMPT_LIMIT {
        let delay = retry_delay(original, count);
        assert_jittered(delay, chrono::Duration::seconds(FAST_SERIES_SECONDS[count - 1]), "fast series");
        let due = at + delay;
        send_when_due(turn, clock, due, count);
        at = due;
    }
    assert_eq!(canonical(turn, original).attempts.len(), ENGRAM_CHECKPOINT_FAST_ATTEMPT_LIMIT);
    at
}

/// Alternating Busy (certified not applied) and lost (Unknown) replies for
/// `sends` recovery calls.
fn failed_sends(sends: usize) -> Vec<ScriptedEngramControlResponse> {
    (1..=sends).map(|send| if send % 2 == 1 {
        remote_error_reply("control_store_busy")
    } else {
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::transport("reply lost again")))
    }).collect()
}

/// Each recovery attempt after the original keeps its own truthful result.
fn assert_failed_attempts(operation: &EngramCheckpointOperation, sends: usize) {
    for (index, attempt) in operation.attempts.iter().enumerate().skip(1).take(sends) {
        if index % 2 == 1 {
            assert!(matches!(attempt.result, Some(EngramCheckpointAttemptResult::CertifiedNotApplied { .. })),
                "attempt {index} was Busy");
        } else {
            assert!(matches!(attempt.result, Some(EngramCheckpointAttemptResult::Unknown { .. })),
                "attempt {index} was lost");
        }
    }
    assert!(operation.has_uncertain_checkpoint_history(), "no later failure certifies the original Unknown");
}

/// More reads of the same currently held work authority, one per extra send.
fn more_binding_reads(turn: &CheckedTurn, original: &EngramCheckpointOperation, reads: usize) {
    turn.transport.work_bindings.lock().unwrap().extend(
        (0..reads).map(|_| Ok(original.original.work_binding.clone())));
}

fn settled(turn: &CheckedTurn, original: &EngramCheckpointOperation) -> bool {
    turn.state.inner.lock().unwrap().engram_confirmed_containment_ids.contains(&original.original.id)
}

fn original_held(turn: &CheckedTurn) -> bool {
    engram_session_obligation_locked(&turn.state.inner.lock().unwrap(), &turn.session_id).is_some()
}

fn requests_named(turn: &CheckedTurn, operation: &str) -> usize {
    turn.transport.requests().iter()
        .filter(|request| request.request["operation"] == operation).count()
}

/// Recovery sent no admission operation: each count is what the turns
/// themselves sent.
fn assert_admissions(turn: &CheckedTurn, each: usize) {
    for operation in ["session_bind", "turn_evaluate", "turn_begin"] {
        assert_eq!(requests_named(turn, operation), each, "recovery sends no {operation}");
    }
}

/// Prompts the shared provider runtime received since the last call.
fn delivered_prompts(turn: &CheckedTurn) -> usize {
    std::iter::from_fn(|| turn._runtime_rx.try_recv().ok())
        .filter(|command| matches!(command, CodexRuntimeCommand::Prompt { .. }))
        .count()
}

/// The generic transport-rebind flag a transport failure leaves on the root.
fn rebind_required(turn: &CheckedTurn) -> bool {
    turn.record(|record| record.engram.rebind_required)
}

/// Whether the admission guard still excludes the root's mediated admission.
fn admission_excluded(turn: &CheckedTurn) -> bool {
    require_no_engram_session_obligation(&turn.state.inner.lock().unwrap(), &turn.session_id).is_err()
}

/// The next provider prompt: the session it is for and its text. Without
/// one, the failure names the held root's admission state.
fn next_prompt(turn: &CheckedTurn, phase: &str) -> (String, String) {
    match turn._runtime_rx.recv_timeout(DEADLOCK_GUARD) {
        Ok(CodexRuntimeCommand::Prompt { session_id, command }) => (session_id, command.prompt),
        Ok(_) => panic!("{phase}: expected a provider prompt"),
        Err(error) => {
            let state = turn.record(|record| format!(
                "queued={} engram={:#?} session={:#?}",
                record.queued_prompts.len(), record.engram, record.session));
            panic!("{phase}: no provider prompt ({error}); held root: {state}")
        }
    }
}

/// A second, independent root of `turn`'s project under Engram control,
/// admitted through its own bind, evaluate and begin, with its prompt
/// delivered. It holds no claim, so its one work-binding read is scripted as
/// none and its bind, evaluate and begin take the next scripted replies.
fn independent_root(turn: &CheckedTurn, directory: &str, text: &str) -> String {
    let project_id = turn.record(|record| record.session.project_id.clone().expect("the root belongs to a project"));
    let workdir = turn.root.join(directory);
    fs::create_dir_all(&workdir).expect("the independent root's directory should exist");
    let other = create_test_project_session(&turn.state, Agent::Codex, &project_id,
        &normalize_user_facing_path(&workdir));
    {
        let mut inner = turn.state.inner.lock().unwrap();
        let index = inner.find_session_index(&other).expect("independent root should exist");
        inner.sessions[index].engram.context_nudge_pending = false;
    }
    turn.transport.work_bindings.lock().unwrap().push_front(Ok(None));
    let dispatch = match turn.state.dispatch_turn(&other, SendMessageRequest {
        text: text.to_owned(),
        expanded_text: None,
        attachments: Vec::new(),
        source_session_id: None,
        source_mailbox: None,
    }).expect("the independent root should reach admission") {
        DispatchTurnResult::Dispatched(dispatch) | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
        DispatchTurnResult::Queued => panic!("another root's held checkpoint must not queue this root"),
    };
    deliver_turn_dispatch(&turn.state, dispatch).expect("the independent turn should be delivered");
    let (session, prompt) = next_prompt(turn, "the independent root's prompt");
    assert_eq!(session, other);
    assert!(prompt.contains(text));
    other
}

fn active_grant(turn: &CheckedTurn, session: &str) -> Option<String> {
    let inner = turn.state.inner.lock().unwrap();
    let index = inner.find_session_index(session).expect("the root exists");
    inner.sessions[index].engram.active_grant_id.clone()
}

fn load_canonical(turn: &CheckedTurn, original: &EngramCheckpointOperation) -> (StateInner, EngramCheckpointOperation) {
    turn.state.shutdown_persist_blocking();
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    let canonical = loaded.engram_session_obligations.checkpoint_operations.iter()
        .find(|operation| operation.original.id == original.original.id).cloned()
        .expect("the original duty is durable");
    canonical.validate().unwrap();
    (loaded, canonical)
}

/// Condition: healthy authority and no owner drift. Engram answers each
/// recovery's read-only policy probe, and nothing changes
/// the root, its project or the duty while the schedule runs. Then the first
/// automatic recovery falls due one jittered first retry delay after the tick
/// that first sees the held Unknown, runs on the first production tick at or
/// after that, and settles there.
#[test]
fn automatic_ticks_settle_the_original_checkpoint_within_the_first_retry_delay() {
    let (turn, original) = lost_checkpoint("progress-latency", vec![checkpoint_reply(CHECK_GRANT)]);
    assert!(original_held(&turn));
    let clock = pinned_clock(&turn);
    let triggered = clock.now();
    let mut at = clock.wall_now();
    tick_at(&turn, &clock, at);
    let mut ticks = 0_i64;
    while !settled(&turn, &original) {
        assert_eq!(checkpoints(&turn).len(), 1, "no recovery runs before it is due");
        assert!(ticks < 8, "the recovery never fell due on the production tick");
        at += production_tick();
        ticks += 1;
        tick_at(&turn, &clock, at);
    }
    let elapsed = clock.elapsed_since(triggered);
    let first_delay = retry_delay(&original, 1);
    assert_jittered(first_delay, chrono::Duration::seconds(FAST_SERIES_SECONDS[0]), "first retry");
    let first_delay = first_delay.num_milliseconds();
    let tick = production_tick().num_milliseconds();
    eprintln!(
        "CHECKPOINT_RESULT_RECOVERY_SCRIPTED_ELAPSED elapsed_ms={} first_delay_ms={first_delay} ticks={ticks}",
        elapsed.as_millis()
    );
    assert_eq!(ticks, (first_delay + tick - 1) / tick, "settled on the first tick at or after the due time");
    assert_eq!(elapsed, std::time::Duration::from_millis((ticks * tick) as u64));
    assert!(turn.record(|record| record.engram.active_grant_id.is_none()));
    assert!(!original_held(&turn));
    assert_recovered(&turn, &original);
}

#[test]
fn busy_and_lost_recovery_runs_the_fast_series_then_keeps_the_slow_cadence() {
    let fast = ENGRAM_CHECKPOINT_FAST_ATTEMPT_LIMIT;
    // Two sends past the fast series show the cadence continues.
    let sends = fast + 1;
    let (turn, original) = lost_checkpoint("cadence-slow", failed_sends(sends));
    more_binding_reads(&turn, &original, sends);
    delivered_prompts(&turn);
    let clock = pinned_clock(&turn);
    let start = clock.wall_now();
    tick_at(&turn, &clock, start);
    assert_eq!(checkpoints(&turn).len(), 1, "the first tick only schedules");
    let mut at = exhaust_fast_series(&turn, &clock, &original, start);
    let mut slow = Vec::new();
    for count in fast..=sends {
        let delay = retry_delay(&original, count);
        assert_jittered(delay, slow_retry(), "slow cadence");
        let due = at + delay;
        send_when_due(&turn, &clock, due, count);
        slow.push(delay.num_milliseconds());
        at = due;
    }
    let fast_ms = (1..fast).map(|count| retry_delay(&original, count).num_milliseconds()).collect::<Vec<_>>();
    eprintln!("CHECKPOINT_RESULT_RECOVERY_CADENCE fast_ms={fast_ms:?} slow_ms={slow:?}");
    assert!(turn.state.engram_retry_slots.checkpoint_due.lock().unwrap().contains_key(&original.original.id),
        "the held original stays scheduled: no lifetime stop");
    let sent = checkpoints(&turn);
    assert_eq!(sent.len(), sends + 1);
    assert!(sent.iter().all(|request| request == &sent[0]), "every call replays the original request");
    assert_admissions(&turn, 1);
    assert_eq!(delivered_prompts(&turn), 0, "recovery dispatches nothing to the provider");
    assert_eq!(active_grant(&turn, &turn.session_id).as_deref(), Some(CHECK_GRANT));
    assert!(!settled(&turn, &original));
    assert!(original_held(&turn));
    let (loaded, canonical) = load_canonical(&turn, &original);
    assert_eq!(canonical.attempts.len(), sends + 1);
    assert_failed_attempts(&canonical, sends);
    assert!(!matches!(canonical.progress, EngramExistingCheckpointProgress::Accepted { .. }));
    assert_ne!(canonical.protection.phase, EngramSessionObligationPhase::Confirmed);
    assert!(!loaded.engram_confirmed_containment_ids.contains(&original.original.id));
    assert!(engram_session_obligation_locked(&loaded, &turn.session_id).is_some());
}

#[test]
fn persistent_validation_rejection_keeps_exact_custody_and_refusal_stops_replay_after_reload() {
    const CODE: &str = "environment_fingerprint_mismatch";
    let fast = ENGRAM_CHECKPOINT_FAST_ATTEMPT_LIMIT;
    let rejected_sends = fast + 1;
    let mut replies = (0..rejected_sends).map(|_| remote_error_reply(CODE)).collect::<Vec<_>>();
    replies.push(checkpoint_refusal_reply("evidence_rejected"));
    let (turn, original) = lost_checkpoint("persistent-validation-rejection", replies);
    more_binding_reads(&turn, &original, rejected_sends + 1);
    delivered_prompts(&turn);
    let clock = pinned_clock(&turn);
    let start = clock.wall_now();
    tick_at(&turn, &clock, start);
    let mut at = exhaust_fast_series(&turn, &clock, &original, start);
    for count in fast..=rejected_sends {
        let delay = retry_delay(&original, count);
        assert_jittered(delay, slow_retry(), "persistent rejection slow cadence");
        at += delay;
        send_when_due(&turn, &clock, at, count);
        let current = canonical(&turn, &original);
        assert_eq!(current.attempts.len(), count + 1);
        assert!(current.has_uncertain_checkpoint_history());
        assert_eq!(current.protection.reason_code, "original_checkpoint_unknown");
        assert!(admission_excluded(&turn));
        assert!(!settled(&turn, &original));
    }
    let rejected = canonical(&turn, &original);
    assert_eq!(serde_json::to_value(&rejected.original).unwrap(),
        serde_json::to_value(&original.original).unwrap());
    assert_eq!(serde_json::to_value(&rejected.attempts[0]).unwrap(),
        serde_json::to_value(&original.attempts[0]).unwrap());
    for attempt in &rejected.attempts[1..] {
        assert!(matches!(&attempt.result,
            Some(EngramCheckpointAttemptResult::Unknown { code: Some(code) }) if code == CODE),
            "the repeated validation error is not a non-application certificate");
    }
    {
        let inner = turn.state.inner.lock().unwrap();
        let summary = engram_control_recovery_summaries(&inner).into_iter()
            .find(|summary| summary.session_id == turn.session_id).unwrap();
        assert!(summary.excluded && !summary.settlement_confirmed);
        assert_eq!(summary.code, "original_checkpoint_unknown");
    }
    let delay = retry_delay(&original, rejected.attempts.len());
    assert_jittered(delay, slow_retry(), "refusal is received on the existing slow cadence");
    at += delay;
    send_when_due(&turn, &clock, at, rejected.attempts.len());
    let refused = canonical(&turn, &original);
    assert_eq!(serde_json::to_value(&refused.attempts[..rejected.attempts.len()]).unwrap(),
        serde_json::to_value(&rejected.attempts).unwrap(), "a refusal preserves every earlier invocation");
    assert!(matches!(&refused.attempts.last().unwrap().result,
        Some(EngramCheckpointAttemptResult::Refused { response })
            if response == &json!({ "decision": "refuse", "code": "evidence_rejected" })));
    assert!(refused.has_uncertain_checkpoint_history());
    assert_eq!(refused.protection.reason_code, "original_checkpoint_refused");
    assert_ne!(refused.protection.phase, EngramSessionObligationPhase::Confirmed);
    for intervals in [1, 2, 6] {
        tick_at(&turn, &clock, at + slow_retry() * intervals);
        assert_eq!(serde_json::to_value(canonical(&turn, &original)).unwrap(),
            serde_json::to_value(&refused).unwrap(), "held refusal appends no attempt");
    }
    let sent = checkpoints(&turn);
    assert_eq!(sent.len(), rejected_sends + 2);
    assert!(sent.iter().all(|request| request == &sent[0]), "no changed report, key or bare close");
    assert_admissions(&turn, 1);
    assert_eq!(delivered_prompts(&turn), 0);
    assert_eq!(active_grant(&turn, &turn.session_id).as_deref(), Some(CHECK_GRANT));
    assert!(admission_excluded(&turn));
    let (loaded, durable) = load_canonical(&turn, &original);
    assert_eq!(serde_json::to_value(&durable).unwrap(), serde_json::to_value(&refused).unwrap());
    assert!(engram_session_obligation_locked(&loaded, &turn.session_id).is_some());
    assert!(!loaded.engram_confirmed_containment_ids.contains(&original.original.id));
    let (fresh, transport) = super::effects::fresh_host_at_cut(&turn.state,
        turn.root.join("persistent-refusal-restart.sqlite"), turn.root.clone());
    drop(fresh.engram_retry_slots.release_boot_hold_on_drop());
    for intervals in [1, 2, 6] {
        fresh.engram_checkpoint_result_retry_tick(at + slow_retry() * intervals);
    }
    assert!(transport.requests().is_empty(), "a new host does not re-send the retained Refused result");
    {
        let inner = fresh.inner.lock().unwrap();
        let operation = inner.engram_session_obligations.checkpoint_operations.iter()
            .find(|operation| operation.original.id == original.original.id).unwrap();
        assert_eq!(serde_json::to_value(operation).unwrap(), serde_json::to_value(&refused).unwrap());
        assert!(require_no_engram_session_obligation(&inner, &turn.session_id).is_err());
        assert!(!inner.engram_confirmed_containment_ids.contains(&original.original.id));
        let summary = engram_control_recovery_summaries(&inner).into_iter()
            .find(|summary| summary.session_id == turn.session_id).unwrap();
        assert!(summary.excluded && !summary.settlement_confirmed);
        assert_eq!(summary.code, "original_checkpoint_refused");
    }
    fresh.shutdown_persist_blocking();
    assert!(turn.transport.responses.lock().unwrap().is_empty(), "every scripted rejection was actually returned");
}

/// Condition: healthy authority returns after the fast series. The orphan's
/// first slow-cadence call then settles it, so the progress bound is the fast
/// series plus one slow interval, each with its jitter.
#[test]
fn an_orphan_recovers_automatically_on_the_slow_cadence_after_the_fast_series() {
    let fast = ENGRAM_CHECKPOINT_FAST_ATTEMPT_LIMIT;
    let mut replies = failed_sends(fast - 1);
    replies.push(checkpoint_reply(CHECK_GRANT));
    let (turn, original) = lost_checkpoint("cadence-orphan", replies);
    let clock = pinned_clock(&turn);
    let triggered = clock.now();
    let start = clock.wall_now();
    tick_at(&turn, &clock, start);
    let at = exhaust_fast_series(&turn, &clock, &original, start);
    turn.state.kill_session(&turn.session_id).unwrap();
    assert_eq!(checkpoints(&turn).len(), fast, "administrative removal is not another checkpoint trigger");
    {
        let inner = turn.state.inner.lock().unwrap();
        let summaries = engram_control_recovery_summaries(&inner);
        let summary = summaries.iter().find(|summary| summary.session_id == turn.session_id).unwrap();
        assert!(summary.excluded && !summary.settlement_confirmed);
    }
    let delay = retry_delay(&original, fast);
    assert_jittered(delay, slow_retry(), "slow cadence");
    let due = at + delay;
    tick_at(&turn, &clock, due - chrono::Duration::milliseconds(1));
    assert!(!settled(&turn, &original));
    tick_at(&turn, &clock, due);
    assert!(settled(&turn, &original), "the orphan's slow-cadence call settles it");
    let elapsed = clock.elapsed_since(triggered);
    let expected = (1..=fast).map(|count| retry_delay(&original, count)).fold(chrono::Duration::zero(), |sum, delay| sum + delay);
    eprintln!("CHECKPOINT_RESULT_RECOVERY_ORPHAN_ELAPSED elapsed_ms={} bound_ms={}",
        elapsed.as_millis(), expected.num_milliseconds());
    assert_eq!(elapsed, expected.to_std().unwrap());
    let sent = checkpoints(&turn);
    assert_eq!(sent.len(), fast + 1);
    assert!(sent.iter().all(|request| request == &sent[0]));
    assert_admissions(&turn, 1);
    assert!(engram_control_recovery_summaries(&turn.state.inner.lock().unwrap()).is_empty());
    let (loaded, canonical) = load_canonical(&turn, &original);
    assert!(matches!(canonical.attempts.last().unwrap().result, Some(EngramCheckpointAttemptResult::Accepted)));
    assert!(matches!(canonical.progress, EngramExistingCheckpointProgress::Accepted { .. }));
    assert_eq!(canonical.protection.phase, EngramSessionObligationPhase::Confirmed);
    assert!(loaded.engram_confirmed_containment_ids.contains(&original.original.id));
    assert!(engram_session_obligation_locked(&loaded, &turn.session_id).is_none());
}

#[test]
fn a_restart_during_the_cooldown_keeps_the_slow_cadence() {
    let fast = ENGRAM_CHECKPOINT_FAST_ATTEMPT_LIMIT;
    let mut replies = failed_sends(fast - 1);
    replies.push(checkpoint_reply(CHECK_GRANT));
    let (turn, original) = lost_checkpoint("cadence-restart", replies);
    let clock = pinned_clock(&turn);
    let start = clock.wall_now();
    tick_at(&turn, &clock, start);
    let at = exhaust_fast_series(&turn, &clock, &original, start);
    let (loaded, durable) = load_canonical(&turn, &original);
    assert_eq!(durable.attempts.len(), fast, "the spent fast series is durable");
    let (fresh, transport) = super::effects::fresh_host_at_cut(&turn.state,
        turn.root.join("durable-slow-restart.sqlite"), turn.root.clone());
    drop(fresh.engram_retry_slots.release_boot_hold_on_drop());
    fresh.engram_checkpoint_result_retry_tick(at);
    {
        let schedule = fresh.engram_retry_slots.checkpoint_due.lock().unwrap();
        let entry = schedule.get(&original.original.id).unwrap();
        assert_eq!(entry.preallocation_failures, 0);
        assert_eq!(entry.allocated_attempts, fast);
        assert_jittered(entry.next_at - at, slow_retry(), "new host keeps durable slow floor");
    }
    assert!(transport.requests().is_empty());
    fresh.shutdown_persist_blocking();
    // A restarted host: the whole state reloads and the process-local
    // schedule starts empty.
    let kept = turn.state.engram_budget_clock();
    {
        let mut inner = turn.state.inner.lock().unwrap();
        *inner = loaded;
        inner.engram_budget_clock = kept;
    }
    install_control_only_transport(&turn.state, turn.transport.clone());
    turn.state.engram_retry_slots.checkpoint_due.lock().unwrap().clear();
    let boot = at + chrono::Duration::seconds(60);
    tick_at(&turn, &clock, boot);
    // Past the longest fast delay after boot: the fast series is not renewed.
    let longest_fast = chrono::Duration::seconds(FAST_SERIES_SECONDS[FAST_SERIES_SECONDS.len() - 1])
        * (100 + ENGRAM_ABORT_RETRY_MAX_JITTER_PERCENT as i32) / 100;
    tick_at(&turn, &clock, boot + longest_fast + production_tick());
    assert_eq!(checkpoints(&turn).len(), fast, "no fast call after restart");
    let delay = retry_delay(&original, fast);
    assert_jittered(delay, slow_retry(), "slow cadence after restart");
    send_when_due(&turn, &clock, boot + delay, fast);
    assert!(settled(&turn, &original));
    let current = canonical(&turn, &original);
    assert!(matches!(current.attempts.last().unwrap().result, Some(EngramCheckpointAttemptResult::Accepted)));
    assert_admissions(&turn, 1);
}

/// Cadence-level concurrency only: two held originals in one host. The tick
/// makes at most one call per pass, serves the earlier due first, defers
/// rather than drops a call while every shared slot is taken, and never sends
/// twice for one attempt. The second root holds no claim (see
/// `independent_root`).
#[test]
fn two_held_originals_share_the_tick_one_call_at_a_time_in_due_order() {
    const SECOND_GRANT: &str = "second-held-grant";
    let (turn, first) = lost_checkpoint("cadence-shared", vec![
        bind_reply("second-held-token"),
        grant_reply(SECOND_GRANT),
        begin_reply(SECOND_GRANT),
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::transport("second reply lost"))),
        remote_error_reply("control_store_busy"),
        remote_error_reply("control_store_busy"),
    ]);
    delivered_prompts(&turn);
    let second_session = independent_root(&turn, "second", "Work on the second item.");
    let token = {
        let inner = turn.state.inner.lock().unwrap();
        let index = inner.find_session_index(&second_session).unwrap();
        inner.sessions[index].runtime.runtime_token().expect("the second turn owns the runtime")
    };
    turn.state.finish_turn_ok_if_runtime_matches(&second_session, &token).expect("the second turn completes");
    let second = turn.state.inner.lock().unwrap().engram_session_obligations.checkpoint_operations.iter()
        .find(|operation| operation.protection.local_session_id == second_session).cloned()
        .expect("the second root's checkpoint reply was lost too");
    assert!(second.has_uncertain_checkpoint_history());
    // An unclaimed row of the same confirmed project holds a recoverable
    // original: confirmed authority, and no claim then or now.
    assert!(second.protection.authority_confirmed);
    assert!(second.original.work_binding.is_none());
    for operation in [&first, &second] {
        assert!(EngramCheckpointResultOwner::capture(&turn.state.inner.lock().unwrap(), &operation.original.id).is_some(),
            "both held originals are eligible for recovery");
    }
    assert_eq!(checkpoints(&turn).len(), 2, "each root sent its own original checkpoint");

    let clock = pinned_clock(&turn);
    let start = clock.wall_now();
    let first_due = start + retry_delay(&first, 1);
    let second_due = start + retry_delay(&second, 1);
    // Due order is (due time, original id).
    let (earlier, later) = if (first_due, &first.original.id) <= (second_due, &second.original.id) {
        (&first, &second)
    } else {
        (&second, &first)
    };
    tick_at(&turn, &clock, start);
    let both_due = first_due.max(second_due);
    let held = (0..ENGRAM_RETRY_MAX_IN_FLIGHT)
        .map(|_| turn.state.engram_retry_slots.try_acquire().expect("a free shared slot"))
        .collect::<Vec<_>>();
    tick_at(&turn, &clock, both_due);
    assert_eq!(checkpoints(&turn).len(), 2, "with every shared slot taken the due calls wait");
    drop(held);
    let grant = |operation: &EngramCheckpointOperation| operation.protection.grant_id.clone();
    // The earlier due pass's read-only policy probe fails; the probe answers
    // again before the other duty's pass.
    let roots = turn.transport.named_roots.lock().unwrap().take();
    tick_at(&turn, &clock, both_due);
    *turn.transport.named_roots.lock().unwrap() = roots;
    let sent = checkpoints(&turn);
    assert_eq!(sent.len(), 2, "the earlier due pass fails before allocating or sending");
    assert_eq!(turn.state.engram_retry_slots.checkpoint_due.lock().unwrap()
        .get(&earlier.original.id).unwrap().preallocation_failures, 1);
    tick_at(&turn, &clock, both_due);
    let sent = checkpoints(&turn);
    assert_eq!(sent.len(), 3, "the other due duty progresses on the next tick");
    assert_eq!(sent[2]["grant_id"].as_str(), Some(grant(later).as_str()));
    tick_at(&turn, &clock, both_due);
    assert_eq!(checkpoints(&turn).len(), 3, "both are rescheduled; no busy loop");
    let retry_at = turn.state.engram_retry_slots.checkpoint_due.lock().unwrap()
        .get(&earlier.original.id).unwrap().next_at;
    tick_at(&turn, &clock, retry_at);
    assert_eq!(checkpoints(&turn).len(), 4, "the earlier original recovers its slot once the probe clears");
    assert_eq!(checkpoints(&turn)[3]["grant_id"].as_str(), Some(grant(earlier).as_str()));
    for operation in [&first, &second] {
        assert_eq!(canonical(&turn, operation).attempts.len(), 2, "one recovery attempt each");
    }
    assert_admissions(&turn, 2);
    assert_eq!(delivered_prompts(&turn), 0);
    assert!(turn.transport.responses.lock().unwrap().is_empty(), "every scripted reply went to its own request");
}

#[test]
fn read_only_probe_failures_consume_no_send_attempt() {
    let (turn, original) = lost_checkpoint("cadence-probe", vec![checkpoint_reply(CHECK_GRANT)]);
    // The read-only policy probe fails on every pass until the roots are
    // restored. A failed probe never reaches allocation.
    let roots = turn.transport.named_roots.lock().unwrap().take();
    let clock = pinned_clock(&turn);
    let mut at = clock.wall_now();
    tick_at(&turn, &clock, at);
    for probe in 1..=ENGRAM_CHECKPOINT_FAST_ATTEMPT_LIMIT {
        let due = turn.state.engram_retry_slots.checkpoint_due.lock().unwrap()
            .get(&original.original.id).unwrap().next_at;
        tick_at(&turn, &clock, due - chrono::Duration::milliseconds(1));
        assert!(turn.state.recover_original_checkpoint_before_admission(&turn.session_id, None).is_err());
        assert_eq!(turn.state.engram_retry_slots.checkpoint_due.lock().unwrap()
            .get(&original.original.id).unwrap().preallocation_failures, probe - 1,
            "automatic/pre-admission triggers cannot bypass the due time");
        tick_at(&turn, &clock, due);
        assert_eq!(checkpoints(&turn).len(), 1, "probe failure {probe} sends nothing");
        assert_eq!(canonical(&turn, &original).attempts.len(), 1, "probe failure {probe} allocates no attempt");
        assert!(original_held(&turn));
        let schedule = turn.state.engram_retry_slots.checkpoint_due.lock().unwrap();
        let entry = schedule.get(&original.original.id).unwrap();
        assert_eq!(entry.preallocation_failures, probe);
        assert_eq!(entry.next_at, due + retry_delay(&original, probe));
        if probe == ENGRAM_CHECKPOINT_FAST_ATTEMPT_LIMIT {
            assert_jittered(entry.next_at - due, slow_retry(), "unsent failure backoff");
        }
        at = due;
    }
    // Owner drift/temporary ineligibility must not discard scheduling history.
    {
        let mut inner = turn.state.inner.lock().unwrap();
        let index = inner.find_session_index(&turn.session_id).unwrap();
        inner.sessions[index].hidden = true;
        inner.sessions[index].engram.dispatch_generation += 1;
    }
    tick_at(&turn, &clock, at);
    assert_eq!(turn.state.engram_retry_slots.checkpoint_due.lock().unwrap()
        .get(&original.original.id).unwrap().preallocation_failures, ENGRAM_CHECKPOINT_FAST_ATTEMPT_LIMIT);
    {
        let mut inner = turn.state.inner.lock().unwrap();
        let index = inner.find_session_index(&turn.session_id).unwrap();
        inner.sessions[index].hidden = false;
    }
    *turn.transport.named_roots.lock().unwrap() = roots;
    send_when_due(&turn, &clock, at + retry_delay(&original, ENGRAM_CHECKPOINT_FAST_ATTEMPT_LIMIT), 1);
    assert!(settled(&turn, &original));
    assert!(!turn.state.engram_retry_slots.checkpoint_due.lock().unwrap().contains_key(&original.original.id));
    assert_recovered(&turn, &original);
}

#[test]
fn policy_failures_back_off_and_a_new_host_gets_only_local_fast_grace() {
    let (turn, original) = lost_checkpoint("cadence-policy-restart", vec![]);
    let roots = turn.transport.named_roots.lock().unwrap().take();
    let clock = pinned_clock(&turn);
    let mut at = clock.wall_now();
    tick_at(&turn, &clock, at);
    for failed in 1..=ENGRAM_CHECKPOINT_FAST_ATTEMPT_LIMIT {
        let due = turn.state.engram_retry_slots.checkpoint_due.lock().unwrap()
            .get(&original.original.id).unwrap().next_at;
        tick_at(&turn, &clock, due);
        let schedule = turn.state.engram_retry_slots.checkpoint_due.lock().unwrap();
        let entry = schedule.get(&original.original.id).unwrap();
        assert_eq!(entry.preallocation_failures, failed);
        assert_eq!(entry.next_at, due + retry_delay(&original, failed));
        at = due;
    }
    assert_eq!(canonical(&turn, &original).attempts.len(), 1);
    assert_eq!(checkpoints(&turn).len(), 1);
    assert_eq!(turn.state.engram_retry_slots.in_flight(), 0);
    assert!(turn.transport.work_bindings.lock().unwrap().len() >= 8,
        "original result recovery never reads the current work binding");
    *turn.transport.named_roots.lock().unwrap() = roots;
    turn.state.shutdown_persist_blocking();
    let (fresh, transport) = super::effects::fresh_host_at_cut(&turn.state,
        turn.root.join("preallocation-restart.sqlite"), turn.root.clone());
    assert!(fresh.engram_retry_slots.checkpoint_due.lock().unwrap().is_empty());
    drop(fresh.engram_retry_slots.release_boot_hold_on_drop());
    fresh.engram_checkpoint_result_retry_tick(at);
    let schedule = fresh.engram_retry_slots.checkpoint_due.lock().unwrap();
    let entry = schedule.get(&original.original.id).unwrap();
    assert_eq!(entry.preallocation_failures, 0, "only local failures reset on a new host");
    assert_eq!(entry.allocated_attempts, 1, "the durable lineage was not expanded");
    assert_eq!(entry.next_at, at + retry_delay(&original, 1));
    drop(schedule);
    assert!(transport.requests().is_empty());
    fresh.shutdown_persist_blocking();
}

#[test]
fn a_full_attempt_history_is_held_visibly_without_a_send() {
    let (turn, original) = lost_checkpoint("cadence-capacity", vec![]);
    {
        // A valid lineage that fills the per-original attempt capacity.
        let mut inner = turn.state.inner.lock().unwrap();
        let mut operation = inner.engram_session_obligations.checkpoint_operations.iter()
            .find(|operation| operation.original.id == original.original.id).cloned().unwrap();
        let template = operation.attempts[0].clone();
        while operation.attempts.len() < ENGRAM_CHECKPOINT_ATTEMPT_CAPACITY {
            let mut attempt = template.clone();
            attempt.id = Uuid::new_v4().to_string();
            attempt.predecessor = operation.attempts.last().map(|previous| previous.id.clone());
            attempt.result = Some(EngramCheckpointAttemptResult::CertifiedNotApplied {
                code: "control_store_busy".to_owned(),
            });
            operation.attempts.push(attempt);
        }
        publish_checkpoint_recovery_operation(&mut inner, operation).unwrap();
        turn.state.commit_locked(&mut inner).unwrap();
    }
    let clock = pinned_clock(&turn);
    let start = clock.wall_now();
    tick_at(&turn, &clock, start);
    let delay = retry_delay(&original, ENGRAM_CHECKPOINT_ATTEMPT_CAPACITY);
    assert_jittered(delay, slow_retry(), "slow cadence at capacity");
    tick_at(&turn, &clock, start + delay);
    tick_at(&turn, &clock, start + delay + slow_retry() * 2);
    assert_eq!(checkpoints(&turn).len(), 1, "a full history sends nothing");
    let held = canonical(&turn, &original);
    assert_eq!(held.attempts.len(), ENGRAM_CHECKPOINT_ATTEMPT_CAPACITY, "nothing is appended or discarded");
    assert_eq!(held.protection.reason_code, "original_checkpoint_attempt_capacity");
    assert!(held.has_uncertain_checkpoint_history());
    assert!(original_held(&turn));
    {
        let inner = turn.state.inner.lock().unwrap();
        let summaries = engram_control_recovery_summaries(&inner);
        let summary = summaries.iter().find(|summary| summary.session_id == turn.session_id).unwrap();
        assert!(summary.excluded && !summary.settlement_confirmed, "the held capacity is visible");
    }
    assert_admissions(&turn, 1);
    assert_eq!(delivered_prompts(&turn), 0);
}

/// Runs `action` at the `remaining`-th containment acknowledgement made on
/// this thread; the tick runs its recovery on the calling thread.
fn at_acknowledgement(remaining: usize, action: Box<dyn FnOnce()>) {
    TEST_CONTAINMENT_BEFORE_ACK_RETURN.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            if remaining == 1 { action() } else { at_acknowledgement(remaining - 1, action) }
        }));
    });
}

/// Past the fast series, a slow-cadence call is answered Checkpointed but the
/// transaction recording Accepted fails. The retained Accepted response then
/// finishes local accounting on the accounting delay, not the slow send
/// cadence, and without another network call.
#[test]
fn an_accepted_result_finishes_accounting_without_another_send_after_the_fast_series() {
    let fast = ENGRAM_CHECKPOINT_FAST_ATTEMPT_LIMIT;
    let mut replies = failed_sends(fast - 1);
    replies.push(checkpoint_reply(CHECK_GRANT));
    let (turn, original) = lost_checkpoint("cadence-accounting", replies);
    let clock = pinned_clock(&turn);
    let start = clock.wall_now();
    tick_at(&turn, &clock, start);
    let at = exhaust_fast_series(&turn, &clock, &original, start);
    let path = turn.state.persistence_path.as_path().to_path_buf();
    // The second acknowledgement precedes the send; the Accepted write follows it.
    at_acknowledgement(2, Box::new(move || {
        rusqlite::Connection::open(path).unwrap().execute_batch(
            "CREATE TRIGGER fail_accepted_accounting BEFORE UPDATE ON state_metadata_units
             WHEN NEW.field_name='engramSessionObligations'
             BEGIN SELECT RAISE(ABORT,'fixture Accepted transaction failure'); END;").unwrap();
    }));
    let sent_at = at + retry_delay(&original, fast);
    send_when_due(&turn, &clock, sent_at, fast);
    assert!(!settled(&turn, &original), "the failed Accepted transaction settles nothing");
    assert!(original_held(&turn));
    rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap()
        .execute_batch("DROP TRIGGER fail_accepted_accounting").unwrap();
    let accounting = engram_abort_retry_delay(&original.original.id, 1);
    assert!(accounting < slow_retry(), "accounting does not wait for the send cadence");
    tick_at(&turn, &clock, sent_at + accounting - chrono::Duration::milliseconds(1));
    assert!(!settled(&turn, &original));
    tick_at(&turn, &clock, sent_at + accounting);
    assert!(settled(&turn, &original), "the retained Accepted response finishes locally");
    assert_eq!(checkpoints(&turn).len(), fast + 1, "no network call after the Accepted reply");
    assert_admissions(&turn, 1);
    assert_eq!(delivered_prompts(&turn), 0);
    let (loaded, canonical) = load_canonical(&turn, &original);
    assert!(matches!(canonical.progress, EngramExistingCheckpointProgress::Accepted { .. }));
    assert_eq!(canonical.protection.phase, EngramSessionObligationPhase::Confirmed);
    assert!(loaded.engram_confirmed_containment_ids.contains(&original.original.id));
    assert!(engram_session_obligation_locked(&loaded, &turn.session_id).is_none());
}

#[test]
fn an_independent_root_progresses_while_an_unknown_is_held_and_the_held_root_resumes_after_settlement() {
    const OTHER_GRANT: &str = "independent-root-grant";
    const RESUMED_GRANT: &str = "resumed-root-grant";
    let (turn, original) = lost_checkpoint("progress-independent", vec![
        bind_reply("independent-root-token"),
        grant_reply(OTHER_GRANT),
        begin_reply(OTHER_GRANT),
        checkpoint_reply(CHECK_GRANT),
        // The original checkpoint's lost reply was a transport failure, so
        // the held root's next ordinary admission revalidates its control
        // session (status, then bind) before it evaluates and begins. The
        // claimed root's status read is answered by the named-root model,
        // not from this queue.
        bind_reply("resumed-root-token"),
        grant_reply(RESUMED_GRANT),
        begin_reply(RESUMED_GRANT),
    ]);
    delivered_prompts(&turn);
    assert!(rebind_required(&turn), "the lost original reply was a transport failure");
    let clock = pinned_clock(&turn);
    let held_at = clock.wall_now();
    tick_at(&turn, &clock, held_at);
    assert_eq!(checkpoints(&turn).len(), 1);
    assert!(original_held(&turn));
    assert!(admission_excluded(&turn), "the unsettled original excludes the held root's admission");

    let other = independent_root(&turn, "independent", "Work on the other item.");
    assert_eq!(active_grant(&turn, &other).as_deref(), Some(OTHER_GRANT));
    assert_eq!(checkpoints(&turn).len(), 1, "the independent dispatch triggers no recovery");
    assert!(original_held(&turn));
    assert!(admission_excluded(&turn));
    assert!(!settled(&turn, &original));

    // Recovery phase: the exact original settles, and only its exclusion is
    // released; no admission operation or prompt is part of it.
    tick_at(&turn, &clock, held_at + retry_delay(&original, 1));
    assert!(settled(&turn, &original));
    let sent = checkpoints(&turn);
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], sent[1], "recovery replays the exact original request");
    assert!(!original_held(&turn));
    assert!(!admission_excluded(&turn), "settlement releases the original exclusion");
    assert!(turn.record(|record| record.engram.active_grant_id.is_none()));
    assert!(rebind_required(&turn), "settlement does not clear the generic transport-rebind flag");
    assert_eq!(delivered_prompts(&turn), 0, "settlement itself dispatches nothing");
    assert_admissions(&turn, 2);

    // Next-turn phase, separately requested: healthy authority, the current
    // owner reconciles through the existing rebind path, and no independent
    // duty blocks it.
    begin_check_outcome_turn(&turn);
    let (session, prompt) = next_prompt(&turn, "the resumed root's prompt");
    assert_eq!(session, turn.session_id);
    assert!(prompt.contains("Run the next check."), "the new turn's prompt, not a replayed one");
    assert_eq!(delivered_prompts(&turn), 0, "exactly one new provider prompt");
    assert_eq!(turn.record(|record| record.engram.active_grant_id.clone()).as_deref(), Some(RESUMED_GRANT));
    assert!(requests_named(&turn, "session_status") >= 1, "the resumed turn read its control session's status");
    assert_eq!(requests_named(&turn, "session_bind"), 3, "the resumed turn's ordinary rebind is the only new bind");
    assert_eq!(requests_named(&turn, "turn_evaluate"), 3);
    assert_eq!(requests_named(&turn, "turn_begin"), 3);
    assert!(turn.transport.responses.lock().unwrap().is_empty(), "every scripted reply went to its own request");
}
