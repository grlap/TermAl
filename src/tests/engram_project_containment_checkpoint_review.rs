// Reproduces exact-owner replay and the hold-to-promotion race at existing
// checkpoint boundaries. No live store, generic exclusion bypass or scheduler.
use super::*;

#[path = "engram_project_containment_checkpoint_watermark.rs"]
mod watermark;

fn quarantined_checkpoint_survives_other_session_compaction(remove_other: bool) {
    for invalid_agent in [false, true] {
        let (state, project, session, root, receiver) = fixture();
        let transport = ScriptedEngramControlTransport::new([
            checkpoint_reply("retained-grant"),
            checkpoint_reply("other-grant"),
        ]);
        state.install_control_test_transport(transport.clone());
        assert!(matches!(
            close(&state, &session),
            EngramCheckpointOutcome::Succeeded
        ));
        let original = state
            .inner
            .lock()
            .unwrap()
            .engram_session_obligations
            .checkpoint_operations[0]
            .clone();
        let other = create_test_project_session(&state, Agent::Codex, &project, &root);
        {
            let mut inner = state.inner.lock().unwrap();
            let record = inner.session_mut(&other).unwrap();
            record.session.status = SessionStatus::Error;
            record.engram.routing_token = Some("other-token".into());
            record.engram.active_grant_id = Some("other-grant".into());
            record.engram.begins_recorded = true;
            state.commit_locked(&mut inner).unwrap();
        }
        state.shutdown_persist_blocking();
        let row = {
            let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
            let sql = if invalid_agent {
                "UPDATE sessions SET value_json=json_set(value_json, '$.session.agent', 'unsupported_fixture_agent') WHERE id=?1"
            } else {
                "UPDATE sessions SET value_json=json_set(value_json, '$.session.projectId', 'missing-fixture-project') WHERE id=?1"
            };
            assert_eq!(connection.execute(sql, [&session]).unwrap(), 1);
            connection
                .query_row(
                    "SELECT value_json FROM sessions WHERE id=?1",
                    [&session],
                    |row| row.get::<_, String>(0),
                )
                .unwrap()
        };
        let loaded = load_state(state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        assert!(loaded.find_session_index(&session).is_none());
        assert!(loaded.quarantined_persisted_session_ids.contains(&session));
        assert!(
            loaded
                .engram_confirmed_containment_ids
                .contains(&original.original.id)
        );
        *state.inner.lock().unwrap() = loaded;
        // The raw loader resets runtime-only dependencies. Restore the
        // fixture's clock before any post-load target snapshots it.
        select_scripted_engram_budget_clock_before_enable(&state);
        super::super::admission::restore_containment_test_adapter_after_load(
            &state,
            transport.clone(),
        );
        if remove_other {
            state.kill_session(&other).unwrap();
        } else {
            let outcome = close(&state, &other);
            assert!(
                matches!(outcome, EngramCheckpointOutcome::Succeeded),
                "other checkpoint outcome: {outcome:?}; invalid_agent={invalid_agent}"
            );
        }
        state.shutdown_persist_blocking();
        {
            let connection = rusqlite::Connection::open(state.persistence_path.as_path()).unwrap();
            let after: String = connection
                .query_row(
                    "SELECT value_json FROM sessions WHERE id=?1",
                    [&session],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                after, row,
                "compaction must leave the quarantined row untouched"
            );
        }
        let reloaded = load_state(state.persistence_path.as_path())
            .expect("unrelated compaction must not turn a quarantined row into whole-store refusal")
            .unwrap();
        assert!(
            reloaded
                .quarantined_persisted_session_ids
                .contains(&session)
        );
        let retained = reloaded
            .engram_session_obligations
            .checkpoint_operations
            .iter()
            .find(|operation| operation.original.id == original.original.id)
            .expect("the persisted mirror keeps its exact canonical ledger protection");
        assert_eq!(
            serde_json::to_value(retained).unwrap(),
            serde_json::to_value(&original).unwrap()
        );
        assert_eq!(transport.requests().len(), 2);
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn project_containment_checkpoint_review_quarantined_history_survives_other_checkpoint() {
    quarantined_checkpoint_survives_other_session_compaction(false);
}

#[test]
fn project_containment_checkpoint_review_quarantined_history_survives_other_deletion() {
    quarantined_checkpoint_survives_other_session_compaction(true);
}

#[test]
fn project_containment_checkpoint_review_deleted_session_history_is_bounded_through_reload() {
    const SESSIONS: usize = 24;
    let (state, project, first_session, root, receiver) = fixture();
    let grants: Vec<String> = (0..SESSIONS)
        .map(|index| {
            if index == 0 {
                "retained-grant".into()
            } else {
                format!("deleted-grant-{index}")
            }
        })
        .collect();
    let transport =
        ScriptedEngramControlTransport::new(grants.iter().map(|grant| checkpoint_reply(grant)));
    state.install_control_test_transport(transport.clone());
    for index in 0..SESSIONS {
        let session = if index == 0 {
            first_session.clone()
        } else {
            let session = create_test_project_session(&state, Agent::Codex, &project, &root);
            let mut inner = state.inner.lock().unwrap();
            let record = inner.session_mut(&session).unwrap();
            record.session.status = SessionStatus::Error;
            record.runtime = SessionRuntime::None;
            record.engram.routing_token = Some(format!("deleted-token-{index}"));
            record.engram.active_grant_id = Some(grants[index].clone());
            record.engram.begins_recorded = true;
            state.commit_locked(&mut inner).unwrap();
            session
        };
        state.kill_session(&session).unwrap();
        assert!(
            state
                .inner
                .lock()
                .unwrap()
                .find_session_index(&session)
                .is_none()
        );
    }
    let requests = transport.requests();
    assert_eq!(requests.len(), SESSIONS);
    for (request, grant) in requests.iter().zip(&grants) {
        assert_eq!(request.request["operation"], "turn_checkpoint");
        assert_eq!(request.request["next_intent"], "exit");
        assert_eq!(request.request["grant_id"], *grant);
    }
    {
        let inner = state.inner.lock().unwrap();
        assert!(inner.engram_pending_checkpoint_receipts.entries.is_empty());
        assert!(
            inner
                .engram_session_obligations
                .checkpoint_operations
                .is_empty(),
            "deleted sessions cannot retain full ACKed checkpoint history: {}",
            inner.engram_session_obligations.checkpoint_operations.len()
        );
        assert!(inner.engram_confirmed_containment_ids.is_empty());
    }
    state.shutdown_persist_blocking();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(
        loaded
            .engram_session_obligations
            .checkpoint_operations
            .is_empty()
    );
    assert!(loaded.engram_confirmed_containment_ids.is_empty());
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_checkpoint_review_deletion_preserves_active_and_unlatched_receipts() {
    for active in [true, false] {
        let (state, _project, session, _root, receiver) = fixture();
        let transport = ScriptedEngramControlTransport::new([checkpoint_reply("retained-grant")]);
        state.install_control_test_transport(transport.clone());
        assert!(matches!(
            close(&state, &session),
            EngramCheckpointOutcome::Succeeded
        ));
        let original = state
            .inner
            .lock()
            .unwrap()
            .engram_session_obligations
            .checkpoint_operations[0]
            .clone();
        let reservation = if active {
            Some(state.reserve_checkpoint_receipt(&original).unwrap())
        } else {
            state
                .inner
                .lock()
                .unwrap()
                .engram_confirmed_containment_ids
                .remove(&original.original.id);
            None
        };
        state.kill_session(&session).unwrap();
        {
            let inner = state.inner.lock().unwrap();
            assert!(inner.find_session_index(&session).is_none());
            assert_eq!(
                serde_json::to_value(&inner.engram_session_obligations.checkpoint_operations)
                    .unwrap(),
                json!([original])
            );
        }
        state.shutdown_persist_blocking();
        let loaded = load_state(state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(&loaded.engram_session_obligations.checkpoint_operations).unwrap(),
            json!([original])
        );
        drop(reservation);
        assert_eq!(transport.requests().len(), 1);
        assert!(receiver.try_recv().is_err());
    }
}

fn close(state: &AppState, session: &str) -> EngramCheckpointOutcome {
    state.checkpoint_engram_turn_off_lock(
        session,
        EngramCheckpointPurpose::Settlement,
        None,
        None,
        EngramNextIntent::Wait,
        None,
        None,
    )
}

#[test]
fn project_containment_checkpoint_review_deleted_session_keeps_linked_containment_history() {
    let (state, _project, session, receiver, transport, _request, _retained) =
        checkpoint_overlapped_by_off(false);
    let before = {
        let inner = state.inner.lock().unwrap();
        assert_eq!(inner.engram_session_obligations.entries.len(), 1);
        serde_json::to_value(&inner.engram_session_obligations).unwrap()
    };
    let requests = transport.requests().len();
    state.kill_session(&session).unwrap();
    {
        let inner = state.inner.lock().unwrap();
        assert!(inner.find_session_index(&session).is_none());
        assert_eq!(
            serde_json::to_value(&inner.engram_session_obligations).unwrap(),
            before
        );
    }
    state.shutdown_persist_blocking();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&loaded.engram_session_obligations).unwrap(),
        before
    );
    assert_eq!(transport.requests().len(), requests);
    assert!(receiver.try_recv().is_err());
}

fn successor_grant(state: &AppState, session: &str, grant: &str) {
    let mut inner = state.inner.lock().unwrap();
    let record = inner.session_mut(session).unwrap();
    record.engram.active_grant_id = Some(grant.into());
    record.engram.dispatch_generation += 1;
    record.active_turn_generation += 1;
    state.commit_locked(&mut inner).unwrap();
}

#[test]
fn project_containment_checkpoint_review_active_receipt_survives_compaction_and_old_callback_is_inert()
 {
    let (state, _project, session, _root, receiver) = fixture();
    let transport = ScriptedEngramControlTransport::new([
        checkpoint_reply("retained-grant"),
        checkpoint_reply("next-grant"),
        checkpoint_reply("last-grant"),
    ]);
    state.install_control_test_transport(transport.clone());
    assert!(matches!(
        close(&state, &session),
        EngramCheckpointOutcome::Succeeded
    ));
    let (original, response, card) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        let original = record.engram.checkpoint_operation.clone().unwrap();
        let EngramExistingCheckpointProgress::Accepted { response } = &original.progress else {
            panic!("accepted")
        };
        let card = record
            .session
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::EngramControl { card, .. }
                    if card.stage == EngramControlStage::Checkpoint =>
                {
                    Some(card.clone())
                }
                _ => None,
            })
            .unwrap();
        (original.clone(), response.clone(), card)
    };
    // Model an older callback still holding its bounded custody reservation.
    // Publishing a successor must retain its complete original operation.
    let reservation = state.reserve_checkpoint_receipt(&original).unwrap();
    successor_grant(&state, &session, "next-grant");
    assert!(matches!(
        close(&state, &session),
        EngramCheckpointOutcome::Succeeded
    ));
    {
        let inner = state.inner.lock().unwrap();
        assert_eq!(
            inner.engram_session_obligations.checkpoint_operations.len(),
            2
        );
        assert!(
            inner
                .engram_session_obligations
                .checkpoint_operations
                .iter()
                .any(|operation| operation.original.id == original.original.id)
        );
    }
    drop(reservation);
    successor_grant(&state, &session, "last-grant");
    assert!(matches!(
        close(&state, &session),
        EngramCheckpointOutcome::Succeeded
    ));
    let before = {
        let inner = state.inner.lock().unwrap();
        assert_eq!(
            inner.engram_session_obligations.checkpoint_operations.len(),
            1
        );
        assert_eq!(inner.engram_confirmed_containment_ids.len(), 1);
        json!({"revision": inner.revision, "ledger": inner.engram_session_obligations,
            "row": PersistedSessionRecord::from_record(&inner.sessions[inner.find_session_index(&session).unwrap()])})
    };
    assert!(!state.finish_engram_checkpoint_record_with_operation(
        &session,
        "retained-grant",
        EngramControlCardDecision::Grant,
        card,
        None,
        false,
        Some(&original),
        Some(response),
        Some(state.engram_budget_clock().now() + ENGRAM_CONTROL_SETTLE_TIMEOUT),
        Some(EngramCheckpointAttemptResult::Accepted),
    ));
    let inner = state.inner.lock().unwrap();
    assert_eq!(
        json!({"revision": inner.revision, "ledger": inner.engram_session_obligations,
        "row": PersistedSessionRecord::from_record(&inner.sessions[inner.find_session_index(&session).unwrap()])}),
        before
    );
    assert_eq!(transport.requests().len(), 3);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_checkpoint_review_unlatched_history_and_reused_wire_identity_are_retained() {
    for change in ["unlatched", "same-key"] {
        let (state, _project, session, _root, receiver) = fixture();
        let transport = ScriptedEngramControlTransport::new([checkpoint_reply("retained-grant")]);
        state.install_control_test_transport(transport.clone());
        assert!(matches!(
            close(&state, &session),
            EngramCheckpointOutcome::Succeeded
        ));
        let before = {
            let mut inner = state.inner.lock().unwrap();
            let original = inner.engram_session_obligations.checkpoint_operations[0]
                .original
                .id
                .clone();
            if change == "unlatched" {
                inner.engram_confirmed_containment_ids.remove(&original);
            }
            serde_json::to_value(&inner.engram_session_obligations).unwrap()
        };
        successor_grant(
            &state,
            &session,
            if change == "same-key" {
                "retained-grant"
            } else {
                "next-grant"
            },
        );
        assert!(!matches!(
            close(&state, &session),
            EngramCheckpointOutcome::Succeeded
        ));
        let inner = state.inner.lock().unwrap();
        assert_eq!(
            serde_json::to_value(&inner.engram_session_obligations).unwrap(),
            before,
            "{change}"
        );
        assert_eq!(
            transport.requests().len(),
            1,
            "{change}: no new wire effect"
        );
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn project_containment_checkpoint_review_linked_containment_history_survives_successor() {
    let (state, project, session, receiver, _transport, _request, _retained) =
        checkpoint_overlapped_by_off(false);
    let (original_id, entries) = {
        let mut inner = state.inner.lock().unwrap();
        assert_eq!(inner.engram_session_obligations.entries.len(), 1);
        inner
            .projects
            .iter_mut()
            .find(|entry| entry.id == project)
            .unwrap()
            .engram
            .as_mut()
            .unwrap()
            .turn_gated_control = true;
        (
            inner.engram_session_obligations.checkpoint_operations[0]
                .original
                .id
                .clone(),
            serde_json::to_value(&inner.engram_session_obligations.entries).unwrap(),
        )
    };
    let transport = ScriptedEngramControlTransport::new([checkpoint_reply("next-grant")]);
    state.install_control_test_transport(transport.clone());
    successor_grant(&state, &session, "next-grant");
    assert!(matches!(
        close(&state, &session),
        EngramCheckpointOutcome::Succeeded
    ));
    let inner = state.inner.lock().unwrap();
    assert!(
        inner
            .engram_session_obligations
            .checkpoint_operations
            .iter()
            .any(|operation| operation.original.id == original_id)
    );
    assert_eq!(
        serde_json::to_value(&inner.engram_session_obligations.entries).unwrap(),
        entries
    );
    assert_eq!(transport.requests().len(), 1);
    assert!(receiver.try_recv().is_err());
}

fn queue_head(state: &AppState, session: &str) -> EngramQueuedAdmissionOwner {
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(session).unwrap();
    let id = inner.next_message_id();
    queue_prompt_on_record_with_source(
        &mut inner.sessions[index],
        PendingPrompt {
            id,
            timestamp: stamp_now(),
            text: "Checkpoint successor".into(),
            expanded_text: None,
            source: None,
            attachments: Vec::new(),
            engram_interrupted: false,
            is_engram_retained: false,
        },
        Vec::new(),
        QueuedPromptSource::User,
    );
    let owner = EngramQueuedAdmissionOwner::capture(&inner.sessions[index]).unwrap();
    state.commit_locked(&mut inner).unwrap();
    owner
}

fn certified_close() -> (
    AppState,
    String,
    EngramQueuedAdmissionOwner,
    Arc<ScriptedEngramControlTransport>,
    mpsc::Receiver<CodexRuntimeCommand>,
) {
    let (state, _project, session, _root, receiver) = fixture();
    let transport = ScriptedEngramControlTransport::new([
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::remote(
            EngramControlErrorBody {
                code: "control_store_busy".into(),
                message: "fixture busy".into(),
            },
        ))),
        checkpoint_reply("retained-grant"),
    ]);
    state.install_control_test_transport(transport.clone());
    assert!(matches!(
        close(&state, &session),
        EngramCheckpointOutcome::Failed(_)
    ));
    let owner = queue_head(&state, &session);
    (state, session, owner, transport, receiver)
}

#[test]
fn project_containment_checkpoint_review_exact_queued_owner_replays_only_original_close() {
    let (state, session, owner, transport, receiver) = certified_close();
    {
        let inner = state.inner.lock().unwrap();
        assert!(
            certified_checkpoint_replay_target_locked(&inner, &session, Some(&owner)).is_some()
        );
        assert!(engram_admission_obligation_locked(&inner, &session).is_some());
    }
    // Admission remains excluded; the explicit checkpoint action alone owns
    // settlement of the selected operation, with no Evaluate/Begin or delivery.
    assert!(
        state
            .require_queued_engram_owner(&session, &owner, "fresh admission")
            .is_err()
    );
    state
        .settle_certified_checkpoint_before_admission(&session, Some(&owner))
        .unwrap();
    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].request, requests[1].request);
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(owner.matches(record));
    assert!(record.engram.active_grant_id.is_none());
    assert!(engram_session_obligation_locked(&inner, &session).is_none());
    assert_eq!(record.queued_prompts.len(), 1);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_checkpoint_review_changed_owner_and_independent_duty_never_replay() {
    for change in ["owner", "independent"] {
        let (state, session, owner, transport, receiver) = certified_close();
        let target = {
            let mut inner = state.inner.lock().unwrap();
            let target =
                certified_checkpoint_replay_target_locked(&inner, &session, Some(&owner)).unwrap();
            let index = inner.find_session_index(&session).unwrap();
            if change == "owner" {
                inner.sessions[index]
                    .queued_prompts
                    .front_mut()
                    .unwrap()
                    .pending_prompt
                    .id
                    .push_str("-replaced");
            } else {
                let mut other = inner.engram_session_obligations.checkpoint_operations[0].clone();
                other.original.id = Uuid::new_v4().to_string();
                if let EngramControlRequest::TurnCheckpoint {
                    grant_id,
                    idempotency_key,
                    ..
                } = &mut other.original.request
                {
                    grant_id.push_str("-independent");
                    idempotency_key.push_str("-independent");
                    other.protection.grant_id = grant_id.clone();
                }
                other.protection.id = other.original.id.clone();
                other.protection.frozen_request = serde_json::to_value(&other.original).unwrap();
                other.attempts[0].id = other.original.id.clone();
                inner
                    .engram_session_obligations
                    .checkpoint_operations
                    .push(other);
                inner.engram_session_obligations.validate().unwrap();
            }
            state.commit_locked(&mut inner).unwrap();
            target
        };
        assert!(
            state
                .retry_certified_engram_checkpoint(&target, Some(&owner), None)
                .is_err(),
            "{change}"
        );
        assert_eq!(
            transport.requests().len(),
            1,
            "{change}: no retry was authorized"
        );
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn project_containment_checkpoint_review_skipped_evaluation_stays_held_after_close_ack() {
    let (state, _project, session, _root, receiver) = fixture();
    let (step, gate) = gated_engram_step("turn_checkpoint", checkpoint_reply("retained-grant"));
    let transport = GatedEngramControlTransport::new([step]);
    state.install_control_test_transport(transport.clone());
    let worker_state = state.clone();
    let worker_session = session.clone();
    let closing = std::thread::spawn(move || close(&worker_state, &worker_session));
    gate.wait();
    let owner = queue_head(&state, &session);
    let intent = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
        let head = record.queued_prompts.front().unwrap();
        EngramTurnIntentSnapshot {
            session_id: session.clone(),
            dispatch_generation: owner.generation,
            intent_fingerprint: engram_turn_intent_fingerprint(
                &head.pending_prompt.text,
                head.pending_prompt.expanded_text.as_deref(),
                &head.attachments,
                head.pending_prompt.source.as_ref(),
                head.source,
            ),
        }
    };
    let pending = state.evaluate_engram_turn_off_lock(&intent);
    // Always release and join before assertions, including on a causal RED.
    gate.release();
    let outcome = closing.join().unwrap();
    assert!(pending.is_none());
    assert!(matches!(outcome, EngramCheckpointOutcome::Succeeded));
    let held = {
        let mut inner = state.inner.lock().unwrap();
        assert!(engram_session_obligation_locked(&inner, &session).is_none());
        assert!(AppState::engram_session_requires_dispatch_card_locked(
            &inner, &session
        ));
        let index = inner.find_session_index(&session).unwrap();
        state
            .start_next_queued_turn_locked(&mut inner, index, true, pending)
            .unwrap()
            .is_none()
    };
    assert!(
        held,
        "an obligation-skipped evaluation cannot become an ungated promotion after ACK"
    );
    let inner = state.inner.lock().unwrap();
    assert!(owner.matches(&inner.sessions[inner.find_session_index(&session).unwrap()]));
    assert_eq!(transport.requests().len(), 1);
    assert!(receiver.try_recv().is_err());
}
