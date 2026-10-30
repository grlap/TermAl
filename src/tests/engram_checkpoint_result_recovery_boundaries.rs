// Boundary witnesses for original-checkpoint result recovery, a child of
// engram_checkpoint_result_recovery.rs using its lost-checkpoint fixture. Owns
// concurrent triggers during an in-flight replay, accounting that resumes after
// an interrupted local acknowledgement, owner/successor/independent drift
// while a recovered reply returns, and a same-id adopted enclosing duty that
// must keep its exclusion through the original's acknowledgement and a reload,
// with live and actual SQLite custody. Does not own the recovery implementation
// (src/engram_checkpoint_result_recovery.rs) or the parent's happy-path,
// restart, orphan and reader-floor cases. New file, not split from another.
use super::*;

type ReplayHook = Box<dyn FnOnce() + Send>;

/// Delegates to the scripted transport. On the first TurnCheckpoint replay it
/// runs `before` while the request is on the wire, and `after` once the
/// scripted reply has been produced but before it is returned to recovery.
struct ReplayHookTransport {
    inner: Arc<ScriptedEngramControlTransport>,
    before: Mutex<Option<ReplayHook>>,
    after: Mutex<Option<ReplayHook>>,
    after_probe: Mutex<Option<ReplayHook>>,
}

impl EngramControlTransport for ReplayHookTransport {
    fn request(&self, connection: &EngramConnectionConfig, request: &EngramControlRequest,
        timeout: Duration) -> std::result::Result<Value, EngramTransportError> {
        let checkpoint = matches!(request, EngramControlRequest::TurnCheckpoint { .. });
        if checkpoint && let Some(hook) = self.before.lock().unwrap().take() {
            hook();
        }
        let reply = self.inner.request(connection, request, timeout);
        if checkpoint && reply.is_ok() && let Some(hook) = self.after.lock().unwrap().take() {
            hook();
        }
        reply
    }

    // Recovery's one read-only preflight probe: `after_probe` runs once it
    // has answered and before recovery revalidates its owner.
    fn read_observation_policy(&self, connection: &EngramConnectionConfig, store: &EngramAuthorityStoreKey,
        timeout: Duration, host_workdir: &FsPath) -> Result<EngramObservationPolicyBasis, EngramTransportError> {
        let policy = self.inner.read_observation_policy(connection, store, timeout, host_workdir);
        let hook = self.after_probe.lock().unwrap().take();
        if let Some(hook) = hook { hook(); }
        policy
    }

    fn read_work_binding(&self, connection: &EngramConnectionConfig, preference: EngramBindingPreference<'_>,
        timeout: Duration) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.inner.read_work_binding(connection, preference, timeout)
    }

    fn read_work_binding_for_boot(&self, connection: &EngramConnectionConfig, preference: EngramBindingPreference<'_>,
        timeout: Duration) -> std::result::Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.inner.read_work_binding_for_boot(connection, preference, timeout)
    }

    fn read_held_claims(&self, connection: &EngramConnectionConfig, timeout: Duration)
        -> std::result::Result<EngramHeldClaims, EngramTransportError> {
        self.inner.read_held_claims(connection, timeout)
    }

    fn shutdown_session(&self, session_id: &str) {
        self.inner.shutdown_session(session_id)
    }
}

fn install_replay_hooks(turn: &CheckedTurn, before: Option<ReplayHook>, after: Option<ReplayHook>) {
    install_control_only_transport(&turn.state, Arc::new(ReplayHookTransport {
        inner: turn.transport.clone(),
        before: Mutex::new(before),
        after: Mutex::new(after),
        after_probe: Mutex::new(None),
    }));
}

fn install_probe_hook(turn: &CheckedTurn, hook: ReplayHook) {
    install_control_only_transport(&turn.state, Arc::new(ReplayHookTransport {
        inner: turn.transport.clone(), before: Mutex::new(None), after: Mutex::new(None),
        after_probe: Mutex::new(Some(hook)),
    }));
}

#[test]
fn successful_probes_then_owner_drift_accumulate_unsent_backoff() {
    let (turn, original) = lost_checkpoint("preflight-owner-drift", vec![checkpoint_reply(CHECK_GRANT)]);
    let clock = turn.state.engram_budget_clock();
    clock.pin_wall();
    let mut at = clock.wall_now();
    turn.state.engram_checkpoint_result_retry_tick(at);
    for failed in 1..=3 {
        let (state, session) = (turn.state.clone(), turn.session_id.clone());
        install_probe_hook(&turn, Box::new(move || {
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].engram.dispatch_generation += 1;
        }));
        let due = turn.state.engram_retry_slots.checkpoint_due.lock().unwrap()
            .get(&original.original.id).unwrap().next_at;
        clock.advance((due - at).to_std().unwrap());
        turn.state.engram_checkpoint_result_retry_tick(due);
        let schedule = turn.state.engram_retry_slots.checkpoint_due.lock().unwrap();
        let entry = schedule.get(&original.original.id).unwrap();
        assert_eq!(entry.preallocation_failures, failed,
            "successful probes followed by owner drift are not progress");
        assert_eq!(entry.next_at, due + checkpoint_result_retry_delay(&original.original.id, failed));
        drop(schedule);
        assert_eq!(canonical(&turn, &original).attempts.len(), 1);
        assert_eq!(checkpoints(&turn).len(), 1);
        assert_eq!(turn.state.engram_retry_slots.in_flight(), 0);
        at = due;
    }
    install_control_only_transport(&turn.state, turn.transport.clone());
    let due = turn.state.engram_retry_slots.checkpoint_due.lock().unwrap()
        .get(&original.original.id).unwrap().next_at;
    clock.advance((due - at).to_std().unwrap());
    turn.state.engram_checkpoint_result_retry_tick(due);
    assert_recovered(&turn, &original);
}

#[test]
fn delayed_preflight_failure_cannot_overwrite_a_concurrent_accepted_result() {
    let (turn, original) = lost_checkpoint("preflight-concurrent-accepted", vec![checkpoint_reply(CHECK_GRANT)]);
    // The outer pass pauses after its preflight probe, before revalidating
    // its owner. The nested pass settles the original meanwhile, so the outer
    // pass's late owner check fails without sending or rescheduling.
    let (state, id) = (turn.state.clone(), original.original.id.clone());
    install_probe_hook(&turn, Box::new(move || {
        assert!(state.recover_original_checkpoint_result(&id).unwrap());
    }));
    assert!(turn.state.recover_original_checkpoint_result(&original.original.id).is_err());
    assert!(!turn.state.engram_retry_slots.checkpoint_due.lock().unwrap().contains_key(&original.original.id));
    assert_eq!(turn.state.engram_retry_slots.in_flight(), 0);
    assert_recovered(&turn, &original);
}

/// Bind, Evaluate and Begin requests: recovery may never add one.
fn admission_requests(turn: &CheckedTurn) -> usize {
    turn.transport.requests().iter().filter(|request|
        matches!(request.request["operation"].as_str(), Some("session_bind" | "turn_evaluate" | "turn_begin"))).count()
}

fn canonical(turn: &CheckedTurn, original: &EngramCheckpointOperation) -> EngramCheckpointOperation {
    turn.state.inner.lock().unwrap().engram_session_obligations.checkpoint_operations.iter()
        .find(|operation| operation.original.id == original.original.id).unwrap().clone()
}

fn latched(turn: &CheckedTurn, id: &str) -> bool {
    turn.state.inner.lock().unwrap().engram_confirmed_containment_ids.contains(id)
}

/// Queues a real ordinary user prompt behind the session's queue and commits
/// it as an enqueue does; returns the queued record's full serialized identity.
fn queue_successor(state: &AppState, session: &str, text: &str) -> Value {
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
    let index = inner.find_session_index(session).unwrap();
    queue_prompt_on_record(inner.session_mut_by_index(index).unwrap(), prompt, Vec::new());
    state.commit_locked(&mut inner).unwrap();
    serde_json::to_value(inner.sessions[index].queued_prompts.back().unwrap()).unwrap()
}

/// Every queued record of the session, fully serialized, in queue order.
fn queue_of(inner: &StateInner, session: &str) -> Vec<Value> {
    inner.sessions[inner.find_session_index(session).unwrap()].queued_prompts.iter()
        .map(|head| serde_json::to_value(head).unwrap()).collect()
}

fn queue(turn: &CheckedTurn) -> Vec<Value> {
    queue_of(&turn.state.inner.lock().unwrap(), &turn.session_id)
}

/// The persisted state as SQLite holds it now. Test states have no persist
/// worker, so every commit has already been saved synchronously.
fn durable(turn: &CheckedTurn) -> StateInner {
    load_state(turn.state.persistence_path.as_path()).unwrap().unwrap()
}

fn durable_operation<'a>(loaded: &'a StateInner, original: &EngramCheckpointOperation) -> &'a EngramCheckpointOperation {
    loaded.engram_session_obligations.checkpoint_operations.iter()
        .find(|operation| operation.original.id == original.original.id).unwrap()
}

#[test]
fn concurrent_recovery_triggers_during_an_in_flight_replay_send_one_exact_replay() {
    let (turn, original) = lost_checkpoint("boundary-concurrent", vec![checkpoint_reply(CHECK_GRANT)]);
    let admissions = admission_requests(&turn);
    let outcomes = Arc::new(Mutex::new(Vec::new()));
    let (state, id, seen) = (turn.state.clone(), original.original.id.clone(), outcomes.clone());
    install_replay_hooks(&turn, Some(Box::new(move || {
        // The first replay is on the wire. A trigger on another thread, one on
        // this thread and the automatic retry tick must all find it owned.
        let other = {
            let (state, id) = (state.clone(), id.clone());
            std::thread::spawn(move || format!("{:?}", state.recover_original_checkpoint_result(&id).map_err(|error| error.to_string())))
                .join().unwrap()
        };
        let same = format!("{:?}", state.recover_original_checkpoint_result(&id).map_err(|error| error.to_string()));
        state.engram_abort_retry_tick(chrono::Utc::now() + chrono::Duration::hours(1));
        seen.lock().unwrap().extend([other, same]);
    })), None);
    assert!(turn.state.recover_original_checkpoint_result(&original.original.id).unwrap(),
        "the owning recovery settles the original checkpoint");
    let outcomes = outcomes.lock().unwrap().clone();
    assert_eq!(outcomes.len(), 2);
    for outcome in &outcomes {
        assert_ne!(outcome, "Ok(true)", "only the owning recovery may settle: {outcomes:?}");
    }
    assert_eq!(admission_requests(&turn), admissions, "recovery never binds, evaluates or begins");
    // Exactly the lost original and one replay, byte-identical, and the
    // persisted canonical operation confirmed through its own receipt.
    assert_recovered(&turn, &original);
}

#[test]
fn accepted_result_with_interrupted_local_acknowledgement_completes_without_another_send() {
    const VERIFICATION: &str = "abababababababababababababababababababababababababababababababab";
    const CHECKPOINT: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";
    // The replayed receipt settles the original report's one test check, so the
    // receipt effects in the Confirmed cut are real rather than vacuous.
    let (turn, original) = lost_checkpoint("boundary-interrupted-ack", vec![ScriptedEngramControlResponse::Reply(Ok(json!({
        "decision": "checkpointed", "receipt": {
            "grant_id": CHECK_GRANT, "cursor": 3, "confirmed_cursor": 3,
            "checkpoint": CHECKPOINT, "verification_evidence": [VERIFICATION]
        }
    })))]);
    let observation = {
        let EngramControlRequest::TurnCheckpoint { report, .. } = &original.original.request else { unreachable!() };
        assert_eq!(report.verification_evidence.len(), 1, "the original report carries one test check");
        let EngramObservationReference::ObservationId { observation_id } =
            &report.verification_evidence[0].producer_observation;
        let inner = turn.state.inner.lock().unwrap();
        let row = inner.test_run_observations.values()
            .find(|row| row.evidence.iter().any(|item| &item.producer_observation_id == observation_id))
            .expect("the check's observation carries its association");
        assert!(row.evidence.iter().all(|item| item.verification_record_id.is_none()), "unsettled before recovery");
        row.execution_id.clone()
    };
    // Exactly the settled association the receipt effects write.
    let settled = |inner: &StateInner| {
        let row = &inner.test_run_observations[&observation];
        assert_eq!(row.evidence.len(), 1);
        assert_eq!(row.evidence[0].verification_record_id.as_deref(), Some(VERIFICATION));
        assert_eq!(row.evidence[0].checkpoint_id.as_deref(), Some(CHECKPOINT));
        assert_eq!(row.evidence[0].confirmed_cursor, Some(3));
        serde_json::to_value(&row.evidence).unwrap()
    };
    // A real successor waits behind the exclusion. Recovery may never consume,
    // reorder or rewrite it, whether the acknowledgement fails or completes.
    let successor = queue_successor(&turn.state, &turn.session_id, "successor waiting behind the recovered checkpoint");
    assert_eq!(queue(&turn), vec![successor.clone()]);
    let generation = turn.record(|record| record.engram.dispatch_generation);
    let admissions = admission_requests(&turn);
    let (clock, timeout) = (turn.state.engram_budget_clock(), original.original.settings.call_timeout());
    // The reply is authoritative, but the call's deadline passes before
    // recovery sees it: the result is retained and its acknowledgement fails.
    install_replay_hooks(&turn, None, Some(Box::new(move || clock.advance(timeout + Duration::from_secs(1)))));
    assert!(turn.state.recover_original_checkpoint_result(&original.original.id).is_err(),
        "an acknowledgement past its deadline must not report settlement");
    let interrupted = canonical(&turn, &original);
    assert!(matches!(interrupted.progress, EngramExistingCheckpointProgress::Accepted { .. }),
        "the authoritative result stays in canonical custody");
    assert_eq!(interrupted.protection.phase, EngramSessionObligationPhase::Confirmed);
    assert!(matches!(interrupted.attempts.last().unwrap().result, Some(EngramCheckpointAttemptResult::Accepted)));
    {
        let inner = turn.state.inner.lock().unwrap();
        assert!(inner.engram_pending_checkpoint_receipts.entries.get(&checkpoint_receipt_key(&interrupted))
            .is_some_and(|pending| pending.accepted), "the exact accepted receipt keeps its custody");
        assert!(engram_session_obligation_locked(&inner, &turn.session_id).is_some(), "the exclusion is still held");
    }
    assert!(!latched(&turn, &original.original.id));
    assert_eq!(checkpoints(&turn).len(), 2);
    assert_eq!(queue(&turn), vec![successor.clone()], "the failed acknowledgement leaves the successor exactly as queued");
    // Confirmed and the receipt effects are one atomic publication.
    let live_evidence = settled(&*turn.state.inner.lock().unwrap());
    // The actual SQLite cut before the continuation: a durable Confirmed implies
    // durable effects, so a reload materializes the latch and releases the
    // exclusion only together with those effects.
    {
        let cut = durable(&turn);
        let stored = durable_operation(&cut, &original);
        assert_eq!(serde_json::to_value(stored).unwrap(), serde_json::to_value(&interrupted).unwrap(),
            "the durable canonical operation is the in-memory Accepted custody");
        assert!(matches!(stored.progress, EngramExistingCheckpointProgress::Accepted { .. }));
        assert_eq!(stored.protection.phase, EngramSessionObligationPhase::Confirmed);
        assert_eq!(settled(&cut), live_evidence, "the receipt effects are durable in the same cut");
        assert!(cut.engram_confirmed_containment_ids.contains(&original.original.id),
            "a reloaded durable Confirmed materializes its latch");
        assert!(engram_session_obligation_locked(&cut, &turn.session_id).is_none(),
            "the reloaded exclusion is released, with its effects already durable");
        assert_eq!(queue_of(&cut, &turn.session_id), vec![successor.clone()], "the durable successor is unchanged");
    }
    // Same process, fresh deadline: accounting resumes from the retained
    // Accepted response with no further wire checkpoint and no new attempt.
    assert!(turn.state.recover_original_checkpoint_result(&original.original.id).unwrap());
    assert_eq!(checkpoints(&turn).len(), 2, "the retained result is never sent again");
    assert_eq!(canonical(&turn, &original).attempts.len(), interrupted.attempts.len(), "no new attempt");
    assert!(turn.state.inner.lock().unwrap().engram_pending_checkpoint_receipts.entries
        .get(&checkpoint_receipt_key(&interrupted)).is_none(), "custody is released after the acknowledgement");
    assert_eq!(admission_requests(&turn), admissions);
    assert_eq!(queue(&turn), vec![successor.clone()], "completion neither consumes nor delivers the successor");
    assert_eq!(turn.record(|record| record.engram.dispatch_generation), generation,
        "only the original's own head could be retired");
    assert_recovered(&turn, &original);
    let completed = durable(&turn);
    assert_eq!(queue_of(&completed, &turn.session_id), vec![successor], "the durable successor survives completion");
    assert_eq!(settled(&completed), live_evidence, "completion applies the receipt effects exactly once");
}

#[test]
fn owner_or_successor_drift_while_a_recovered_reply_returns_retains_custody_without_a_wrong_release() {
    fn edit(state: &AppState, session: &str, change: impl FnOnce(&mut SessionRecord)) {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(session).unwrap();
        change(inner.session_mut_by_index(index).unwrap());
    }
    // Each drift returns the successor it queued, if any.
    type Drift = fn(&AppState, &str) -> Option<Value>;
    let drifts: [(&str, Drift); 3] = [
        ("owner", |state, session| { edit(state, session, |record| record.session.status = SessionStatus::Active); None }),
        // A real ordinary successor is enqueued and committed while the reply returns.
        ("successor", |state, session| Some(queue_successor(state, session, "successor queued while the reply returns"))),
        ("pause", |state, session| { edit(state, session, |record| record.orchestrator_auto_dispatch_blocked = true); None }),
    ];
    for (label, drift) in drifts {
        let (turn, original) = lost_checkpoint(&format!("boundary-drift-{label}"), vec![checkpoint_reply(CHECK_GRANT)]);
        let admissions = admission_requests(&turn);
        let queued = Arc::new(Mutex::new(None));
        let (state, session, seen) = (turn.state.clone(), turn.session_id.clone(), queued.clone());
        install_replay_hooks(&turn, Some(Box::new(move || {
            *seen.lock().unwrap() = drift(&state, &session);
        })), None);
        let grant = turn.record(|record| record.engram.active_grant_id.clone());
        let mut expected_queue = queue(&turn);
        let generation = turn.record(|record| record.engram.dispatch_generation);
        assert!(turn.state.recover_original_checkpoint_result(&original.original.id).is_err(),
            "{label}: a drifted owner cannot finish accounting");
        let successor = queued.lock().unwrap().clone();
        assert_eq!(successor.is_some(), label == "successor", "{label}: only the successor drift queues one");
        expected_queue.extend(successor.clone());
        assert_eq!(turn.record(|record| record.engram.active_grant_id.clone()), grant,
            "{label}: no grant released by a drifted owner");
        assert_eq!(queue(&turn), expected_queue,
            "{label}: no head consumed, and a successor stays exactly as queued");
        assert_eq!(turn.record(|record| record.engram.dispatch_generation), generation,
            "{label}: no head retired by a drifted owner");
        turn.record(|record| match label {
            "owner" => assert_eq!(record.session.status, SessionStatus::Active, "the current owner's state stays"),
            "successor" => assert_eq!(record.queued_prompts.back().map(|head| serde_json::to_value(head).unwrap()),
                successor.clone(), "the successor is the queue's tail, unchanged"),
            _ => assert!(record.orchestrator_auto_dispatch_blocked, "the operator pause stays"),
        });
        assert_eq!(checkpoints(&turn).len(), 2, "{label}: one replay, no replacement send");
        assert_eq!(admission_requests(&turn), admissions, "{label}: no replacement provider delivery");
        assert!(!latched(&turn, &original.original.id), "{label}: no latch for a drifted owner");
        let retained = canonical(&turn, &original);
        assert_eq!(serde_json::to_value(&retained.original).unwrap(), serde_json::to_value(&original.original).unwrap());
        assert!(matches!(retained.progress, EngramExistingCheckpointProgress::Accepted { .. }),
            "{label}: the exact original Accepted result is retained");
        assert_ne!(retained.protection.phase, EngramSessionObligationPhase::Confirmed,
            "{label}: confirmation belongs to a matching owner only");
        turn.state.shutdown_persist_blocking();
        let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
        let durable = loaded.engram_session_obligations.checkpoint_operations.iter()
            .find(|operation| operation.original.id == original.original.id).unwrap();
        assert!(matches!(durable.progress, EngramExistingCheckpointProgress::Accepted { .. }),
            "{label}: the Accepted result is durable");
        assert_eq!(queue_of(&loaded, &turn.session_id), expected_queue, "{label}: the durable queue is unchanged");
    }
}

#[test]
fn an_independent_duty_added_while_a_recovered_reply_returns_is_never_released_or_latched() {
    let (turn, original) = lost_checkpoint("boundary-independent-duty", vec![checkpoint_reply(CHECK_GRANT)]);
    let admissions = admission_requests(&turn);
    let independent = Uuid::new_v4().to_string();
    let (state, duty) = (turn.state.clone(), independent.clone());
    install_replay_hooks(&turn, Some(Box::new(move || {
        // An unrelated duty for the same session appears while the reply
        // returns. Recovery may confirm its own original component only.
        // A valid issued-grant retirement keeps the session's real authority,
        // connection and owner fields, with its own kind, basis, owner head
        // and frozen SessionBind, as the existing ledger fixtures build one.
        let mut inner = state.inner.lock().unwrap();
        let operation = inner.engram_session_obligations.checkpoint_operations[0].clone();
        let mut entry = operation.protection;
        entry.id = duty;
        entry.kind = EngramSessionObligationKind::IssuedGrantRetirement;
        entry.basis = EngramSessionObligationBasis::ProvenNeverSentBegin;
        entry.prompt_id = Some("independent-owner-head".to_owned());
        entry.grant_id = "independent-issued-grant".to_owned();
        entry.phase = EngramSessionObligationPhase::Intended;
        entry.settlement_evidence = Value::Null;
        entry.frozen_request = serde_json::to_value(EngramControlRequest::SessionBind {
            external_ref: entry.external_ref.clone(),
            title: "Independent retirement".to_owned(),
            assurance: ENGRAM_CONTROL_ASSURANCE.to_owned(),
            mediated_effects: Vec::new(),
            capability_map_revision: 1,
            work_binding: None,
            idempotency_key: "independent-retirement-key".to_owned(),
        }).unwrap();
        let ledger = inner.engram_session_obligations.clone().with_entry(entry)
            .expect("the independent duty is a valid ledger entry");
        ledger.validate().expect("the whole ledger validates with the independent duty");
        inner.engram_session_obligations = ledger;
    })), None);
    let _ = turn.state.recover_original_checkpoint_result(&original.original.id);
    let inner = turn.state.inner.lock().unwrap();
    let entry = inner.engram_session_obligations.entries.iter().find(|entry| entry.id == independent)
        .expect("the independent duty is never released by the original's recovery");
    assert_eq!(entry.phase, EngramSessionObligationPhase::Intended, "the independent duty keeps its own phase");
    assert!(!inner.engram_confirmed_containment_ids.contains(&independent), "the independent duty is never latched");
    assert!(engram_session_obligation_locked(&inner, &turn.session_id).is_some(),
        "the independent exclusion still holds the session");
    let retained = inner.engram_session_obligations.checkpoint_operations.iter()
        .find(|operation| operation.original.id == original.original.id).unwrap();
    assert!(matches!(retained.progress, EngramExistingCheckpointProgress::Accepted { .. }),
        "the original Accepted result stays in canonical custody");
    drop(inner);
    assert_eq!(checkpoints(&turn).len(), 2, "one replay, no replacement send");
    assert_eq!(admission_requests(&turn), admissions, "no replacement provider delivery");
    // A whole-state reload keeps both: the original Accepted custody and the
    // independent duty with its own phase, unlatched, still excluding the session.
    turn.state.shutdown_persist_blocking();
    let loaded = durable(&turn);
    let stored = durable_operation(&loaded, &original);
    assert!(matches!(stored.progress, EngramExistingCheckpointProgress::Accepted { .. }),
        "the original Accepted custody is durable");
    assert_eq!(serde_json::to_value(&stored.original).unwrap(), serde_json::to_value(&original.original).unwrap());
    let entry = loaded.engram_session_obligations.entries.iter().find(|entry| entry.id == independent)
        .expect("the independent duty survives a reload");
    assert_eq!(entry.phase, EngramSessionObligationPhase::Intended, "the reloaded duty keeps its own phase");
    assert_eq!(entry.kind, EngramSessionObligationKind::IssuedGrantRetirement);
    assert!(!loaded.engram_confirmed_containment_ids.contains(&independent), "the reloaded duty is not latched");
    assert!(engram_session_obligation_locked(&loaded, &turn.session_id).is_some(),
        "the reloaded exclusion still holds the session");
    loaded.engram_session_obligations.validate().expect("the reloaded ledger validates");
}

#[test]
fn an_adopted_same_id_enclosing_duty_keeps_its_exclusion_through_the_original_acknowledgement_and_reload() {
    let (turn, original) = lost_checkpoint("boundary-adopted-enclosing", vec![checkpoint_reply(CHECK_GRANT)]);
    // Adopt the unresolved original exactly as the containment plan does. The
    // enclosing CheckpointContainment deliberately reuses the original's id, so
    // all_protections selects it instead of the canonical operation protection.
    let enclosing = {
        let mut inner = turn.state.inner.lock().unwrap();
        let prepared = prepare_engram_containment_entry_locked(&inner, &turn.session_id,
            &original.protection.project_id, &original.protection.authority, true)
            .expect("the containment plan accepts the session")
            .expect("the unresolved original checkpoint is adopted");
        let entry = prepared.obligation;
        assert_eq!(entry.id, original.original.id, "adoption reuses the original checkpoint id");
        assert_eq!(entry.kind, EngramSessionObligationKind::CheckpointContainment);
        assert_ne!(entry.phase, EngramSessionObligationPhase::Confirmed, "the enclosing duty is unsettled");
        let ledger = inner.engram_session_obligations.clone().with_entry(entry.clone())
            .expect("the adopted duty is a valid ledger entry");
        ledger.validate().expect("the whole ledger validates with the adopted duty");
        inner.engram_session_obligations = ledger;
        // Publication moves the session to the adopted owner's generation.
        let index = inner.find_session_index(&turn.session_id).unwrap();
        inner.session_mut_by_index(index).unwrap().engram.dispatch_generation = entry.owner_generation;
        turn.state.commit_locked(&mut inner).unwrap();
        assert!(engram_session_obligation_locked(&inner, &turn.session_id)
            .is_some_and(|held| held.id == entry.id && held.kind == EngramSessionObligationKind::CheckpointContainment),
            "the enclosing duty is the selected protection before recovery");
        entry
    };
    let admissions = admission_requests(&turn);
    // Recovery may account the original receipt. Whatever it reports, the
    // enclosing duty's own custody and exclusion must survive it.
    let _ = turn.state.recover_original_checkpoint_result(&original.original.id);
    let check = |inner: &StateInner, when: &str| {
        let held = engram_session_obligation_locked(inner, &turn.session_id)
            .unwrap_or_else(|| panic!("{when}: the unsettled enclosing duty must still exclude the session"));
        assert_eq!(held.id, enclosing.id, "{when}: the enclosing duty is still the selected protection");
        let entry = inner.engram_session_obligations.entries.iter().find(|entry| entry.id == enclosing.id)
            .unwrap_or_else(|| panic!("{when}: the enclosing duty is never released by the original's recovery"));
        assert_eq!(serde_json::to_value(entry).unwrap(), serde_json::to_value(&enclosing).unwrap(),
            "{when}: the enclosing duty keeps its exact frozen custody and phase");
        let canonical = inner.engram_session_obligations.checkpoint_operations.iter()
            .find(|operation| operation.original.id == original.original.id)
            .unwrap_or_else(|| panic!("{when}: the canonical original operation is retained"));
        assert_eq!(serde_json::to_value(&canonical.original).unwrap(), serde_json::to_value(&original.original).unwrap());
        assert!(matches!(canonical.progress, EngramExistingCheckpointProgress::Accepted { .. }),
            "{when}: the original's Accepted result is in canonical custody");
    };
    check(&*turn.state.inner.lock().unwrap(), "live");
    assert_eq!(checkpoints(&turn).len(), 2, "one replay, no replacement send");
    assert_eq!(admission_requests(&turn), admissions, "no bind, evaluate or begin for the enclosing duty");
    turn.state.shutdown_persist_blocking();
    check(&durable(&turn), "reloaded");
}
