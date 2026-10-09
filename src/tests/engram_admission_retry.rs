// Owns the tests of the automatic re-admission of a retained Engram prompt
// parked after an unknown evaluate or an unavailable control: the replay of
// the exact retained intent on the common recovery schedule, without Resume,
// a message or a restart; what stays held; the restart rebuild; and the
// host-wide cap. Does not own the abort retry of a withheld delivery
// (src/tests/engram_abort_retry.rs), the bind retry
// (src/tests/engram_bind_backoff_retry.rs) or the explicit-Resume replay
// (src/tests/engram_root_dispatch.rs). New module, a child of the root
// dispatch tests whose fixtures it uses.
use super::*;

#[cfg(windows)]
fn dedicated_parked_fixture(acp: bool) -> (AppState, String, Arc<ScriptedEngramControlTransport>, crate::tests::delegation_process_cleanup::ProductionWorkerTree) {
    let (mut state, session, _receiver, transport) = root_fixture([bind_reply("retry-token"), deadline_reply()]);
    state.agent_runtime_spawning_enabled = true;
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].session.agent = if acp { Agent::Kimi } else { Agent::Claude };
        inner.sessions[index].session.model = inner.sessions[index].session.agent.default_model().to_owned();
    }
    let cwd = state.test_temp_root.as_ref().unwrap().path().join("dedicated-producer");
    fs::create_dir_all(&cwd).unwrap();
    let owner = crate::tests::delegation_process_cleanup::ProductionWorkerTree::spawn(&state, &session, &cwd, acp);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].session.status = SessionStatus::Idle;
    }
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).expect("unknown evaluate parks before provider delivery");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].session.status = SessionStatus::Idle;
        let authority = AppState::engram_binding_target_for_session_shape_locked(&inner, &session, true)
            .unwrap().map(|target| engram_abort_authority(&target)).unwrap();
        assert_eq!(inner.sessions[index].engram.admission_retry.as_ref().unwrap().authority, authority);
    }
    (state, session, transport, owner)
}

#[cfg(windows)]
#[test]
fn dedicated_rework_model_reset_keeps_parked_retry() {
    let (state, session, transport, owner) = dedicated_parked_fixture(false);
    tick_at(&state, &session, chrono::Utc::now());
    let before = admission_retry(&state, &session).unwrap();
    let requests = transport.requests().len();
    let (claimed_tx, claimed_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    *owner.tree.cleanup_gate.lock().unwrap() = Some(TestStopFenceGate { claimed_tx, release_rx });
    let worker_state = state.clone();
    let worker_session = session.clone();
    let worker = std::thread::spawn(move || worker_state.prepare_dedicated_runtime_reset_off_lock(&worker_session, true));
    crate::tests::phase_sync::receive(&claimed_rx, "model refresh owns its reset fence off lock");
    assert!(state.inner.inner.try_lock().is_ok());
    tick_at(&state, &session, due_time(&before) + chrono::Duration::milliseconds(1));
    let during = admission_retry(&state, &session);
    let during_preview = with_record(&state, &session, |record| record.session.preview.clone());
    release_tx.send(()).unwrap();
    let reset = worker.join().unwrap();
    owner.await_exit();
    let after = admission_retry(&state, &session);
    assert!(owner.tree.is_confirmed());
    drop(owner);
    reset.unwrap();
    assert_eq!(during, Some(before.clone()), "a temporary dedicated reset cannot drop or charge the parked retry");
    assert_eq!(after, Some(before), "successful reset preserves the exact retry");
    assert!(during_preview.starts_with(ENGRAM_ADMISSION_RETRY_PREVIEW_PREFIX));
    assert_eq!(transport.requests().len(), requests, "no admission runs while reset owns cleanup");
}

#[cfg(windows)]
#[derive(Clone, Copy, PartialEq)]
enum DedicatedStopCase { WaitingIntent, WaitingNoIntent, Ordinary, SavedBegin, ActiveGrant }

#[cfg(windows)]
fn dedicated_rework_stop_case(acp: bool, case: DedicatedStopCase) {
    let (state, session, transport, owner) = dedicated_parked_fixture(acp);
    let no_intent = matches!(case, DedicatedStopCase::WaitingNoIntent | DedicatedStopCase::Ordinary);
    let retires = matches!(case, DedicatedStopCase::Ordinary | DedicatedStopCase::SavedBegin | DedicatedStopCase::ActiveGrant);
    let parked_prompt_id = with_record(&state, &session, |record| record.queued_prompts.front().unwrap().pending_prompt.id.clone());
    if case != DedicatedStopCase::Ordinary {
        owner.tree.termination_failures.store(1, std::sync::atomic::Ordering::SeqCst);
        owner.exit_root();
        assert!(with_record(&state, &session, |record| record.runtime.dedicated_cleanup_failure().is_some()));
    }
    assert!(admission_retry(&state, &session).is_some());
    if no_intent {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let head = inner.sessions[index].queued_prompts.front_mut().unwrap();
        assert!(head.promoted_message_index.is_some(), "the parked production head remains promoted");
        head.engram_evaluate = None;
        head.engram_bind = None;
    }
    if retires {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        record.engram.admission_retry = None;
        record.queued_prompts.front_mut().unwrap().engram_interrupted = true;
        if case == DedicatedStopCase::Ordinary {
            record.session.status = SessionStatus::Active;
        } else {
            // A saved begin receipt must still retire through the public Stop.
            record.queued_prompts.front_mut().unwrap().engram_evaluate.as_mut().unwrap().begun_grant_id = Some("saved-begin".to_owned());
            if case == DedicatedStopCase::ActiveGrant {
                record.engram.active_grant_id = Some("saved-begin".to_owned());
                transport.responses.lock().unwrap().push_back(checkpoint_reply("saved-begin"));
            }
        }
    }
    state.request_stop_session(&session).unwrap();
    let had_claim = with_record(&state, &session, |record| record.runtime_stop_in_progress || matches!(record.runtime, SessionRuntime::None));
    if had_claim {
        let guard = crate::tests::phase_sync::PollGuard::new();
        while !with_record(&state, &session, |record| matches!(record.runtime, SessionRuntime::None)) {
            guard.wait("public Stop disposes exact dedicated cleanup owner");
        }
    }
    let confirmed = owner.tree.is_confirmed();
    let stopped = with_record(&state, &session, |record| record.clone());
    // RED fallback is after every observation; it cannot supply Stop's success.
    if !confirmed {
        let (tree, process, label) = stopped.runtime.dedicated_tree_owner().unwrap();
        tree.terminate(&process, label).unwrap();
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].clear_runtime();
    }
    drop(owner);
    assert!(had_claim && confirmed, "canceling admission must also retry the unresolved current tree");
    assert!(matches!(stopped.runtime, SessionRuntime::None));
    if retires {
        assert!(stopped.queued_prompts.is_empty(), "ordinary Stop and a saved begin keep their retirement policy");
        if case == DedicatedStopCase::SavedBegin {
            assert_eq!(stopped.engram.uncertain_grant_id.as_deref(), Some("saved-begin"));
            assert!(stopped.engram.rebind_required, "a saved receipt without an active grant needs status reconciliation");
            assert!(!transport.requests().iter().any(|request| request.request["operation"] == "turn_checkpoint"));
        } else if case == DedicatedStopCase::ActiveGrant {
            assert!(transport.requests().iter().any(|request| request.request["operation"] == "turn_checkpoint" && request.request["grant_id"] == "saved-begin"));
            assert!(stopped.engram.active_grant_id.is_none(), "the active grant is retired by its checkpoint");
        }
    } else {
        assert!(stopped.queued_prompts.front().is_some_and(|head| head.pending_prompt.id == parked_prompt_id), "Stop must retain the never-delivered promoted parked prompt");
        if no_intent {
            assert!(stopped.orchestrator_auto_dispatch_blocked && stopped.queued_prompts.front().is_some_and(|head| head.engram_waiting), "the resumable parked head keeps its explicit hold");
        }
        assert_eq!(stopped.queued_prompts.front().unwrap().engram_interrupted, !no_intent);
    }
    assert!(stopped.engram.admission_retry.is_none());
}

#[cfg(windows)]
#[test]
fn dedicated_rework_stop_cleans_claude_with_parked_admission() { dedicated_rework_stop_case(false, DedicatedStopCase::WaitingIntent); }
#[cfg(windows)]
#[test]
fn dedicated_rework_stop_cleans_acp_with_parked_admission() { dedicated_rework_stop_case(true, DedicatedStopCase::WaitingIntent); }
// An expired initialize deadline is the product's designed cleanup: the writer
// kills the dedicated tree, root socket included. The fixture's root never
// answers initialize, so the fixture keeps that deadline off the wall clock;
// otherwise, under load, it could expire before the stop case first writes
// to the root.
#[cfg(windows)]
#[test]
fn dedicated_rework_acp_initialize_deadline_kills_fake_root() {
    let (_state, _session, _transport, owner) = dedicated_parked_fixture(true);
    owner.expire_initialize();
    owner.await_exit();
    assert!(owner.tree.is_confirmed(), "the failed handshake's cleanup confirms the exact tree");
    owner.assert_peers_exited();
}
#[cfg(windows)]
#[test]
fn dedicated_rework_stop_cleans_owner_with_no_wire_intent() { dedicated_rework_stop_case(false, DedicatedStopCase::WaitingNoIntent); }

#[cfg(windows)]
#[test]
fn dedicated_rework_ordinary_stop_still_retires_promoted_head() { dedicated_rework_stop_case(false, DedicatedStopCase::Ordinary); }
#[cfg(windows)]
#[test]
fn dedicated_rework_cleanup_stop_still_retires_saved_begin() { dedicated_rework_stop_case(false, DedicatedStopCase::SavedBegin); }
#[cfg(windows)]
#[test]
fn dedicated_rework_cleanup_stop_checkpoints_active_grant() { dedicated_rework_stop_case(false, DedicatedStopCase::ActiveGrant); }

#[cfg(windows)]
fn dedicated_rework_retry_cleanup_case(predecessor: bool) {
        let (state, session, transport, owner) = dedicated_parked_fixture(false);
        tick_at(&state, &session, chrono::Utc::now());
        owner.tree.termination_failures.store(1, std::sync::atomic::Ordering::SeqCst);
        owner.exit_root();
        let before = admission_retry(&state, &session).unwrap();
        let requests = transport.requests().len();
        let preview = with_record(&state, &session, |record| record.session.preview.clone());
        let (admission_step, bind_step, mut bind_record) = {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            if predecessor { record.runtime = SessionRuntime::None; }
            let now = due_time(&before) + chrono::Duration::milliseconds(1);
            assert!(!engram_admission_retry_releases(record, Some(&before.authority)), "cleanup cannot release an admission captured by an earlier tick");
            let admission_step = engram_admission_retry_step(record, Some(&before.authority), now, std::time::Instant::now() + Duration::from_secs(3600));
            let mut bind_record = record.clone();
            bind_record.engram.admission_retry = None;
            bind_record.queued_prompts.front_mut().unwrap().engram_evaluate = None;
            let head = bind_record.queued_prompts.front().unwrap();
            bind_record.engram.bind_retry_runtime = bind_record.runtime.runtime_token();
            bind_record.engram.bind_retry = Some(EngramBindRetry {
                proof: EngramBindRetryProof { prompt_id: before.prompt_id.clone(), fingerprint: before.fingerprint.clone(),
                    authority: before.authority.clone(), dispatch_generation: record.engram.dispatch_generation,
                    generation_before_promotion: record.engram.dispatch_generation, promoted_turn_generation: record.active_turn_generation,
                    promotion_index: None, runtime_before_promotion: None, previous_attempts: 0, retry_eligible: true,
                    phase: engram_bind_retry_phase(head) },
                attempts: 1, due_at: before.due_at.clone(), acknowledged: true, held_since: None,
            });
            bind_record.engram.abort_retry_acknowledged = true;
            let bind_step = engram_bind_retry_step(&mut bind_record, Some(&before.authority), now, std::time::Instant::now() + Duration::from_secs(3600));
            (admission_step, bind_step, bind_record)
        };
        tick_at(&state, &session, due_time(&before) + chrono::Duration::milliseconds(1));
        let after_tick = admission_retry(&state, &session);
        if predecessor { owner.tree.termination_failures.store(1, std::sync::atomic::Ordering::SeqCst); }
        let drain = state.dispatch_next_queued_turn(&session, false);
        let held_requests = transport.requests().len();
        let held_preview = with_record(&state, &session, |record| record.session.preview.clone());
        // Always finish the native fixture before assertions, including RED.
        let retained = with_record(&state, &session, |record| record.retained_dedicated_owners[0].clone());
        retained.tree.terminate(&retained.process, retained.label).unwrap();
        let mut confirmed_record = with_record(&state, &session, |record| record.clone());
        let now = due_time(&before) + chrono::Duration::milliseconds(1);
        let confirmed_admission = engram_admission_retry_step(&mut confirmed_record, Some(&before.authority), now, std::time::Instant::now() + Duration::from_secs(3600));
        let confirmed_bind = engram_bind_retry_step(&mut bind_record, Some(&before.authority), now, std::time::Instant::now() + Duration::from_secs(3600));
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].clear_runtime();
        }
        drop(owner);
        assert_eq!(after_tick, Some(before.clone()), "a due production tick retains the held retry");
        if predecessor {
            assert!(drain.is_err(), "the failed predecessor cleanup refuses the drain");
        } else {
            assert!(matches!(drain, Ok(None)), "the held current owner returns no dispatch");
        }
        assert_eq!(held_requests, requests, "tick and drain must issue no external admission");
        assert_eq!(held_preview, preview, "tick and drain preserve the cleanup diagnosis");
        assert_eq!(admission_step, EngramAbortRetryStep::Wait, "cleanup hold must prevent external replay, predecessor={predecessor}");
        assert_eq!(bind_step, EngramAbortRetryStep::Wait, "bind retry must wait for cleanup, predecessor={predecessor}");
        assert_eq!(confirmed_admission, EngramAbortRetryStep::Due, "confirmed cleanup releases the due admission");
        if !predecessor { assert_eq!(confirmed_bind, EngramAbortRetryStep::Due, "confirmed current owner releases the due bind retry"); }
        assert_eq!(admission_retry(&state, &session), Some(before));
        assert_eq!(with_record(&state, &session, |record| record.session.preview.clone()), preview);
        assert_eq!(transport.requests().len(), requests);
}

#[cfg(windows)]
#[test]
fn dedicated_rework_retry_waits_for_current_cleanup() { dedicated_rework_retry_cleanup_case(false); }
#[cfg(windows)]
#[test]
fn dedicated_rework_retry_waits_for_predecessor_cleanup() { dedicated_rework_retry_cleanup_case(true); }

fn deadline_reply() -> ScriptedEngramControlResponse {
    ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
        "evaluate missed its deadline",
    )))
}

fn with_record<T>(state: &AppState, session: &str, read: impl FnOnce(&SessionRecord) -> T) -> T {
    let inner = state.inner.lock().unwrap();
    read(&inner.sessions[inner.find_session_index(session).unwrap()])
}

fn admission_retry(state: &AppState, session: &str) -> Option<EngramAdmissionRetry> {
    with_record(state, session, |record| record.engram.admission_retry.clone())
}

fn due_time(retry: &EngramAdmissionRetry) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(&retry.due_at)
        .unwrap()
        .with_timezone(&chrono::Utc)
}

fn evaluate_keys(transport: &ScriptedEngramControlTransport) -> Vec<Value> {
    transport
        .requests()
        .iter()
        .filter(|request| request.request["operation"] == "turn_evaluate")
        .map(|request| request.request["idempotency_key"].clone())
        .collect()
}

/// Lets the transport failure's bind backoff run out on the state's scripted
/// clock, when one is still pending.
fn let_backoff_pass(state: &AppState, session: &str) {
    let clock = state.engram_budget_clock();
    let pending = with_record(state, session, |record| record.engram.next_bind_retry_at)
        .and_then(|at| at.checked_duration_since(clock.now()));
    if let Some(left) = pending {
        clock.advance(left + Duration::from_millis(1));
    }
}

/// Passes `at` the way the two-second tick does: the backoff has run out,
/// one pass acknowledges the durable retry record and the next starts what
/// is due.
fn tick_at(state: &AppState, session: &str, at: chrono::DateTime<chrono::Utc>) {
    let_backoff_pass(state, session);
    abort_retry::await_settlement_acknowledgement(state, session);
    state.engram_abort_retry_tick(at);
    state.engram_abort_retry_tick(at);
}

/// `tick_at` just past the scheduled attempt, or an hour on when nothing is
/// scheduled.
fn tick_past_due(state: &AppState, session: &str) {
    let at = admission_retry(state, session).map_or_else(
        || chrono::Utc::now() + chrono::Duration::hours(1),
        |retry| due_time(&retry) + chrono::Duration::milliseconds(1),
    );
    tick_at(state, session, at);
}

/// The root session parked after one unknown evaluate, its retry scheduled.
fn parked_after_deadline(
    after: impl IntoIterator<Item = ScriptedEngramControlResponse>,
) -> RootFixture {
    let mut replies = vec![bind_reply("retry-token"), deadline_reply()];
    replies.extend(after);
    let (state, session, receiver, transport) = root_fixture(replies);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false))
        .expect("the unknown admission parks");
    assert!(receiver.try_recv().is_err(), "nothing reached the provider");
    assert!(admission_retry(&state, &session).is_some(), "the retry is scheduled");
    (state, session, receiver, transport)
}

/// An evaluate that misses its deadline before any grant parks the retained
/// prompt. Once the transport recovers, the retry tick re-admits it when due:
/// the exact retained evaluate is replayed with the same key, and the
/// provider receives the prompt exactly once, with no Resume.
#[test]
fn an_unknown_evaluate_is_replayed_automatically_when_due() {
    let (state, session, receiver, transport) = root_fixture([
        bind_reply("retry-token"),
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
            "evaluate missed its deadline",
        ))),
        grant_reply("retried-grant"),
        begin_reply("retried-grant"),
    ]);
    let dispatch = root_dispatch(&state, &session, false);
    deliver_turn_dispatch(&state, dispatch).expect("the unknown admission parks");
    assert!(receiver.try_recv().is_err(), "nothing reached the provider");
    {
        let inner = state.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner.find_session_index(&session).expect("root exists")];
        assert!(record.orchestrator_auto_dispatch_blocked, "the head is parked");
        assert!(
            record.queued_prompts[0].engram_evaluate.is_some(),
            "the evaluate stays retained"
        );
    }

    // Past the first scheduled attempt (2 s plus at most 20% jitter) and the
    // transport failure's bind backoff, with no Resume, message or restart.
    // The tick runs every two seconds: one pass acknowledges the durable
    // retry record, the next starts the replay that is due.
    tick_at(
        &state,
        &session,
        chrono::Utc::now() + chrono::Duration::milliseconds(2_500),
    );
    assert!(
        matches!(
            receiver.try_recv(),
            Ok(CodexRuntimeCommand::Prompt { .. })
        ),
        "the parked prompt is re-admitted automatically when due"
    );
    assert!(receiver.try_recv().is_err(), "exactly once");
    let evaluate_keys = evaluate_keys(&transport);
    assert_eq!(evaluate_keys.len(), 2);
    assert_eq!(
        evaluate_keys[0], evaluate_keys[1],
        "the replay carries the same retained evaluate key"
    );
    assert!(admission_retry(&state, &session).is_none(), "nothing is held any more");
}

/// Criterion 2: each retry outcome (an unknown evaluate, an unavailable,
/// open-circuited, backed-off or exhausted control) enters the schedule on
/// a parked head; every other card code keeps the explicit hold.
#[test]
fn only_retry_outcomes_enter_the_schedule() {
    let (state, session, _receiver, _transport) = parked_after_deadline([]);
    let (record, authority) = {
        let inner = state.inner.lock().unwrap();
        let record = inner.sessions[inner.find_session_index(&session).unwrap()].clone();
        let authority =
            AppState::engram_binding_target_for_session_shape_locked(&inner, &session, true)
                .unwrap()
                .map(|target| engram_abort_authority(&target))
                .unwrap();
        (record, authority)
    };
    let budget_now = state.engram_budget_clock().now();
    for code in [
        "deadline_exceeded",
        "control_unavailable",
        "control_circuit_open",
        "control_backoff",
        "dispatch_budget_exhausted",
    ] {
        let mut parked = record.clone();
        parked.engram.admission_retry = None;
        assert!(
            schedule_engram_admission_retry_locked(
                &mut parked,
                Some(code),
                Some(&authority),
                chrono::Utc::now(),
                budget_now,
            ),
            "{code}"
        );
        let retry = parked.engram.admission_retry.as_ref().unwrap();
        assert_eq!(retry.code, code);
        assert_eq!(retry.attempts, 1);
        assert!(!retry.acknowledged, "{code}: acknowledged only once durable");
    }
    for code in [
        "authorization_unknown",
        "authorization_superseded",
        "begin_grant_mismatch",
        "control_disabled",
        "protocol_error",
    ] {
        let mut parked = record.clone();
        assert!(
            !schedule_engram_admission_retry_locked(
                &mut parked,
                Some(code),
                Some(&authority),
                chrono::Utc::now(),
                budget_now,
            ),
            "{code}"
        );
        assert!(parked.engram.admission_retry.is_none(), "{code}");
        assert_eq!(parked.session.preview, ENGRAM_ADMISSION_HELD_PREVIEW, "{code}");
    }
    // No authority to replay under (Engram no longer admits the session).
    let mut parked = record.clone();
    assert!(!schedule_engram_admission_retry_locked(
        &mut parked,
        Some("deadline_exceeded"),
        None,
        chrono::Utc::now(),
        budget_now,
    ));
}

/// Criterion 2: a transport failure after the evaluate request was persisted
/// (control unavailable) is replayed the same way, with the same key.
#[test]
fn an_unavailable_control_is_replayed_with_the_same_key() {
    let (state, session, receiver, transport) = root_fixture([
        bind_reply("retry-token"),
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::transport(
            "control process unavailable",
        ))),
        grant_reply("retried-grant"),
        begin_reply("retried-grant"),
    ]);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    let retry = admission_retry(&state, &session).expect("the retry is scheduled");
    assert_eq!(retry.code, "control_unavailable");
    tick_past_due(&state, &session);
    assert_eq!(abort_retry::prompts_received(&receiver), 1);
    let keys = evaluate_keys(&transport);
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], keys[1]);
}

/// Criteria 2 and 3: a slow Engram keeps timing out. Each replay sends the
/// same retained key, parks again as the next attempt with the same first-
/// held time, and is due on the schedule (2, 5, 10, 20, 30, then 60 s, each
/// with at most 20% jitter) but never before the bind backoff; the hold says
/// so. Once Engram recovers the prompt reaches the provider exactly once.
#[test]
fn a_slow_engram_is_replayed_on_the_schedule_until_it_recovers() {
    const TIMEOUTS: u32 = 6;
    let mut replies = vec![bind_reply("retry-token")];
    replies.extend((0..TIMEOUTS).map(|_| deadline_reply()));
    replies.extend([grant_reply("late-grant"), begin_reply("late-grant")]);
    let (state, session, receiver, transport) = root_fixture(replies);
    let clock = state.engram_budget_clock();
    let mut held_since = None;
    for attempt in 1..=TIMEOUTS {
        let before = chrono::Utc::now();
        if attempt == 1 {
            deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
        } else {
            tick_past_due(&state, &session);
        }
        let after = chrono::Utc::now();
        assert!(receiver.try_recv().is_err(), "attempt {attempt}: nothing delivered");
        let retry = admission_retry(&state, &session).expect("still scheduled");
        assert_eq!(retry.attempts, attempt, "the attempt index advances by one");
        let held = held_since.get_or_insert_with(|| retry.held_since.clone());
        assert_eq!(&retry.held_since, held, "the first-held time is kept");
        let base_ms = [2_000, 5_000, 10_000, 20_000, 30_000, 60_000]
            [usize::try_from(attempt - 1).unwrap().min(5)];
        let backoff = with_record(&state, &session, |record| record.engram.next_bind_retry_at)
            .and_then(|at| at.checked_duration_since(clock.now()))
            .map_or(chrono::Duration::zero(), |left| {
                chrono::Duration::from_std(left).unwrap()
            });
        let earliest = chrono::Duration::milliseconds(base_ms).max(backoff);
        let latest = chrono::Duration::milliseconds(base_ms * 120 / 100).max(backoff);
        let due = due_time(&retry);
        assert!(due >= before + earliest, "attempt {attempt}: never early");
        assert!(due <= after + latest, "attempt {attempt}: at most 20% jitter");
        with_record(&state, &session, |record| {
            assert_eq!(
                record.session.preview,
                format!(
                    "Engram: waiting for admission; retrying automatically, attempt {attempt}, \
                     next at {}.",
                    retry.due_at
                )
            );
            assert!(record.orchestrator_auto_dispatch_blocked);
        });
    }
    tick_past_due(&state, &session);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "exactly once");
    let keys = evaluate_keys(&transport);
    assert_eq!(keys.len(), usize::try_from(TIMEOUTS).unwrap() + 1);
    assert!(keys.iter().all(|key| key == &keys[0]), "no new key while unresolved");
    with_record(&state, &session, |record| {
        assert!(record.engram.admission_retry.is_none());
        assert!(record.queued_prompts.is_empty());
    });
}

/// Criteria 2 and 3: a Defer returned by a replay ends that evaluation and
/// parks as a Defer does: a new generation, the explicit hold, no schedule.
#[test]
fn a_defer_returned_by_a_replay_parks_as_a_defer_does() {
    let (state, session, receiver, transport) =
        parked_after_deadline([defer_reply("authority_busy")]);
    let generation = with_record(&state, &session, |record| record.engram.dispatch_generation);
    tick_past_due(&state, &session);
    assert!(receiver.try_recv().is_err());
    with_record(&state, &session, |record| {
        assert!(record.engram.admission_retry.is_none());
        assert_eq!(record.engram.dispatch_generation, generation + 1);
        assert_eq!(record.session.preview, ENGRAM_ADMISSION_HELD_PREVIEW);
        assert!(record.queued_prompts[0].engram_waiting);
    });
    let sent = transport.requests().len();
    tick_at(&state, &session, chrono::Utc::now() + chrono::Duration::hours(2));
    assert_eq!(transport.requests().len(), sent, "a Defer is not replayed by this schedule");
}

/// Criterion 2: one attempt per head. While another admission of the head
/// runs, a due tick starts nothing and charges no attempt.
#[test]
fn a_due_tick_starts_no_second_attempt_while_one_is_running() {
    let (state, session, receiver, transport) =
        parked_after_deadline([grant_reply("grant"), begin_reply("grant")]);
    let_backoff_pass(&state, &session);
    abort_retry::await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now());
    let retry = admission_retry(&state, &session).unwrap();
    assert!(retry.acknowledged);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram.admission_in_progress =
            Some(Arc::new(std::sync::atomic::AtomicBool::new(false)));
    }
    let sent = transport.requests().len();
    state.engram_abort_retry_tick(due_time(&retry) + chrono::Duration::seconds(1));
    assert_eq!(transport.requests().len(), sent);
    assert_eq!(admission_retry(&state, &session), Some(retry.clone()), "no attempt charged");
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram.admission_in_progress = None;
    }
    state.engram_abort_retry_tick(due_time(&retry) + chrono::Duration::seconds(1));
    assert_eq!(abort_retry::prompts_received(&receiver), 1);
}

/// Criterion 4: a begin whose reply was lost (the grant possibly begun) is a
/// Reconcile hold, never scheduled.
#[test]
fn a_begin_unknown_keeps_the_explicit_hold() {
    let (state, session, receiver, transport) = root_fixture([
        bind_reply("token"),
        grant_reply("grant"),
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
            "begin reply lost",
        ))),
    ]);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    with_record(&state, &session, |record| {
        assert!(record.engram.uncertain_grant_id.is_some(), "possibly begun");
        assert!(record.engram.admission_retry.is_none());
        assert!(!record
            .session
            .preview
            .starts_with(ENGRAM_ADMISSION_RETRY_PREVIEW_PREFIX));
    });
    let sent = transport.requests().len();
    tick_past_due(&state, &session);
    assert_eq!(transport.requests().len(), sent);
    assert!(receiver.try_recv().is_err());
}

/// Criterion 4: a refusal is not retried.
#[test]
fn a_refusal_keeps_its_hold() {
    let (state, session, receiver, transport) =
        root_fixture([bind_reply("token"), evaluation_refusal_reply("policy_denied")]);
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false))
        .into_public_result()
        .expect_err("a refusal withholds the prompt");
    assert!(admission_retry(&state, &session).is_none());
    let sent = transport.requests().len();
    tick_past_due(&state, &session);
    assert_eq!(transport.requests().len(), sent);
    assert!(receiver.try_recv().is_err());
}

/// Criterion 4: control disabled for the session after a fatal error is not
/// retried.
#[test]
fn a_fatal_disabled_control_keeps_its_hold() {
    let (state, session, receiver, transport) = root_fixture([bind_reply("token")]);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram.disabled_reason = Some("protocol_error".to_owned());
    }
    let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
    assert!(admission_retry(&state, &session).is_none());
    let sent = transport.requests().len();
    tick_past_due(&state, &session);
    assert_eq!(transport.requests().len(), sent);
    assert!(receiver.try_recv().is_err());
}

/// Criterion 4: a cancelled head is never replayed, and the prompt behind it
/// stays behind the paused queue.
#[test]
fn a_cancelled_head_is_never_replayed() {
    let (state, session, receiver, transport) = parked_after_deadline([]);
    queue_test_engram_prompt(
        &state,
        &session,
        "Successor",
        QueuedPromptSource::User,
        None,
    );
    let head = with_record(&state, &session, |record| {
        record.queued_prompts[0].pending_prompt.id.clone()
    });
    state.cancel_queued_prompt(&session, &head).unwrap();
    let sent = transport.requests().len();
    tick_past_due(&state, &session);
    assert_eq!(transport.requests().len(), sent);
    assert!(receiver.try_recv().is_err());
    with_record(&state, &session, |record| {
        assert!(record.engram.admission_retry.is_none());
        assert_eq!(record.queued_prompts.len(), 1);
        assert_eq!(record.queued_prompts[0].pending_prompt.text, "Successor");
        assert!(record.orchestrator_auto_dispatch_blocked);
    });
}

/// Criterion 4: a committed change of the project's admission settings, or
/// of the principal the session is bound as (the host's developer name,
/// which names the connection's actor), drops the schedule; the head keeps
/// the explicit hold.
#[test]
fn a_changed_authority_keeps_the_explicit_hold() {
    for change in ["settings", "principal"] {
        let (state, session, receiver, transport) = parked_after_deadline([]);
        let authority = || {
            let inner = state.inner.lock().unwrap();
            AppState::engram_binding_target_for_session_shape_locked(&inner, &session, true)
                .unwrap()
                .map(|target| engram_abort_authority(&target))
        };
        let before = authority();
        {
            let mut inner = state.inner.lock().unwrap();
            match change {
                "settings" => {
                    let project_id = engram_project_for_session_locked(&inner, &session)
                        .unwrap()
                        .id
                        .clone();
                    inner
                        .projects
                        .iter_mut()
                        .find(|project| project.id == project_id)
                        .unwrap()
                        .engram
                        .as_mut()
                        .unwrap()
                        .deadline_ms = Some(777);
                }
                "principal" => {
                    inner.preferences.engram.developer_name.push_str("-rotated");
                }
                _ => unreachable!(),
            }
        }
        assert_ne!(authority(), before, "{change}: the authority changed");
        let sent = transport.requests().len();
        tick_past_due(&state, &session);
        assert_eq!(transport.requests().len(), sent, "{change}");
        assert!(receiver.try_recv().is_err(), "{change}");
        with_record(&state, &session, |record| {
            assert!(record.engram.admission_retry.is_none(), "{change}");
            assert_eq!(record.session.preview, ENGRAM_ADMISSION_HELD_PREVIEW, "{change}");
            assert!(record.orchestrator_auto_dispatch_blocked, "{change}");
        });
    }
}

/// Criterion 4: an operator's pause (a public Stop) carries its own marker,
/// distinct from the park's pause; no retry passes it, before or after a
/// restart.
#[test]
fn an_operator_pause_keeps_the_hold() {
    let (state, session, receiver, transport) = parked_after_deadline([]);
    with_record(&state, &session, |record| {
        assert!(record.orchestrator_auto_dispatch_blocked, "the park paused the queue");
        assert!(!record.engram.operator_paused, "a park is no operator pause");
    });
    state.request_stop_session(&session).unwrap();
    let sent = transport.requests().len();
    tick_past_due(&state, &session);
    assert_eq!(transport.requests().len(), sent);
    assert!(receiver.try_recv().is_err());
    let saved = with_record(&state, &session, |record| {
        assert!(record.engram.operator_paused);
        assert!(record.engram.admission_retry.is_none());
        assert!(!record
            .session
            .preview
            .starts_with(ENGRAM_ADMISSION_RETRY_PREVIEW_PREFIX));
        PersistedSessionRecord::from_record(record)
    });
    let mut pre_field = serde_json::to_value(&saved).unwrap();
    assert_eq!(pre_field["engramOperatorPaused"], json!(true));
    let loaded = saved.into_record().unwrap();
    assert!(loaded.engram.operator_paused);
    assert!(loaded.engram.admission_retry.is_none());
    assert!(loaded.orchestrator_auto_dispatch_blocked);

    // A record written before the field existed has no such key: it loads
    // as no operator pause, as an unpaused record is saved without it.
    pre_field
        .as_object_mut()
        .unwrap()
        .remove("engramOperatorPaused");
    let older = serde_json::from_value::<PersistedSessionRecord>(pre_field)
        .unwrap()
        .into_record()
        .unwrap();
    assert!(!older.engram.operator_paused);
    let unpaused = with_record(&state, &session, |record| {
        let mut record = record.clone();
        record.engram.operator_paused = false;
        serde_json::to_value(PersistedSessionRecord::from_record(&record)).unwrap()
    });
    assert!(unpaused.get("engramOperatorPaused").is_none());
}

/// Criterion 4: an owner superseded since the park (another head, changed
/// content, or a moved generation) starts nothing and drops the schedule.
#[test]
fn a_superseded_owner_starts_nothing() {
    for change in ["head", "content", "generation"] {
        let (state, session, receiver, transport) = parked_after_deadline([]);
        {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            let record = &mut inner.sessions[index];
            match change {
                "head" => record.queued_prompts[0]
                    .pending_prompt
                    .id
                    .push_str("-successor"),
                "content" => record.queued_prompts[0]
                    .pending_prompt
                    .text
                    .push_str(" changed"),
                "generation" => record.engram.dispatch_generation += 1,
                _ => unreachable!(),
            }
        }
        let sent = transport.requests().len();
        tick_past_due(&state, &session);
        assert_eq!(transport.requests().len(), sent, "{change}");
        assert!(receiver.try_recv().is_err(), "{change}");
        assert!(admission_retry(&state, &session).is_none(), "{change}");
    }
}

/// Criterion 5: a record saved before its acknowledgement loads as the hold;
/// one saved after it is rebuilt, continues from its saved attempt index,
/// and its one replay (the reconciliation) delivers the prompt once, with
/// the original key and no new wake.
#[test]
fn a_restart_rebuilds_an_acknowledged_retry_and_keeps_an_unacknowledged_hold() {
    // After the restart, recovery reads the original control session before
    // the replay: one reconciliation.
    let (state, session, receiver, transport) = parked_after_deadline([
        deadline_reply(),
        status_reply("ready"),
        grant_reply("after-restart"),
        begin_reply("after-restart"),
    ]);
    let unacknowledged =
        with_record(&state, &session, PersistedSessionRecord::from_record);
    assert!(with_record(&state, &session, |record| record
        .session
        .preview
        .starts_with(ENGRAM_ADMISSION_RETRY_PREVIEW_PREFIX)));
    let loaded = unacknowledged.into_record().unwrap();
    assert!(loaded.engram.admission_retry.is_none(), "not yet durable");
    assert!(loaded.orchestrator_auto_dispatch_blocked);
    assert!(loaded.queued_prompts[0].engram_waiting, "still retained");
    assert_eq!(
        loaded.session.preview, ENGRAM_ADMISSION_HELD_PREVIEW,
        "nothing retries it after the restart, so it says it is held"
    );

    // One more timeout before the restart: the second attempt is scheduled.
    tick_past_due(&state, &session);
    let_backoff_pass(&state, &session);
    abort_retry::await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now());
    let before_restart = admission_retry(&state, &session).unwrap();
    assert_eq!(before_restart.attempts, 2);
    assert!(before_restart.acknowledged);
    let prompt_id = with_record(&state, &session, |record| {
        record.queued_prompts[0].pending_prompt.id.clone()
    });

    // The restart: the saved record replaces the live one.
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let saved = PersistedSessionRecord::from_record(&inner.sessions[index]);
        inner.sessions[index] = saved.into_record().unwrap();
        inner.recover_interrupted_sessions();
        let record = &inner.sessions[index];
        let rebuilt = record.engram.admission_retry.as_ref().expect("rebuilt");
        assert_eq!(rebuilt, &before_restart, "the saved attempt index and timers");
        assert!(record.engram.abort_retry_acknowledged);
        assert!(record.orchestrator_auto_dispatch_blocked);
        assert!(record
            .session
            .preview
            .starts_with("Engram: waiting for admission; retrying automatically, attempt 2"));
    }
    state.engram_abort_retry_tick(due_time(&before_restart) + chrono::Duration::milliseconds(1));
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "exactly once");
    let keys = evaluate_keys(&transport);
    assert!(keys.iter().all(|key| key == &keys[0]), "the original key");
    with_record(&state, &session, |record| {
        assert!(record.engram.admission_retry.is_none());
        assert_eq!(
            record
                .session
                .messages
                .iter()
                .filter(|message| message.id() == prompt_id)
                .count(),
            1,
            "no new message or wake"
        );
    });
}

/// Criterion 5: a bind retry left on a retained head after a restart (its
/// live runtime proof is gone) becomes the same durable retry record, and
/// its exact retained bind is replayed when due.
#[test]
fn a_restart_turns_an_acknowledged_bind_retry_into_the_same_replay() {
    let (state, session, receiver, transport) = root_fixture([
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
            "bind reply lost",
        ))),
        bind_reply("rebound"),
        grant_reply("grant"),
        begin_reply("grant"),
    ]);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    abort_retry::await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now());
    let bind = with_record(&state, &session, |record| record.engram.bind_retry.clone())
        .expect("a bind retry");
    assert!(bind.acknowledged);
    let rebuilt = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let saved = PersistedSessionRecord::from_record(&inner.sessions[index]);
        inner.sessions[index] = saved.into_record().unwrap();
        inner.recover_interrupted_sessions();
        let record = &inner.sessions[index];
        assert!(record.engram.bind_retry.is_none(), "no live proof survives");
        record.engram.admission_retry.clone().expect("the same retry record")
    };
    assert_eq!(rebuilt.prompt_id, bind.proof.prompt_id);
    assert_eq!(rebuilt.attempts, bind.attempts);
    assert_eq!(rebuilt.due_at, bind.due_at);
    assert_eq!(Some(&rebuilt.held_since), bind.held_since.as_ref());
    assert!(rebuilt.acknowledged);
    let_backoff_pass(&state, &session);
    state.engram_abort_retry_tick(due_time(&rebuilt) + chrono::Duration::milliseconds(1));
    assert_eq!(abort_retry::prompts_received(&receiver), 1);
    let binds = transport
        .requests()
        .iter()
        .filter(|request| request.request["operation"] == "session_bind")
        .map(|request| request.request.clone())
        .collect::<Vec<_>>();
    assert_eq!(binds.len(), 2);
    assert_eq!(binds[0], binds[1], "the exact retained bind");
}

/// Criterion 6: the cap. With all but one slot held by attempts already in
/// flight, the earliest-due head runs and the other is deferred without its
/// attempt index or first-held time moving; the slot is released when the
/// attempt completes, and the deferred head runs on a later tick.
#[test]
fn the_host_wide_cap_defers_attempts_in_due_order() {
    // One transport per state, answering in request order: both parks, then
    // the earlier replay, then the deferred one.
    let (state, first, receiver, transport) = root_fixture([
        bind_reply("first-token"),
        deadline_reply(),
        bind_reply("second-token"),
        deadline_reply(),
        grant_reply("earlier"),
        begin_reply("earlier"),
        grant_reply("deferred"),
        begin_reply("deferred"),
    ]);
    deliver_turn_dispatch(&state, root_dispatch(&state, &first, false)).unwrap();
    let root = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("second-project");
    fs::create_dir_all(&root).unwrap();
    let project = create_test_project(&state, &root, "Second control");
    let second = create_test_project_session(&state, Agent::Codex, &project, &root);
    enable_test_project_engram(&state, &project, &root);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&second).unwrap();
        inner.sessions[index].engram.context_nudge_pending = false;
    }
    deliver_turn_dispatch(&state, root_dispatch(&state, &second, false)).unwrap();
    for session in [&first, &second] {
        let_backoff_pass(&state, session);
        abort_retry::await_settlement_acknowledgement(&state, session);
    }
    state.engram_abort_retry_tick(chrono::Utc::now());
    let mut parked = [&first, &second].map(|session| {
        let retry = admission_retry(&state, session).expect("both parked");
        assert!(retry.acknowledged);
        (session.clone(), retry)
    });
    parked.sort_by(|left, right| left.1.due_at.cmp(&right.1.due_at));
    let [(earlier, _), (deferred, deferred_retry)] = parked;

    let held = (1..ENGRAM_RETRY_MAX_IN_FLIGHT)
        .map(|_| state.engram_retry_slots.try_acquire().expect("a free slot"))
        .collect::<Vec<_>>();
    let both_due = due_time(&deferred_retry) + chrono::Duration::milliseconds(1);
    state.engram_abort_retry_tick(both_due);
    assert!(admission_retry(&state, &earlier).is_none(), "the earliest due ran");
    assert_eq!(
        admission_retry(&state, &deferred),
        Some(deferred_retry.clone()),
        "the deferred head keeps its attempt index and first-held time"
    );
    assert_eq!(
        state.engram_retry_slots.in_flight(),
        held.len(),
        "the completed attempt released its slot"
    );
    drop(held);
    state.engram_abort_retry_tick(both_due);
    assert!(admission_retry(&state, &deferred).is_none(), "the deferred head ran");
    assert_eq!(state.engram_retry_slots.in_flight(), 0);
    assert_eq!(operations(&transport).len(), 8, "every scripted reply was used");
    let _ = receiver;
}

/// A second Engram session on `state`'s transport, its first dispatch
/// delivered (the transport's next replies answer it).
fn second_session(state: &AppState) -> String {
    extra_session(state, "second")
}

/// Another Engram session, in its own project named after `label`, on
/// `state`'s transport, its first dispatch delivered.
fn extra_session(state: &AppState, label: &str) -> String {
    let root = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join(format!("{label}-project"));
    fs::create_dir_all(&root).unwrap();
    let project = create_test_project(state, &root, &format!("{label} control"));
    let second = create_test_project_session(state, Agent::Codex, &project, &root);
    enable_test_project_engram(state, &project, &root);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&second).unwrap();
        inner.sessions[index].engram.context_nudge_pending = false;
    }
    deliver_turn_dispatch(state, root_dispatch(state, &second, false)).unwrap();
    second
}

/// Both sessions parked, acknowledged and past their backoff; returns the
/// time at which both attempts are due.
fn both_acknowledged(state: &AppState, sessions: [&str; 2]) -> chrono::DateTime<chrono::Utc> {
    for session in sessions {
        let_backoff_pass(state, session);
        abort_retry::await_settlement_acknowledgement(state, session);
    }
    state.engram_abort_retry_tick(chrono::Utc::now());
    sessions
        .iter()
        .map(|session| {
            let retry = admission_retry(state, session).expect("parked");
            assert!(retry.acknowledged);
            due_time(&retry)
        })
        .max()
        .unwrap()
        + chrono::Duration::milliseconds(1)
}

/// Criterion 6 and the shared tick: a concurrent attempt that panics is
/// logged and kept for a later tick; the other attempt completes, every slot
/// is released, and the tick, which also drives carried-run polling, lives.
#[test]
fn a_panicking_concurrent_attempt_does_not_stop_the_tick() {
    let (state, first, receiver, _transport) = root_fixture([
        bind_reply("first-token"),
        deadline_reply(),
        bind_reply("second-token"),
        deadline_reply(),
        grant_reply("survivor"),
        begin_reply("survivor"),
    ]);
    deliver_turn_dispatch(&state, root_dispatch(&state, &first, false)).unwrap();
    let second = second_session(&state);
    let due = both_acknowledged(&state, [&first, &second]);
    let panicking = admission_retry(&state, &first).unwrap();
    let faults = set_engram_retry_test_faults(
        &state,
        EngramRetryTestFaults {
            panic_session: Some(first.clone()),
            ..EngramRetryTestFaults::default()
        },
    );
    state.engram_abort_retry_tick(due);
    drop(faults);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "the other attempt ran");
    assert!(admission_retry(&state, &second).is_none());
    assert_eq!(
        admission_retry(&state, &first),
        Some(panicking),
        "the panicked attempt's record is kept, uncharged"
    );
    assert_eq!(state.engram_retry_slots.in_flight(), 0, "unwinding released its slot");
}

/// The shared tick: when attempt threads cannot be created, the admitted
/// attempts run on the tick's thread, earliest due first.
#[test]
fn attempts_run_on_the_tick_thread_when_no_thread_can_be_created() {
    let (state, first, receiver, transport) = root_fixture([
        bind_reply("first-token"),
        deadline_reply(),
        bind_reply("second-token"),
        deadline_reply(),
        grant_reply("earlier"),
        begin_reply("earlier"),
        grant_reply("later"),
        begin_reply("later"),
    ]);
    deliver_turn_dispatch(&state, root_dispatch(&state, &first, false)).unwrap();
    let second = second_session(&state);
    let due = both_acknowledged(&state, [&first, &second]);
    let faults = set_engram_retry_test_faults(
        &state,
        EngramRetryTestFaults {
            spawn_fails: true,
            ..EngramRetryTestFaults::default()
        },
    );
    state.engram_abort_retry_tick(due);
    drop(faults);
    assert_eq!(abort_retry::prompts_received(&receiver), 2);
    assert!(admission_retry(&state, &first).is_none());
    assert!(admission_retry(&state, &second).is_none());
    assert_eq!(state.engram_retry_slots.in_flight(), 0);
    assert_eq!(operations(&transport).len(), 8, "every scripted reply was used");
}

/// The shared tick: an attempt running on the tick's thread (here the first
/// of two run inline because no thread can be created) that panics does not
/// take the tick down or skip the other attempt; it keeps its record and
/// releases its slot.
#[test]
fn a_panicking_inline_attempt_does_not_stop_the_tick() {
    let (state, first, receiver, _transport) = root_fixture([
        bind_reply("first-token"),
        deadline_reply(),
        bind_reply("second-token"),
        deadline_reply(),
        grant_reply("survivor"),
        begin_reply("survivor"),
    ]);
    deliver_turn_dispatch(&state, root_dispatch(&state, &first, false)).unwrap();
    let second = second_session(&state);
    let due = both_acknowledged(&state, [&first, &second]);
    // Inline attempts run earliest due first: that one panics.
    let mut parked = [&first, &second].map(|session| {
        (session.clone(), admission_retry(&state, session).unwrap())
    });
    parked.sort_by(|left, right| left.1.due_at.cmp(&right.1.due_at));
    let [(panicking, panicking_retry), (survivor, _)] = parked;
    let faults = set_engram_retry_test_faults(
        &state,
        EngramRetryTestFaults {
            spawn_fails: true,
            panic_session: Some(panicking.clone()),
        },
    );
    state.engram_abort_retry_tick(due);
    drop(faults);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "the other attempt ran");
    assert!(admission_retry(&state, &survivor).is_none());
    assert_eq!(admission_retry(&state, &panicking), Some(panicking_retry));
    assert_eq!(state.engram_retry_slots.in_flight(), 0);
}

/// The shared tick: a single due attempt runs on the tick's thread; when it
/// panics the tick returns, the record is kept uncharged and the slot is
/// released, and a later tick admits the prompt.
#[test]
fn a_panicking_single_attempt_does_not_stop_the_tick() {
    let (state, session, receiver, _transport) =
        parked_after_deadline([grant_reply("later"), begin_reply("later")]);
    let_backoff_pass(&state, &session);
    abort_retry::await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now());
    let retry = admission_retry(&state, &session).unwrap();
    assert!(retry.acknowledged);
    let due = due_time(&retry) + chrono::Duration::milliseconds(1);
    let faults = set_engram_retry_test_faults(
        &state,
        EngramRetryTestFaults {
            panic_session: Some(session.clone()),
            ..EngramRetryTestFaults::default()
        },
    );
    state.engram_abort_retry_tick(due);
    drop(faults);
    assert!(receiver.try_recv().is_err());
    assert_eq!(admission_retry(&state, &session), Some(retry), "kept, uncharged");
    assert_eq!(state.engram_retry_slots.in_flight(), 0);
    state.engram_abort_retry_tick(due);
    assert_eq!(abort_retry::prompts_received(&receiver), 1);
}

/// Criteria 6 and 7: the cap covers the bind retry too. With every slot
/// held, a due bind retry keeps its attempt index and due time; once a slot
/// is free it runs.
#[test]
fn the_cap_defers_a_due_bind_retry_without_charging_it() {
    let (state, session, receiver, _transport) = root_fixture([
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
            "bind reply lost",
        ))),
        bind_reply("bound"),
        grant_reply("grant"),
        begin_reply("grant"),
    ]);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    abort_retry::await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now());
    let bind = with_record(&state, &session, |record| record.engram.bind_retry.clone())
        .expect("a bind retry");
    assert!(bind.acknowledged);
    let_backoff_pass(&state, &session);
    let due = chrono::DateTime::parse_from_rfc3339(&bind.due_at)
        .unwrap()
        .with_timezone(&chrono::Utc)
        + chrono::Duration::milliseconds(1);
    let held = (0..ENGRAM_RETRY_MAX_IN_FLIGHT)
        .map(|_| state.engram_retry_slots.try_acquire().expect("a free slot"))
        .collect::<Vec<_>>();
    state.engram_abort_retry_tick(due);
    assert!(receiver.try_recv().is_err());
    let deferred = with_record(&state, &session, |record| record.engram.bind_retry.clone())
        .expect("still scheduled");
    assert_eq!(deferred.attempts, bind.attempts, "no attempt charged");
    assert_eq!(deferred.due_at, bind.due_at, "not moved");
    drop(held);
    state.engram_abort_retry_tick(due);
    assert_eq!(abort_retry::prompts_received(&receiver), 1);
    assert_eq!(state.engram_retry_slots.in_flight(), 0);
}

/// Criterion 5: after a restart whose eager boot recovery left this session
/// unstarted, an acknowledged retry on a head with no wire intent (as a park
/// before the evaluate was prepared leaves it) is not stranded behind the
/// readiness fence. Its due attempt starts the session's lazy boot recovery,
/// waits for it uncharged, and is admitted once the fence is lowered.
#[test]
fn a_restarted_retry_starts_lazy_boot_recovery_instead_of_waiting_for_it() {
    // The lazy recovery reads the original control session and rebinds;
    // then the replay.
    let (state, session, receiver, transport) = parked_after_deadline([
        status_reply("ready"),
        bind_reply("recovered-token"),
        grant_reply("after-recovery"),
        begin_reply("after-recovery"),
    ]);
    let_backoff_pass(&state, &session);
    abort_retry::await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now());
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let mut saved = PersistedSessionRecord::from_record(&inner.sessions[index]);
        // No wire intent on the parked head.
        saved.queued_prompts[0].engram_evaluate = None;
        inner.sessions[index] = saved.into_record().unwrap();
        inner.recover_interrupted_sessions();
        let record = &inner.sessions[index];
        assert!(!record.queued_prompts[0].has_engram_intent());
        assert!(record
            .engram
            .admission_retry
            .as_ref()
            .is_some_and(|retry| retry.acknowledged));
    }
    // The readiness fence is raised, and the eager pass never starts this
    // session (its budget expired first).
    state.prepare_engram_sessions_for_boot_recovery().unwrap();
    assert!(with_record(&state, &session, |record| record.engram_boot_recovery_pending));
    let before = admission_retry(&state, &session).unwrap();
    let due = due_time(&before) + chrono::Duration::milliseconds(1);
    state.engram_abort_retry_tick(due);
    let deadline = std::time::Instant::now() + phase_sync::DEADLOCK_GUARD;
    while with_record(&state, &session, |record| record.engram_boot_recovery_pending) {
        assert!(std::time::Instant::now() < deadline, "the lazy recovery finishes");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        admission_retry(&state, &session).map(|retry| (retry.attempts, retry.due_at)),
        Some((before.attempts, before.due_at.clone())),
        "waiting for the recovery charged no attempt"
    );
    // The recovery then re-kicks the queue on its own thread; that admission
    // finds the paused queue and starts nothing, and while it runs the
    // one-attempt-per-head guard holds the retry back. The two-second tick
    // passes until the replay runs; no pass charges an attempt meanwhile.
    loop {
        match receiver.try_recv() {
            Ok(CodexRuntimeCommand::Prompt { .. }) => break,
            Ok(_) => continue,
            Err(_) => {}
        }
        assert!(std::time::Instant::now() < deadline, "the replay is admitted");
        assert_eq!(
            admission_retry(&state, &session).map(|retry| retry.attempts),
            Some(before.attempts),
            "no attempt is charged before the replay"
        );
        state.engram_abort_retry_tick(due);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(abort_retry::prompts_received(&receiver), 0, "exactly once");
    assert!(admission_retry(&state, &session).is_none());
    assert!(operations(&transport).contains(&"turn_begin".to_owned()));
}

/// Criterion 6: more than K parked heads. With no slot held elsewhere, the K
/// earliest-due attempts run (on the tick's thread here, so the scripted
/// replies are consumed in due order) and the one beyond the cap is deferred
/// with its attempt index and first-held time unchanged; a later tick runs it.
#[test]
fn more_than_k_parked_heads_run_k_at_a_time_in_due_order() {
    let parked_heads = ENGRAM_RETRY_MAX_IN_FLIGHT + 1;
    let mut replies = Vec::new();
    for index in 0..parked_heads {
        replies.push(bind_reply(&format!("token-{index}")));
        replies.push(deadline_reply());
    }
    for _ in 0..parked_heads {
        replies.push(grant_reply("recovered"));
        replies.push(begin_reply("recovered"));
    }
    let (state, first, receiver, _transport) = root_fixture(replies);
    deliver_turn_dispatch(&state, root_dispatch(&state, &first, false)).unwrap();
    let mut sessions = vec![first];
    for index in 1..parked_heads {
        sessions.push(extra_session(&state, &format!("parked-{index}")));
    }
    for session in &sessions {
        let_backoff_pass(&state, session);
        abort_retry::await_settlement_acknowledgement(&state, session);
    }
    state.engram_abort_retry_tick(chrono::Utc::now());
    let mut parked = sessions
        .iter()
        .map(|session| {
            let retry = admission_retry(&state, session).expect("parked");
            assert!(retry.acknowledged);
            (session.clone(), retry)
        })
        .collect::<Vec<_>>();
    parked.sort_by(|left, right| left.1.due_at.cmp(&right.1.due_at));
    let all_due = due_time(&parked.last().unwrap().1) + chrono::Duration::milliseconds(1);
    let faults = set_engram_retry_test_faults(
        &state,
        EngramRetryTestFaults {
            spawn_fails: true,
            ..EngramRetryTestFaults::default()
        },
    );
    state.engram_abort_retry_tick(all_due);
    drop(faults);
    assert_eq!(
        abort_retry::prompts_received(&receiver),
        ENGRAM_RETRY_MAX_IN_FLIGHT,
        "K attempts ran"
    );
    for (session, _) in &parked[..ENGRAM_RETRY_MAX_IN_FLIGHT] {
        assert!(admission_retry(&state, session).is_none(), "an earliest-due head ran");
    }
    let (deferred, deferred_retry) = parked.last().unwrap();
    assert_eq!(
        admission_retry(&state, deferred).as_ref(),
        Some(deferred_retry),
        "the deferred head keeps its attempt index and first-held time"
    );
    assert_eq!(state.engram_retry_slots.in_flight(), 0);
    state.engram_abort_retry_tick(all_due);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "the deferred head ran");
    assert!(admission_retry(&state, deferred).is_none());
}

/// Criterion 6: a restart's lazy boot recovery started by a due attempt runs
/// under the cap. The attempt's slot stays held while the recovery runs and
/// is released when it finishes.
#[test]
fn a_lazy_boot_recovery_started_by_an_attempt_holds_its_slot() {
    let (state, session, _receiver, _transport) = parked_after_deadline([
        status_reply("ready"),
        bind_reply("recovered-token"),
    ]);
    let_backoff_pass(&state, &session);
    abort_retry::await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now());
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let mut saved = PersistedSessionRecord::from_record(&inner.sessions[index]);
        saved.queued_prompts[0].engram_evaluate = None;
        inner.sessions[index] = saved.into_record().unwrap();
        inner.recover_interrupted_sessions();
    }
    state.prepare_engram_sessions_for_boot_recovery().unwrap();
    assert!(with_record(&state, &session, |record| record.engram_boot_recovery_pending));
    let retry = admission_retry(&state, &session).unwrap();
    let gate = BindDispositionGate::new(&state, &session, "lazy_boot_recovery");
    state.engram_abort_retry_tick(due_time(&retry) + chrono::Duration::milliseconds(1));
    gate.wait();
    assert_eq!(
        state.engram_retry_slots.in_flight(),
        1,
        "the recovery holds the attempt's slot while it runs"
    );
    gate.release();
    let deadline = std::time::Instant::now() + phase_sync::DEADLOCK_GUARD;
    while state.engram_retry_slots.in_flight() != 0
        || with_record(&state, &session, |record| record.engram_boot_recovery_pending)
    {
        assert!(std::time::Instant::now() < deadline, "the recovery finishes and releases");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A parked head scheduled for its automatic retry, with no wire intent,
/// after a restart whose readiness fence is raised: the shape the eager boot
/// recovery plan selects (a routing token and no saved bind or evaluate).
fn restarted_retry_head(
    after: impl IntoIterator<Item = ScriptedEngramControlResponse>,
) -> RootFixture {
    let (state, session, receiver, transport) = parked_after_deadline(after);
    let_backoff_pass(&state, &session);
    abort_retry::await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now());
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let mut saved = PersistedSessionRecord::from_record(&inner.sessions[index]);
        saved.queued_prompts[0].engram_evaluate = None;
        inner.sessions[index] = saved.into_record().unwrap();
        inner.recover_interrupted_sessions();
        let record = &inner.sessions[index];
        assert!(record
            .engram
            .admission_retry
            .as_ref()
            .is_some_and(|retry| retry.acknowledged));
    }
    state.prepare_engram_sessions_for_boot_recovery().unwrap();
    assert!(with_record(&state, &session, |record| record.engram_boot_recovery_pending));
    (state, session, receiver, transport)
}

/// Criterion 6: the eager boot recovery reconciles a retry head only under
/// the host-wide cap. With every slot held, it leaves that session fenced
/// and sends it nothing; once a slot is free, the head's own due attempt
/// runs the lazy recovery and the replay delivers the prompt once.
#[test]
fn eager_boot_recovery_leaves_a_retry_head_to_its_attempt_when_the_cap_is_full() {
    let (state, session, receiver, transport) = restarted_retry_head([
        status_reply("ready"),
        bind_reply("recovered-token"),
        grant_reply("after-recovery"),
        begin_reply("after-recovery"),
    ]);
    let held = (0..ENGRAM_RETRY_MAX_IN_FLIGHT)
        .map(|_| state.engram_retry_slots.try_acquire().expect("a free slot"))
        .collect::<Vec<_>>();
    let sent = transport.requests().len();
    state.recover_engram_sessions_after_boot();
    assert_eq!(
        transport.requests().len(),
        sent,
        "the eager pass sends a capped retry head nothing"
    );
    assert!(
        with_record(&state, &session, |record| record.engram_boot_recovery_pending),
        "the retry head stays fenced for its own attempt"
    );
    assert_eq!(state.engram_retry_slots.in_flight(), held.len());
    drop(held);
    let retry = admission_retry(&state, &session).unwrap();
    let due = due_time(&retry) + chrono::Duration::milliseconds(1);
    let deadline = std::time::Instant::now() + phase_sync::DEADLOCK_GUARD;
    loop {
        match receiver.try_recv() {
            Ok(CodexRuntimeCommand::Prompt { .. }) => break,
            Ok(_) => continue,
            Err(_) => {}
        }
        assert!(std::time::Instant::now() < deadline, "the retry head is delivered");
        state.engram_abort_retry_tick(due);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(abort_retry::prompts_received(&receiver), 0, "exactly once");
    assert!(admission_retry(&state, &session).is_none());
}

/// Criterion 6: with a slot free, the eager boot recovery reconciles a retry
/// head itself, holding that slot while its worker runs and releasing it
/// when the worker finishes; the head's due attempt then replays and
/// delivers the prompt once.
#[test]
fn eager_boot_recovery_reconciles_a_retry_head_holding_a_slot() {
    let (state, session, receiver, transport) = restarted_retry_head([
        status_reply("ready"),
        bind_reply("recovered-token"),
        grant_reply("after-recovery"),
        begin_reply("after-recovery"),
    ]);
    let held = (1..ENGRAM_RETRY_MAX_IN_FLIGHT)
        .map(|_| state.engram_retry_slots.try_acquire().expect("a free slot"))
        .collect::<Vec<_>>();
    let sent = transport.requests().len();
    state.recover_engram_sessions_after_boot();
    let deadline = std::time::Instant::now() + phase_sync::DEADLOCK_GUARD;
    while with_record(&state, &session, |record| record.engram_boot_recovery_pending)
        || state.engram_retry_slots.in_flight() != held.len()
    {
        assert!(std::time::Instant::now() < deadline, "the eager recovery finishes");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        operations(&transport)[sent..],
        ["session_status", "session_bind"],
        "the eager pass reconciled the retry head under its slot"
    );
    drop(held);
    assert_eq!(state.engram_retry_slots.in_flight(), 0);
    let retry = admission_retry(&state, &session).unwrap();
    let due = due_time(&retry) + chrono::Duration::milliseconds(1);
    loop {
        match receiver.try_recv() {
            Ok(CodexRuntimeCommand::Prompt { .. }) => break,
            Ok(_) => continue,
            Err(_) => {}
        }
        assert!(std::time::Instant::now() < deadline, "the retry head is delivered");
        state.engram_abort_retry_tick(due);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(abort_retry::prompts_received(&receiver), 0, "exactly once");
    assert!(admission_retry(&state, &session).is_none());
}

/// Criterion 6: an ordinary drain, a slot-less lazy-recovery trigger, never
/// starts the boot reconciliation of a session whose retry record holds its
/// head. With every slot held it starts nothing and the fence stays raised;
/// once a slot is free, the head's own due attempt recovers and delivers.
#[test]
fn an_ordinary_drain_leaves_a_retry_heads_recovery_to_its_attempt() {
    let (state, session, receiver, transport) = restarted_retry_head([
        status_reply("ready"),
        bind_reply("recovered-token"),
        grant_reply("after-recovery"),
        begin_reply("after-recovery"),
    ]);
    let held = (0..ENGRAM_RETRY_MAX_IN_FLIGHT)
        .map(|_| state.engram_retry_slots.try_acquire().expect("a free slot"))
        .collect::<Vec<_>>();
    let sent = transport.requests().len();
    let _ = state.dispatch_next_queued_turn(&session, false);
    with_record(&state, &session, |record| {
        assert!(
            !record.engram_boot_recovery_retry_in_progress,
            "the slot-less drain started no recovery for the retry head"
        );
        assert!(record.engram_boot_recovery_pending, "the fence stays raised");
    });
    assert_eq!(transport.requests().len(), sent, "nothing was sent");
    drop(held);
    let retry = admission_retry(&state, &session).unwrap();
    let due = due_time(&retry) + chrono::Duration::milliseconds(1);
    let deadline = std::time::Instant::now() + phase_sync::DEADLOCK_GUARD;
    loop {
        match receiver.try_recv() {
            Ok(CodexRuntimeCommand::Prompt { .. }) => break,
            Ok(_) => continue,
            Err(_) => {}
        }
        assert!(std::time::Instant::now() < deadline, "the retry head is delivered");
        state.engram_abort_retry_tick(due);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(abort_retry::prompts_received(&receiver), 0, "exactly once");
}

/// Asserts that an ordinary drain of `session`, fenced and holding no retry
/// record, starts its lazy boot recovery at once, as before this item: the
/// recovery is held at its test gate so the in-progress mark is observed.
fn assert_ordinary_drain_starts_lazy_recovery(state: &AppState, session: &str) {
    let gate = BindDispositionGate::new(state, session, "lazy_boot_recovery");
    let _ = state.dispatch_next_queued_turn(session, false);
    gate.wait();
    with_record(state, session, |record| {
        assert!(record.engram.admission_retry.is_none());
        assert!(
            record.engram_boot_recovery_retry_in_progress,
            "the slot-less drain started the lazy recovery"
        );
    });
    gate.release();
    let deadline = std::time::Instant::now() + phase_sync::DEADLOCK_GUARD;
    while with_record(state, session, |record| record.engram_boot_recovery_pending) {
        assert!(std::time::Instant::now() < deadline, "the lazy recovery lowers the fence");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Criterion 6, no stranded fence: once a retry record is dropped while the
/// fence is up (here a committed authority change the tick reads), the head
/// is no longer the retry's, and the next ordinary drain starts the lazy
/// recovery as it always did.
#[test]
fn a_dropped_retry_record_lets_an_ordinary_drain_recover_again() {
    let (state, session, _receiver, _transport) = restarted_retry_head([
        status_reply("ready"),
        bind_reply("recovered-token"),
    ]);
    {
        let mut inner = state.inner.lock().unwrap();
        let project_id = engram_project_for_session_locked(&inner, &session)
            .unwrap()
            .id
            .clone();
        inner
            .projects
            .iter_mut()
            .find(|project| project.id == project_id)
            .unwrap()
            .engram
            .as_mut()
            .unwrap()
            .deadline_ms = Some(777);
    }
    state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::hours(1));
    assert!(admission_retry(&state, &session).is_none(), "the tick dropped the record");
    assert!(with_record(&state, &session, |record| record.engram_boot_recovery_pending));
    assert_ordinary_drain_starts_lazy_recovery(&state, &session);
}

/// Criterion 6, positive control: a fenced session whose park holds no retry
/// record (a known Defer keeps the explicit hold) is recovered by an
/// ordinary drain exactly as before this item.
#[test]
fn an_ordinary_drain_still_recovers_a_session_without_a_retry_record() {
    let (state, session, _receiver, _transport) = root_fixture([
        bind_reply("defer-token"),
        defer_reply("authority_busy"),
        status_reply("ready"),
        bind_reply("recovered-token"),
    ]);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        assert!(inner.sessions[index].engram.admission_retry.is_none(), "a Defer is not scheduled");
        let saved = PersistedSessionRecord::from_record(&inner.sessions[index]);
        inner.sessions[index] = saved.into_record().unwrap();
        inner.recover_interrupted_sessions();
    }
    state.prepare_engram_sessions_for_boot_recovery().unwrap();
    assert!(with_record(&state, &session, |record| record.engram_boot_recovery_pending));
    assert_ordinary_drain_starts_lazy_recovery(&state, &session);
}

/// A delegation record naming `parent` as its parent and a child that does
/// not exist, so a best-effort delegation bind considers only the parent.
fn delegation_with_parent(parent: &str) -> DelegationRecord {
    DelegationRecord {
        id: "delegation-of-a-retry-head".to_owned(),
        parent_session_id: parent.to_owned(),
        child_session_id: "absent-child".to_owned(),
        mode: DelegationMode::Reviewer,
        status: DelegationStatus::Running,
        title: "Reviewer".to_owned(),
        prompt: "Review the patch.".to_owned(),
        cwd: "/tmp".to_owned(),
        agent: Agent::Codex,
        model: None,
        write_policy: DelegationWritePolicy::ReadOnly,
        created_at: stamp_now(),
        started_at: None,
        completed_at: None,
        result: None,
        submitted_review_result: None,
        post_submission_transport_error: None,
        review_result_recovery_probe_attempt: None,
        review_result_recovery_error: None,
        review_result_schema_version: None,
        queued_followup_prompt_id: None,
        review_result_submission_attempt: 0,
        acceptance_evaluation: None,
        attempt: DelegationAttemptState::default(),
    }
}

/// Criterion 6, the retry-head rule: a rebind after a shared runtime's loss
/// is an automatic Engram call on a session holding a retry record, so with
/// every slot held it sends nothing and leaves the rebind owed to the head's
/// attempt.
#[test]
fn a_runtime_loss_rebind_of_a_retry_head_waits_for_a_slot() {
    // The restored shape a plain bind accepts: a retry head with a routing
    // token and no saved bind or evaluate (a head still holding wire intent
    // is never freshly bound).
    let (state, session, _receiver, transport) = restarted_retry_head([
        status_reply("ready"),
        bind_reply("rebound"),
    ]);
    with_record(&state, &session, |record| {
        assert!(!record.queued_prompts[0].has_engram_intent());
        assert!(record.engram.routing_token.is_some());
        assert!(record.engram.admission_retry.is_some());
    });
    let held = (0..ENGRAM_RETRY_MAX_IN_FLIGHT)
        .map(|_| state.engram_retry_slots.try_acquire().expect("a free slot"))
        .collect::<Vec<_>>();
    let sent = transport.requests().len();
    state.rebind_engram_session_after_runtime_loss(&session);
    assert_eq!(
        transport.requests().len(),
        sent,
        "a capped retry head is not rebound"
    );
    assert!(
        with_record(&state, &session, |record| record.engram.admission_retry.is_some()),
        "the head's attempt still owns the reconciliation"
    );
    assert_eq!(state.engram_retry_slots.in_flight(), held.len());
}

/// Criterion 6, the retry-head rule: the best-effort parent bind made while
/// creating a delegation is an automatic Engram call on a session holding a
/// retry record, so with every slot held it sends nothing, keeps the rebind
/// owed, and the delegation goes on as after a best-effort bind failure.
#[test]
fn a_delegation_parent_bind_of_a_retry_head_waits_for_a_slot() {
    // The restored shape a plain bind accepts: a retry head with a routing
    // token and no saved bind or evaluate (a head still holding wire intent
    // is never freshly bound).
    let (state, session, _receiver, transport) = restarted_retry_head([
        status_reply("ready"),
        bind_reply("rebound"),
    ]);
    with_record(&state, &session, |record| {
        assert!(!record.queued_prompts[0].has_engram_intent());
        assert!(record.engram.routing_token.is_some());
        assert!(record.engram.admission_retry.is_some());
    });
    let held = (0..ENGRAM_RETRY_MAX_IN_FLIGHT)
        .map(|_| state.engram_retry_slots.try_acquire().expect("a free slot"))
        .collect::<Vec<_>>();
    let sent = transport.requests().len();
    state.bind_engram_delegation_best_effort(&delegation_with_parent(&session));
    assert_eq!(
        transport.requests().len(),
        sent,
        "a capped retry head parent is not rebound"
    );
    assert!(
        with_record(&state, &session, |record| record.engram.admission_retry.is_some()),
        "the head's attempt still owns the reconciliation"
    );
    assert_eq!(state.engram_retry_slots.in_flight(), held.len());
}

/// A permanent control for the pre-existing exclusion the retry-head rule
/// relies on: a head still holding saved wire intent is never freshly bound
/// by a plain rebind, slots free or not.
#[test]
fn a_plain_rebind_never_binds_a_head_holding_wire_intent() {
    let (state, session, _receiver, transport) = parked_after_deadline([bind_reply("rebound")]);
    assert!(with_record(&state, &session, |record| record.queued_prompts[0].has_engram_intent()));
    assert_eq!(state.engram_retry_slots.in_flight(), 0, "every slot is free");
    let sent = transport.requests().len();
    state.rebind_engram_session_after_runtime_loss(&session);
    state.bind_engram_delegation_best_effort(&delegation_with_parent(&session));
    assert_eq!(transport.requests().len(), sent, "the saved intent is never rebound");
}

/// Criterion 6, positive control: with a slot free, a runtime-loss rebind of
/// a retry head proceeds holding that slot and releases it on return.
#[test]
fn a_runtime_loss_rebind_of_a_retry_head_proceeds_with_a_free_slot() {
    let (state, session, _receiver, transport) = restarted_retry_head([
        status_reply("ready"),
        bind_reply("rebound"),
    ]);
    let sent = transport.requests().len();
    state.rebind_engram_session_after_runtime_loss(&session);
    assert!(transport.requests().len() > sent, "the retry head is rebound under a slot");
    assert_eq!(state.engram_retry_slots.in_flight(), 0, "the slot is released on return");
}

/// Criterion 6, positive control: with a slot free, the best-effort parent
/// bind of a retry head proceeds holding that slot and releases it.
#[test]
fn a_delegation_parent_bind_of_a_retry_head_proceeds_with_a_free_slot() {
    let (state, session, _receiver, transport) = restarted_retry_head([
        status_reply("ready"),
        bind_reply("rebound"),
    ]);
    let sent = transport.requests().len();
    state.bind_engram_delegation_best_effort(&delegation_with_parent(&session));
    assert!(transport.requests().len() > sent, "the retry head parent is bound under a slot");
    assert_eq!(state.engram_retry_slots.in_flight(), 0, "the slot is released on return");
}

/// Criterion 7: an explicit Resume of a fenced retry head starts its lazy
/// boot recovery as before whenever a slot is free, holding that slot.
#[test]
fn a_resume_of_a_fenced_retry_head_starts_its_recovery_with_a_free_slot() {
    let (state, session, _receiver, _transport) = restarted_retry_head([
        status_reply("ready"),
        bind_reply("recovered-token"),
    ]);
    let gate = BindDispositionGate::new(&state, &session, "lazy_boot_recovery");
    let _ = state.resume_session_queue(&session);
    gate.wait();
    with_record(&state, &session, |record| {
        assert!(
            record.engram_boot_recovery_retry_in_progress,
            "the Resume started the lazy recovery"
        );
    });
    assert_eq!(state.engram_retry_slots.in_flight(), 1, "under a retry slot");
    gate.release();
    let deadline = std::time::Instant::now() + phase_sync::DEADLOCK_GUARD;
    while with_record(&state, &session, |record| record.engram_boot_recovery_pending)
        || state.engram_retry_slots.in_flight() != 0
    {
        assert!(std::time::Instant::now() < deadline, "the recovery finishes and releases");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Criteria 6 and 7: with every slot held, an explicit Resume of a fenced
/// retry head starts nothing and leaves the recovery to the head's attempt.
#[test]
fn a_resume_of_a_fenced_retry_head_defers_when_the_cap_is_full() {
    let (state, session, _receiver, transport) = restarted_retry_head([
        status_reply("ready"),
        bind_reply("recovered-token"),
    ]);
    let held = (0..ENGRAM_RETRY_MAX_IN_FLIGHT)
        .map(|_| state.engram_retry_slots.try_acquire().expect("a free slot"))
        .collect::<Vec<_>>();
    let sent = transport.requests().len();
    let _ = state.resume_session_queue(&session);
    with_record(&state, &session, |record| {
        assert!(!record.engram_boot_recovery_retry_in_progress, "no recovery started");
        assert!(record.engram_boot_recovery_pending, "the fence stays raised");
    });
    assert_eq!(transport.requests().len(), sent);
    assert_eq!(state.engram_retry_slots.in_flight(), held.len());
}

/// Criterion 6, due order: once the tick has deferred a due attempt for want
/// of a slot, a call outside the tick does not take the next free slot ahead
/// of it; the next tick gives it to the deferred attempt.
#[test]
fn an_opportunistic_call_does_not_jump_ahead_of_a_deferred_attempt() {
    let (state, session, _receiver, transport) = restarted_retry_head([
        status_reply("ready"),
        bind_reply("recovered-token"),
    ]);
    let mut held = (0..ENGRAM_RETRY_MAX_IN_FLIGHT)
        .map(|_| state.engram_retry_slots.try_acquire().expect("a free slot"))
        .collect::<Vec<_>>();
    let retry = admission_retry(&state, &session).unwrap();
    let due = due_time(&retry) + chrono::Duration::milliseconds(1);
    state.engram_abort_retry_tick(due);
    assert!(with_record(&state, &session, |record| record.engram_boot_recovery_pending));
    // One slot frees; the deferred attempt is still waiting for it.
    held.pop();
    let sent = transport.requests().len();
    state.rebind_engram_session_after_runtime_loss(&session);
    assert_eq!(transport.requests().len(), sent, "the rebind waits behind the deferred attempt");
    assert_eq!(state.engram_retry_slots.in_flight(), held.len());
    // The next tick gives the free slot to the deferred attempt.
    state.engram_abort_retry_tick(due);
    let deadline = std::time::Instant::now() + phase_sync::DEADLOCK_GUARD;
    while with_record(&state, &session, |record| record.engram_boot_recovery_pending) {
        assert!(std::time::Instant::now() < deadline, "the deferred attempt recovers the head");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(transport.requests().len() > sent);
    drop(held);
}

/// Permanent witness (a review question: does an operator pause outlive the
/// Resume that answered it?): a Stop of a running turn leaves the queued work
/// behind it paused with the operator's marker (`stop_session_with_options`).
/// The explicit Resume admits the queued prompt, whose start on the record
/// lifts the pause and the marker with it, before its timed-out evaluate
/// parks; the park is eligible for the automatic retry, which then delivers
/// it with no second Resume.
#[test]
fn a_resume_after_a_stop_makes_a_new_park_retry_automatically() {
    let (state, session, receiver, _transport) = root_fixture([
        bind_reply("retry-token"),
        deadline_reply(),
        grant_reply("after-resume"),
        begin_reply("after-resume"),
    ]);
    queue_test_engram_prompt(
        &state,
        &session,
        "Queued behind a stopped turn",
        QueuedPromptSource::User,
        None,
    );
    {
        // What the Stop leaves on the queued work.
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        record.set_auto_dispatch_blocked(true);
        record.engram.operator_paused = true;
    }
    state.resume_session_queue(&session).unwrap();
    with_record(&state, &session, |record| {
        assert!(!record.engram.operator_paused, "the Resume answered the operator's pause");
        assert!(
            record.engram.admission_retry.is_some(),
            "the new park is scheduled for the automatic retry"
        );
        assert!(record
            .session
            .preview
            .starts_with(ENGRAM_ADMISSION_RETRY_PREVIEW_PREFIX));
    });
    assert!(receiver.try_recv().is_err(), "nothing reached the provider yet");
    tick_past_due(&state, &session);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered without a second Resume");
}

/// Review regression (a retry slot released before a detached delivery's
/// admission): when the retried dispatch goes through Codex Fast discovery,
/// its worker keeps the attempt's slot until it finishes, so with the other
/// slots taken a further automatic attempt stays deferred meanwhile.
#[test]
fn a_fast_discovery_delivery_keeps_its_retry_slot_until_it_finishes() {
    let (state, session, receiver, _transport) = parked_after_deadline([
        grant_reply("fast-grant"),
        begin_reply("fast-grant"),
    ]);
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        // Fast with no model catalogue yet: delivery must discover it first.
        record.session.codex_fast_mode = true;
        record.session.model_options.clear();
    }
    let held = (1..ENGRAM_RETRY_MAX_IN_FLIGHT)
        .map(|_| state.engram_retry_slots.try_acquire().expect("a free slot"))
        .collect::<Vec<_>>();
    tick_past_due(&state, &session);
    let CodexRuntimeCommand::RefreshModelList { response_tx } =
        phase_sync::receive(&receiver, "the retried delivery's Fast discovery")
    else {
        panic!("Fast discovery must precede provider delivery")
    };
    assert_eq!(
        state.engram_retry_slots.in_flight(),
        ENGRAM_RETRY_MAX_IN_FLIGHT,
        "the worker still holds the attempt's slot"
    );
    assert!(
        state.engram_retry_slots.try_acquire().is_none(),
        "a further automatic attempt stays deferred"
    );
    response_tx
        .send(Err("forced catalog failure".to_owned()))
        .unwrap();
    let deadline = std::time::Instant::now() + phase_sync::DEADLOCK_GUARD;
    while state.engram_retry_slots.in_flight() != held.len() {
        assert!(std::time::Instant::now() < deadline, "the worker releases its slot");
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(held);
}

/// Review regression (a retry replayed before boot reconciliation): the
/// retry tick's thread starts before post-listen boot raises the restarted
/// sessions' readiness fences. Until boot preparation has run, a due retry
/// starts nothing; once it has, the retry runs as before.
#[test]
fn no_automatic_retry_starts_before_boot_preparation() {
    let (state, session, receiver, transport) = parked_after_deadline([
        grant_reply("after-boot"),
        begin_reply("after-boot"),
    ]);
    assert!(
        !state.engram_retry_slots.boot_preparation_pending(),
        "construction ran boot preparation, which released the hold"
    );
    state.engram_retry_slots.hold_until_boot_preparation();
    let sent = transport.requests().len();
    tick_past_due(&state, &session);
    assert_eq!(transport.requests().len(), sent, "nothing is sent before boot preparation");
    assert!(receiver.try_recv().is_err());
    assert!(admission_retry(&state, &session).is_some(), "the retry is kept");

    // Boot preparation releases the hold, and the retry runs when due.
    state.prepare_engram_sessions_for_boot_recovery().unwrap();
    assert!(!state.engram_retry_slots.boot_preparation_pending());
    {
        // This fixture's session is no restarted one: it has no fence to
        // recover, so the due attempt replays directly.
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram_boot_recovery_pending = false;
    }
    tick_past_due(&state, &session);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered after boot preparation");
}

/// The boot hold is released on every exit of boot preparation: here its
/// commit fails, and the fences it raised in memory stand, so the automatic
/// retries are not stopped for the life of the process.
#[test]
fn a_failed_boot_preparation_still_releases_the_retry_hold() {
    let (mut state, session, _receiver, _transport) = root_fixture([]);
    {
        // Something for the preparation to commit.
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram_boot_recovery_dispatch_pending = true;
    }
    let failing_persistence_path = state
        .test_temp_root
        .as_ref()
        .unwrap()
        .path()
        .join("boot-preparation-persistence-is-directory");
    fs::create_dir_all(&failing_persistence_path).unwrap();
    state.persistence_path = Arc::new(failing_persistence_path);
    state.engram_retry_slots.hold_until_boot_preparation();
    assert!(state.prepare_engram_sessions_for_boot_recovery().is_err(), "the commit fails");
    assert!(
        !state.engram_retry_slots.boot_preparation_pending(),
        "the hold is released all the same"
    );
}

/// The boot hold is released when boot preparation panics, too (here on a
/// poisoned state mutex).
#[test]
fn a_panicking_boot_preparation_still_releases_the_retry_hold() {
    let (state, _session, _receiver, _transport) = root_fixture([]);
    let poisoner = state.clone();
    assert!(std::thread::spawn(move || {
        let _held = poisoner.inner.lock().unwrap();
        panic!("poison the state mutex");
    })
    .join()
    .is_err());
    state.engram_retry_slots.hold_until_boot_preparation();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        state.prepare_engram_sessions_for_boot_recovery()
    }));
    assert!(outcome.is_err(), "the preparation panicked");
    assert!(!state.engram_retry_slots.boot_preparation_pending());
}

/// A boot with no Engram recovery at all (Engram never enabled, an empty
/// plan) releases the hold the constructor raised before the tick's thread:
/// tests run boot preparation inline in the constructor, after that raise.
#[test]
fn a_boot_without_engram_releases_the_retry_hold() {
    let root = TestTempRoot::create("termal-boot-hold-no-engram");
    // Declared after `root`, so dropped first, panic or not: its persistence
    // is shut down before the temp root is removed.
    let state = home_fixture::BootState::new(
        AppState::new_with_paths(
            "/tmp".to_owned(),
            root.path().join("termal.sqlite"),
            root.path().join("orchestrators.json"),
        )
        .unwrap(),
    );
    assert!(state.inner.lock().unwrap().projects.iter().all(|project| project.engram.is_none()));
    assert!(!state.engram_retry_slots.boot_preparation_pending());
}

/// Review regression (the retry tick racing the eager boot recovery): after
/// the boot hold is released and before the eager pass reaches a restarted
/// retry head, its due attempt may start the session's lazy recovery. The
/// eager pass then leaves that session to the running recovery: it sends it
/// nothing and does not lower its fence, so the session is reconciled once.
#[test]
fn eager_boot_recovery_leaves_a_session_to_its_running_lazy_recovery() {
    let (state, session, _receiver, transport) = restarted_retry_head([
        status_reply("ready"),
        bind_reply("recovered-token"),
        grant_reply("after-recovery"),
        begin_reply("after-recovery"),
    ]);
    let gate = BindDispositionGate::new(&state, &session, "lazy_boot_recovery");
    let retry = admission_retry(&state, &session).unwrap();
    state.engram_abort_retry_tick(due_time(&retry) + chrono::Duration::milliseconds(1));
    gate.wait();
    assert!(with_record(&state, &session, |record| {
        record.engram_boot_recovery_retry_in_progress && record.engram_boot_recovery_pending
    }));
    let sent = transport.requests().len();
    state.recover_engram_sessions_after_boot();
    assert_eq!(
        transport.requests().len(),
        sent,
        "the eager pass sends the session nothing while its lazy recovery runs"
    );
    assert!(
        with_record(&state, &session, |record| record.engram_boot_recovery_pending),
        "the fence stays raised until the running recovery finishes"
    );
    gate.release();
    let deadline = std::time::Instant::now() + phase_sync::DEADLOCK_GUARD;
    while with_record(&state, &session, |record| record.engram_boot_recovery_pending) {
        assert!(std::time::Instant::now() < deadline, "the lazy recovery finishes");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        operations(&transport)
            .iter()
            .filter(|operation| operation.as_str() == "session_bind")
            .count(),
        2,
        "one bind before the park and one reconciliation after the restart"
    );
}

/// The eager claim ends with the recovery that holds it on every exit, even
/// one that finds the fence already down when it finishes (defensive: only
/// the claim holder's own finish lowers it); a claim left standing would keep
/// that session's lazy recovery from ever starting.
#[test]
fn a_finished_recovery_releases_its_claim_even_without_a_fence() {
    let (state, session, _receiver, _transport) = root_fixture([]);
    assert!(
        !state.claim_engram_boot_recovery_for_eager(&session),
        "nothing is owed without a raised fence"
    );
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram_boot_recovery_pending = true;
    }
    assert!(state.claim_engram_boot_recovery_for_eager(&session));
    assert!(
        !state.claim_engram_boot_recovery_for_eager(&session),
        "a claim held keeps any other recovery out"
    );
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].engram_boot_recovery_pending = false;
    }
    state.finish_engram_restart_recovery(
        &session,
        Duration::ZERO,
        Err(EngramTransportError::backoff("nothing to recover")),
    );
    assert!(
        !with_record(&state, &session, |record| record.engram_boot_recovery_retry_in_progress),
        "the finished recovery's claim is released"
    );
}

/// Review regression (an eager target claimed again after its lazy recovery
/// finished): the eager pass consumes the plan boot preparation captured. If
/// a due attempt's lazy recovery has already reconciled a session and lowered
/// its fence by the time the eager pass reaches it, that target is stale and
/// is skipped: no second status or bind.
#[test]
fn eager_boot_recovery_skips_a_session_its_lazy_recovery_already_finished() {
    let (state, session, _receiver, transport) = restarted_retry_head([
        status_reply("ready"),
        bind_reply("recovered-token"),
        grant_reply("after-recovery"),
        begin_reply("after-recovery"),
    ]);
    // The plan as boot preparation captured it, the session's fence raised.
    let plan = state.prepare_engram_sessions_for_boot_recovery().unwrap();
    assert!(plan
        .targets
        .iter()
        .any(|target| target.connection.session_id == session));
    let retry = admission_retry(&state, &session).unwrap();
    state.engram_abort_retry_tick(due_time(&retry) + chrono::Duration::milliseconds(1));
    let deadline = std::time::Instant::now() + phase_sync::DEADLOCK_GUARD;
    while with_record(&state, &session, |record| {
        record.engram_boot_recovery_pending || record.engram_boot_recovery_retry_in_progress
    }) {
        assert!(std::time::Instant::now() < deadline, "the lazy recovery finishes");
        std::thread::sleep(Duration::from_millis(10));
    }
    let binds = |transport: &ScriptedEngramControlTransport| {
        operations(transport)
            .iter()
            .filter(|operation| operation.as_str() == "session_status" || operation.as_str() == "session_bind")
            .count()
    };
    let before = binds(&transport);
    state.recover_prepared_engram_sessions_after_boot(plan);
    assert_eq!(
        binds(&transport),
        before,
        "the eager pass sends a session its lazy recovery reconciled nothing"
    );
    assert!(!with_record(&state, &session, |record| record.engram_boot_recovery_retry_in_progress));
}

/// A restarted retry head whose saved wire intent is gone, its boot recovery
/// done (no fence): the shape a Stop of a no-intent retry head meets.
fn recovered_no_intent_retry_head(
    after: impl IntoIterator<Item = ScriptedEngramControlResponse>,
) -> RootFixture {
    let fixture = restarted_retry_head(after);
    {
        let mut inner = fixture.0.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.1).unwrap();
        inner.sessions[index].engram_boot_recovery_pending = false;
        assert!(!inner.sessions[index].queued_prompts[0].has_engram_intent());
    }
    fixture
}

/// Criteria 4 and 7: a Stop of a retry head with no saved wire intent ends
/// its automatic retry behind the operator's pause and otherwise keeps
/// today's explicit hold: the head stays queued, not interrupted, resumable,
/// with the held preview it had before this item; no tick replays it.
#[test]
fn a_stop_of_a_no_intent_retry_head_keeps_it_resumable() {
    let (state, session, receiver, transport) = recovered_no_intent_retry_head([]);
    let prompt_id = with_record(&state, &session, |record| {
        record.queued_prompts[0].pending_prompt.id.clone()
    });
    state.request_stop_session(&session).unwrap();
    with_record(&state, &session, |record| {
        assert!(record.engram.admission_retry.is_none(), "the automatic retry ends");
        assert!(record.engram.operator_paused, "behind the operator's pause");
        assert!(record.orchestrator_auto_dispatch_blocked, "the queue stays paused");
        let head = &record.queued_prompts[0];
        assert_eq!(head.pending_prompt.id, prompt_id, "the head stays queued");
        assert!(!head.engram_interrupted, "and is not interrupted: it stays resumable");
        assert_eq!(record.session.preview, ENGRAM_ADMISSION_HELD_PREVIEW);
    });
    let sent = transport.requests().len();
    tick_past_due(&state, &session);
    assert_eq!(transport.requests().len(), sent, "no tick replays it");
    assert!(receiver.try_recv().is_err());
}

/// Criteria 4 and 7: after that Stop, an explicit Resume admits the head
/// afresh and delivers it once, as a Resume of the explicit hold always has.
#[test]
fn a_resume_after_stopping_a_no_intent_retry_head_delivers_it() {
    // The restored head first reconciles its retained admission (status and
    // a rebind), then is evaluated and begun afresh.
    let (state, session, receiver, _transport) = recovered_no_intent_retry_head([
        status_reply("ready"),
        bind_reply("after-resume-token"),
        grant_reply("after-resume"),
        begin_reply("after-resume"),
    ]);
    state.request_stop_session(&session).unwrap();
    state.resume_session_queue(&session).unwrap();
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "delivered once after the Resume");
}

/// Review regression (the retries holding up the test-run index): the
/// test-run index tick no longer drives the automatic retries, which run on a
/// tick thread of their own (`spawn_engram_retry_tick`). A retry already due
/// is not replayed by it, so a slow Engram cannot delay carried-gate polling.
#[test]
fn the_test_run_index_tick_runs_no_automatic_retry() {
    let (state, session, receiver, transport) = parked_after_deadline([
        grant_reply("after-tick"),
        begin_reply("after-tick"),
    ]);
    let_backoff_pass(&state, &session);
    abort_retry::await_settlement_acknowledgement(&state, &session);
    state.engram_abort_retry_tick(chrono::Utc::now());
    {
        // Due already, on the real clock the index tick reads.
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let retry = inner.sessions[index].engram.admission_retry.as_mut().unwrap();
        assert!(retry.acknowledged);
        retry.due_at = (chrono::Utc::now() - chrono::Duration::seconds(1))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    }
    let sent = transport.requests().len();
    state.engram_host().test_run_tick();
    assert_eq!(transport.requests().len(), sent, "the index tick sends Engram nothing");
    assert!(receiver.try_recv().is_err());
    assert!(admission_retry(&state, &session).is_some(), "the retry waits for its own tick");
    // Its own tick replays it.
    state.engram_abort_retry_tick(chrono::Utc::now());
    assert_eq!(abort_retry::prompts_received(&receiver), 1);
}
