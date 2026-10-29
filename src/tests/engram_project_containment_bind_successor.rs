// Owns accepted-Bind interruption/reload, successor ACK loss and owner races.
// Does not exercise live stores, settle grants or authorize a new queue head.
use super::*;

struct ReplaceOwnerAfterBindReply {
    inner: Arc<LostContainmentAdmissionReply>,
    state: std::sync::Weak<StateMutex<StateInner>>,
}

impl EngramControlTransport for ReplaceOwnerAfterBindReply {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        let response = self.inner.request(connection, request, timeout)?;
        if matches!(request, EngramControlRequest::SessionBind { .. }) {
            let state = self.state.upgrade().unwrap();
            let mut inner = state.lock().unwrap();
            let index = inner.find_session_index(&connection.session_id).unwrap();
            inner.sessions[index].engram.dispatch_generation += 1;
        }
        Ok(response)
    }
    fn shutdown_session(&self, session_id: &str) {
        self.inner.shutdown_session(session_id);
    }
}

#[test]
fn project_containment_accepted_bind_reply_rechecks_owner_before_retiring_original() {
    let (state, project, session, _root, receiver, transport) =
        retained_admission(&["session_bind"]);
    state
        .update_project_engram_settings(&project, settings(&state, &project, true))
        .unwrap();
    let before = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        (
            serde_json::to_value(&record.queued_prompts[0].engram_bind).unwrap(),
            record.engram.routing_token.clone(),
        )
    };
    state.install_control_test_transport(Arc::new(ReplaceOwnerAfterBindReply {
        inner: transport.clone(),
        state: Arc::downgrade(&state.inner),
    }));
    let _ = state.resume_session_queue(&session);
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert_eq!(
        serde_json::to_value(&record.queued_prompts[0].engram_bind).unwrap(),
        before.0
    );
    assert_eq!(record.engram.routing_token, before.1);
    assert!(record.queued_prompts[0].engram_evaluate.is_none());
    assert!(engram_session_obligation_locked(&inner, &session).is_some());
    assert!(inner.engram_confirmed_containment_ids.is_empty());
    assert!(receiver.try_recv().is_err());
    let requests = transport.inner.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].request, requests[1].request);
}

struct Bound {
    state: AppState,
    session: String,
    receiver: mpsc::Receiver<CodexRuntimeCommand>,
    transport: Arc<LostContainmentAdmissionReply>,
    target: EngramBindingTarget,
    owner: EngramQueuedAdmissionOwner,
    intent: EngramTurnIntentSnapshot,
    request: EngramControlRequest,
    scope: Option<EngramContainmentAdmissionScope>,
}

fn original_bind_replayed() -> Bound {
    let (state, project, session, _root, receiver, transport) =
        retained_admission(&["session_bind"]);
    state
        .update_project_engram_settings(&project, settings(&state, &project, true))
        .unwrap();
    let scope = state
        .begin_contained_admission_resume(&session)
        .unwrap()
        .unwrap();
    let (owner, fingerprint) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        (
            EngramQueuedAdmissionOwner::capture(record).unwrap(),
            containment_prompt_fingerprint(&record.queued_prompts[0]),
        )
    };
    let generation = state.queued_engram_generation(&session, &owner.prompt_id, owner.generation);
    let target = state
        .ensure_engram_session_bound_with_budget_off_lock(
            &session,
            Some(state.engram_budget_clock().now()),
            Some(&owner),
        )
        .unwrap()
        .unwrap();
    let request = EngramControlRequest::TurnEvaluate {
        routing_token: target.routing_token.clone().unwrap(),
        idempotency_key: format!("termal-evaluate:{session}:{generation}:{fingerprint}"),
        intent_fingerprint: fingerprint.clone(),
        purpose: "ordinary".into(),
        requested_effects: target.effects.clone(),
        resource_intents: Vec::new(),
    };
    let intent = EngramTurnIntentSnapshot {
        session_id: session.clone(),
        dispatch_generation: generation,
        intent_fingerprint: fingerprint,
    };
    Bound {
        state,
        session,
        receiver,
        transport,
        target,
        owner,
        intent,
        request,
        scope: Some(scope),
    }
}

#[test]
fn project_containment_accepted_bind_gap_keeps_same_replay_owner_through_reload() {
    for reload in [false, true] {
        // Stop at the real accepted-reply boundary, before any Evaluate is
        // prepared. A budget exit at the next check has this same image.
        let mut fixture = original_bind_replayed();
        let original = fixture.transport.inner.requests()[0].clone();
        drop(fixture.scope.take());
        if reload {
            fixture.state.shutdown_persist_blocking();
            let loaded = load_state(fixture.state.persistence_path.as_path())
                .unwrap()
                .unwrap();
            *fixture.state.inner.lock().unwrap() = loaded;
            restore_containment_test_adapter_after_load(&fixture.state, fixture.transport.clone());
        }
        let (owner, generation, active_generation) = {
            let inner = fixture.state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
            let bind = record.queued_prompts[0]
                .engram_bind
                .as_ref()
                .expect("the original Bind survives acceptance until linked succession");
            assert_eq!(
                serde_json::to_value(&bind.request).unwrap(),
                original.request
            );
            assert!(record.queued_prompts[0].engram_evaluate.is_none());
            assert!(engram_session_obligation_locked(&inner, &fixture.session).is_some());
            assert!(inner.engram_confirmed_containment_ids.is_empty());
            (
                EngramQueuedAdmissionOwner::capture(record).unwrap(),
                record.engram.dispatch_generation,
                record.active_turn_generation,
            )
        };
        let scope = fixture
            .state
            .begin_contained_admission_resume(&fixture.session)
            .unwrap()
            .expect("a stopped pre-link owner remains recoverable");
        let target = fixture
            .state
            .ensure_engram_session_bound_with_budget_off_lock(
                &fixture.session,
                Some(fixture.state.engram_budget_clock().now()),
                Some(&owner),
            )
            .unwrap()
            .unwrap();
        let requests = fixture.transport.inner.requests();
        assert_eq!(requests.len(), 3);
        assert!(
            requests
                .iter()
                .all(|request| request.request == original.request
                    && request.connection == original.connection)
        );
        let request = fixture
            .state
            .queued_engram_evaluate_request(
                &target,
                &fixture.intent,
                &owner,
                fixture.request.clone(),
            )
            .unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::to_value(&fixture.request).unwrap()
        );
        {
            let inner = fixture.state.inner.lock().unwrap();
            let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
            assert_eq!(record.engram.dispatch_generation, generation);
            assert_eq!(record.active_turn_generation, active_generation);
            assert!(record.queued_prompts[0].engram_bind.is_none());
            let entry = engram_session_obligation_locked(&inner, &fixture.session).unwrap();
            let successor = containment_bind_successor(entry).expect("typed exact successor");
            assert_eq!(
                serde_json::to_value(&record.queued_prompts[0].engram_evaluate).unwrap(),
                serde_json::to_value(Some(successor)).unwrap()
            );
            assert_eq!(entry.phase, EngramSessionObligationPhase::Intended);
            let image = EngramProjectContainmentFence::capture(
                &inner,
                &entry.project_id,
                &[fixture.session.clone()],
            )
            .unwrap();
            let database =
                rusqlite::Connection::open(fixture.state.persistence_path.as_path()).unwrap();
            assert!(image.is_already_durable(&database).unwrap());
        }
        drop(scope);
        assert_eq!(
            fixture.transport.inner.requests().len(),
            3,
            "preparation sends no Evaluate"
        );
        assert!(fixture.receiver.try_recv().is_err());
    }
}

#[test]
fn project_containment_bind_successor_lost_ack_keeps_original_and_specific_successor() {
    for changed in [false, true] {
        let mut fixture = original_bind_replayed();
        let sent = fixture.transport.inner.requests().len();
        let original = fixture
            .state
            .inner
            .lock()
            .unwrap()
            .engram_session_obligations
            .entries[0]
            .frozen_request
            .clone();
        TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
        let result = fixture.state.queued_engram_evaluate_request(
            &fixture.target,
            &fixture.intent,
            &fixture.owner,
            fixture.request.clone(),
        );
        TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(true));
        assert!(result.is_err(), "a missing ACK withholds Evaluate");
        drop(fixture.scope.take());
        {
            let mut inner = fixture.state.inner.lock().unwrap();
            let entry = &inner.engram_session_obligations.entries[0];
            assert_eq!(entry.frozen_request, original);
            let successor = containment_bind_successor(entry).expect("specific successor retained");
            assert_eq!(
                serde_json::to_value(&successor.request).unwrap(),
                serde_json::to_value(&fixture.request).unwrap()
            );
            assert_eq!(entry.phase, EngramSessionObligationPhase::Intended);
            assert!(entry.validate().is_ok());
            let mut malformed = entry.clone();
            malformed.settlement_evidence["promptId"] = json!("another-head");
            assert!(
                malformed.validate().is_err(),
                "a partial link names its exact head"
            );
            let mut premature = entry.clone();
            premature.phase = EngramSessionObligationPhase::Confirmed;
            assert!(
                premature.validate().is_err(),
                "partial succession is not settlement"
            );
            assert!(inner.engram_confirmed_containment_ids.is_empty());
            if changed {
                let index = inner.find_session_index(&fixture.session).unwrap();
                let evaluate = inner.session_mut_by_index(index).unwrap().queued_prompts[0]
                    .engram_evaluate
                    .as_mut()
                    .unwrap();
                if let EngramControlRequest::TurnEvaluate {
                    idempotency_key, ..
                } = &mut evaluate.request
                {
                    idempotency_key.push_str("-changed");
                }
                fixture.state.commit_locked(&mut inner).unwrap();
            }
        }
        if changed {
            assert!(
                fixture
                    .state
                    .begin_contained_admission_resume(&fixture.session)
                    .is_err()
            );
        } else {
            let scope = fixture
                .state
                .begin_contained_admission_resume(&fixture.session)
                .unwrap()
                .unwrap();
            let replay = fixture
                .state
                .queued_engram_evaluate_request(
                    &fixture.target,
                    &fixture.intent,
                    &fixture.owner,
                    fixture.request.clone(),
                )
                .unwrap();
            assert_eq!(
                serde_json::to_value(replay).unwrap(),
                serde_json::to_value(&fixture.request).unwrap()
            );
            drop(scope);
        }
        fixture.state.shutdown_persist_blocking();
        let loaded = load_state(fixture.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        let record = &loaded.sessions[loaded.find_session_index(&fixture.session).unwrap()];
        assert!(record.engram.admission_retry.is_none() && record.engram.bind_retry.is_none());
        assert!(engram_session_obligation_locked(&loaded, &fixture.session).is_some());
        assert_eq!(
            loaded.engram_session_obligations.entries[0].frozen_request,
            original
        );
        assert!(loaded.engram_confirmed_containment_ids.is_empty());
        assert_eq!(
            fixture.transport.inner.requests().len(),
            sent,
            "ACK/recovery prepares but transmits nothing"
        );
        assert!(fixture.receiver.try_recv().is_err());
    }
}

#[test]
fn project_containment_bind_successor_ack_cannot_authorize_a_replaced_owner() {
    let mut fixture = original_bind_replayed();
    let original_tx = fixture.state.persist_tx.clone();
    let (tx, rx) = mpsc::channel();
    fixture.state.persist_tx = tx;
    let sent = fixture.transport.inner.requests().len();
    std::thread::scope(|threads| {
        let state = &fixture.state;
        let session = &fixture.session;
        let writer = threads.spawn(move || {
            let fence = loop {
                if let PersistRequest::Fence(fence) =
                    phase_sync::receive(&rx, "Bind successor writer fence")
                {
                    assert!(matches!(
                        &fence.target,
                        PersistFenceTarget::EngramProjectContainment(_)
                    ));
                    break fence;
                }
            };
            let mut inner = state.inner.lock().unwrap();
            // Materialize the captured image, then change the live owner
            // before reporting its ACK. Disk success cannot bless the drift.
            state.persist_internal_locked(&inner).unwrap();
            let index = inner.find_session_index(session).unwrap();
            inner
                .session_mut_by_index(index)
                .unwrap()
                .engram
                .dispatch_generation += 1;
            drop(inner);
            fence.finish(Ok(()));
        });
        assert!(
            fixture
                .state
                .queued_engram_evaluate_request(
                    &fixture.target,
                    &fixture.intent,
                    &fixture.owner,
                    fixture.request.clone()
                )
                .is_err()
        );
        writer.join().unwrap();
    });
    fixture.state.persist_tx = original_tx;
    drop(fixture.scope.take());
    assert!(
        fixture
            .state
            .begin_contained_admission_resume(&fixture.session)
            .is_err()
    );
    let inner = fixture.state.inner.lock().unwrap();
    assert!(engram_session_obligation_locked(&inner, &fixture.session).is_some());
    assert!(inner.engram_confirmed_containment_ids.is_empty());
    assert_eq!(fixture.transport.inner.requests().len(), sent);
    assert!(fixture.receiver.try_recv().is_err());
}
