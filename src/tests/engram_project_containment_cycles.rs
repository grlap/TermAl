// Owns repeated public containment cycles and historical-operation conflicts.
// Does not settle independent grants or authorize live control changes.
use super::*;

struct LostNoGrantDecisions {
    inner: Arc<StatefulEngramControlTransport>,
    evaluations: Mutex<Vec<Value>>,
}

impl EngramControlTransport for LostNoGrantDecisions {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        if matches!(request, EngramControlRequest::TurnEvaluate { .. }) {
            let mut requests = self.evaluations.lock().unwrap();
            requests.push(serde_json::to_value(request).unwrap());
            if requests.len() % 2 == 1 {
                return Err(EngramTransportError::deadline(
                    "fixture lost a no-grant decision",
                ));
            }
            return ScriptedEngramControlTransport::new([evaluation_refusal_reply(
                "operator_review_required",
            )])
            .request(connection, request, timeout);
        }
        assert!(!matches!(request, EngramControlRequest::TurnBegin { .. }));
        self.inner.request(connection, request, timeout)
    }
    fn shutdown_session(&self, session_id: &str) {
        self.inner.shutdown_session(session_id);
    }
}

#[test]
fn project_containment_two_public_off_cycles_keep_distinct_history_through_reload() {
    let (state, project, session, _root, receiver) = fixture();
    {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.engram = EngramSessionState::default();
        record.engram.context_nudge_pending = false;
        record.session.status = SessionStatus::Idle;
        state.commit_locked(&mut inner).unwrap();
    }
    let transport = Arc::new(LostNoGrantDecisions {
        inner: StatefulEngramControlTransport::new(),
        evaluations: Mutex::new(Vec::new()),
    });
    state.install_control_test_transport(transport.clone());
    for cycle in 0..2 {
        match state.dispatch_turn(&session, prompt()).unwrap() {
            DispatchTurnResult::Dispatched(dispatch)
            | DispatchTurnResult::DispatchedAfterQueue(dispatch) => {
                deliver_turn_dispatch(&state, dispatch).unwrap();
            }
            DispatchTurnResult::Queued => {
                let _ = state.resume_session_queue(&session);
            }
        }
        assert_eq!(transport.evaluations.lock().unwrap().len(), cycle * 2 + 1);
        state
            .update_project_engram_settings(&project, settings(&state, &project, false))
            .expect("a previous completed cycle cannot veto a new OFF");
        state
            .update_project_engram_settings(&project, settings(&state, &project, true))
            .unwrap();
        let _ = state.resume_session_queue(&session);
        assert!(receiver.try_recv().is_err());
        {
            let requests = transport.evaluations.lock().unwrap();
            assert_eq!(requests.len(), (cycle + 1) * 2);
            assert_eq!(requests[cycle * 2], requests[cycle * 2 + 1]);
            if cycle == 1 {
                assert_ne!(
                    requests[0]["idempotency_key"],
                    requests[2]["idempotency_key"]
                );
            }
        }
        state.shutdown_persist_blocking();
        let loaded = load_state(state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        assert_eq!(loaded.engram_session_obligations.entries.len(), cycle + 1);
        assert!(loaded.engram_session_obligations.validate().is_ok());
        assert!(engram_session_obligation_locked(&loaded, &session).is_none());
        assert!(
            loaded
                .engram_session_obligations
                .entries
                .iter()
                .all(containment_admission_decision_matches)
        );
        // Restore only the fixture's live runtime capability; all durable
        // state for the next public cycle comes from the real loader.
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let runtime = std::mem::replace(&mut inner.sessions[index].runtime, SessionRuntime::None);
        *inner = loaded;
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].runtime = runtime;
        drop(inner);
        restore_containment_test_adapter_after_load(&state, transport.clone());
    }
    let inner = state.inner.lock().unwrap();
    let first = inner.engram_session_obligations.entries[0].clone();
    let mut duplicate = first.clone();
    duplicate.id = Uuid::new_v4().to_string();
    duplicate.owner_generation += 1;
    assert!(
        inner
            .engram_session_obligations
            .with_entry(duplicate)
            .is_err(),
        "a new UUID/owner cannot disguise the same original wire operation"
    );
}

#[test]
fn project_containment_new_operation_cannot_compete_with_an_unresolved_producer_duty() {
    let (state, _project, _session, _root, _receiver, _transport) =
        retained_admission(&["turn_evaluate"]);
    let inner = state.inner.lock().unwrap();
    let first = inner.engram_session_obligations.entries[0].clone();
    let mut competing = first.clone();
    competing.id = Uuid::new_v4().to_string();
    competing.owner_generation += 1;
    let mut image: EngramUnsettledControlImage =
        serde_json::from_value(competing.frozen_request.clone()).unwrap();
    image.dispatch_generation += 1;
    image.queued_prompts[0]
        .pending_prompt
        .id
        .push_str("-new-cycle");
    if let EngramControlRequest::TurnEvaluate {
        idempotency_key, ..
    } = &mut image.queued_prompts[0]
        .engram_evaluate
        .as_mut()
        .unwrap()
        .request
    {
        idempotency_key.push_str("-new-cycle");
    }
    competing.prompt_id = Some(image.queued_prompts[0].pending_prompt.id.clone());
    competing.frozen_request = serde_json::to_value(image).unwrap();
    assert!(competing.validate().is_ok());
    assert!(
        inner
            .engram_session_obligations
            .with_entry(competing)
            .is_err()
    );
}

#[test]
fn project_containment_failed_transfer_commit_keeps_issued_grant_and_local_ack_recovery() {
    let (state, project, session, root, receiver, transport) =
        retained_admission(&["turn_evaluate"]);
    state
        .update_project_engram_settings(&project, settings(&state, &project, true))
        .unwrap();
    let scope = state
        .begin_contained_admission_resume(&session)
        .unwrap()
        .unwrap();
    let (original_owner, original_active_generation, replay_generation) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        (
            EngramQueuedAdmissionOwner::capture(record).unwrap(),
            record.active_turn_generation,
            record.queued_prompts[0]
                .engram_evaluate
                .as_ref()
                .unwrap()
                .operation_generation
                .unwrap(),
        )
    };
    // Public Resume promotes the exact retained operation before Begin
    // preparation. Keep that real dispatch undelivered at this fault boundary.
    let _dispatch = state
        .dispatch_next_queued_turn_for_resume(&session, original_owner.clone())
        .unwrap()
        .expect("the retained admission must promote before preparing Begin");
    let grant = transport.inner.grant_state(&session).0.unwrap();
    let (owner, begin) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        assert_ne!(original_owner.generation, replay_generation);
        assert_eq!(record.engram.dispatch_generation, replay_generation);
        assert_eq!(
            record.active_turn_generation,
            original_active_generation + 1
        );
        let evaluate = record.queued_prompts[0].engram_evaluate.as_ref().unwrap();
        let EngramControlRequest::TurnEvaluate { routing_token, .. } = &evaluate.request else {
            unreachable!();
        };
        (
            EngramQueuedAdmissionOwner::capture(record).unwrap(),
            EngramControlRequest::TurnBegin {
                routing_token: routing_token.clone(),
                grant_id: grant.clone(),
                delivery_tokens: Vec::new(),
                idempotency_key: "retained-issued-grant-begin".into(),
            },
        )
    };
    let before = transport.inner.requests().len();
    let failing = AppState {
        persistence_path: Arc::new(root),
        ..state.clone()
    };
    let failure = failing
        .prepare_engram_begin_durable(
            &session,
            state.engram_budget_clock().now() + Duration::from_secs(5),
            &owner,
            &begin,
            None,
            EngramBeginPreparationKind::First,
        )
        .err()
        .expect("the directory path refuses the transfer commit");
    assert!(failure.pending && !failure.possibly_sent);
    drop(scope);
    {
        let inner = state.inner.lock().unwrap();
        let entry = engram_session_obligation_locked(&inner, &session).unwrap();
        assert_eq!(entry.phase, EngramSessionObligationPhase::Confirmed);
        assert!(containment_admission_transfer_matches(entry));
        assert!(inner.engram_confirmed_containment_ids.is_empty());
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        let prepared = record.queued_prompts[0]
            .engram_evaluate
            .as_ref()
            .unwrap()
            .prepared_begin
            .as_ref()
            .unwrap();
        assert_eq!(prepared.grant_id, grant);
        assert_eq!(prepared.idempotency_key, "retained-issued-grant-begin");
    }
    assert_eq!(
        transport.inner.grant_state(&session).0.as_deref(),
        Some(grant.as_str())
    );
    state
        .retry_containment_admission_transfer(&session)
        .unwrap();
    assert!(
        state
            .prepare_engram_begin_durable(
                &session,
                state.engram_budget_clock().now() + Duration::from_secs(5),
                &owner,
                &begin,
                None,
                EngramBeginPreparationKind::Pending,
            )
            .is_ok(),
        "the supported next step re-ACKs the exact issued-grant successor"
    );
    state.shutdown_persist_blocking();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(engram_session_obligation_locked(&loaded, &session).is_none());
    assert_eq!(
        transport.inner.requests().len(),
        before,
        "local recovery sends no authority RPC"
    );
    assert!(receiver.try_recv().is_err());
}
