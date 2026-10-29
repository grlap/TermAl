// Owns ended checkpoint-flight cleanup and independent receipt custody.
// Does not authorize a general recovery producer or destructive duty bypass.
use super::*;

fn attach_existing_checkpoint_child(state: &AppState, parent: &str, child: &str) {
    let (delegation, placeholder) =
        crate::tests::delegation_support::install_required_review_delegation(state, parent);
    let mut inner = state.inner.lock().unwrap();
    inner.delegations.iter_mut().find(|row| row.id == delegation).unwrap()
        .child_session_id = child.to_owned();
    inner.session_mut(child).unwrap().session.parent_delegation_id = Some(delegation);
    let index = inner.find_session_index(&placeholder).unwrap();
    inner.remove_session_at(index).unwrap();
    state.commit_locked(&mut inner).unwrap();
}

fn assert_checkpoint_subtree_retained(state: &AppState, parent: &str, child: &str) {
    let inner = state.inner.lock().unwrap();
    assert!(inner.find_session_index(parent).is_some());
    assert!(inner.find_session_index(child).is_some());
    assert!(engram_session_obligation_locked(&inner, child).is_some());
    assert!(!inner.removed_session_ids.contains(&parent.to_owned()));
    assert!(!inner.removed_session_ids.contains(&child.to_owned()));
    drop(inner);
    let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    assert!(loaded.find_session_index(parent).is_some());
    assert!(loaded.find_session_index(child).is_some());
    assert!(engram_session_obligation_locked(&loaded, child).is_some());
}

#[test]
fn project_containment_checkpoint_review_cut_parent_delete_keeps_existing_child_receipt_custody() {
    let (state, project, child, transport) = checkpoint_with_unacknowledged_send();
    {
        let mut inner = state.inner.lock().unwrap();
        let operation = inner.engram_session_obligations.checkpoint_operations[0].clone();
        inner.engram_pending_checkpoint_receipts.reserve(&operation).unwrap();
    }
    let root = state.inner.lock().unwrap().sessions[0].session.workdir.clone();
    let parent = create_test_project_session(&state, Agent::Codex, &project, FsPath::new(&root));
    attach_existing_checkpoint_child(&state, &parent, &child);
    let before = serde_json::to_value(&state.inner.lock().unwrap()
        .engram_session_obligations).unwrap();
    let result = state.kill_session(&parent);
    assert!(matches!(result, Err(ref error) if error.status == StatusCode::CONFLICT),
        "a parent cannot delete a pre-existing protected child");
    assert_checkpoint_subtree_retained(&state, &parent, &child);
    assert_eq!(serde_json::to_value(&state.inner.lock().unwrap()
        .engram_session_obligations).unwrap(), before);
    assert!(transport.requests().is_empty());
}

#[test]
fn project_containment_checkpoint_review_cut_parent_delete_rechecks_late_child_receipt_custody() {
    let (state, project, child, _transport) = checkpoint_with_unacknowledged_send();
    {
        let mut inner = state.inner.lock().unwrap();
        let operation = inner.engram_session_obligations.checkpoint_operations[0].clone();
        inner.engram_pending_checkpoint_receipts.reserve(&operation).unwrap();
    }
    let root = state.inner.lock().unwrap().sessions[0].session.workdir.clone();
    let parent = create_test_project_session(&state, Agent::Codex, &project, FsPath::new(&root));
    {
        let mut inner = state.inner.lock().unwrap();
        let record = inner.session_mut(&parent).unwrap();
        record.session.status = SessionStatus::Error;
        record.runtime = SessionRuntime::None;
        record.engram.routing_token = Some("parent-token".into());
        record.engram.active_grant_id = Some("parent-grant".into());
        record.engram.begins_recorded = true;
        state.commit_locked(&mut inner).unwrap();
    }
    let (step, gate) = gated_engram_step("turn_checkpoint", checkpoint_reply("parent-grant"));
    let transport = GatedEngramControlTransport::new([step]);
    state.install_control_test_transport(transport.clone());
    let deleting_state = state.clone();
    let deleting_parent = parent.clone();
    let deleting = std::thread::spawn(move || deleting_state.kill_session(&deleting_parent));
    gate.wait();
    // The child joins this removal set only after the initial precheck.
    attach_existing_checkpoint_child(&state, &parent, &child);
    gate.release();
    let result = deleting.join().unwrap();
    assert!(matches!(result, Err(ref error) if error.status == StatusCode::CONFLICT),
        "final removal must recheck a newly attached protected child");
    assert_checkpoint_subtree_retained(&state, &parent, &child);
    assert_eq!(transport.requests().len(), 1);
}

#[test]
fn project_containment_checkpoint_failed_presend_ack_releases_only_local_flight() {
    let (state, _project, session, transport) = checkpoint_with_unacknowledged_send();
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(
        !record.engram.checkpoint_in_progress,
        "the caller returned before transport; no live local checkpoint remains"
    );
    assert!(engram_session_obligation_locked(&inner, &session).is_some());
    assert!(inner.engram_confirmed_containment_ids.is_empty());
    assert!(transport.requests().is_empty());
    assert!(matches!(
        record
            .engram
            .checkpoint_operation
            .as_ref()
            .unwrap()
            .progress,
        EngramExistingCheckpointProgress::Retained
    ));
}

#[test]
fn project_containment_checkpoint_failed_initial_commit_releases_only_local_flight() {
    let (state, _project, session, root, receiver) = fixture();
    let transport = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(transport.clone());
    let failing = AppState {
        persistence_path: Arc::new(root),
        ..state.clone()
    };
    let result = failing.checkpoint_engram_turn_off_lock(
        &session,
        EngramCheckpointPurpose::Settlement,
        None,
        None,
        EngramNextIntent::Wait,
        None,
        None,
    );
    assert!(matches!(result, EngramCheckpointOutcome::Failed(_)));
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(
        !record.engram.checkpoint_in_progress,
        "initial commit failure must end the exact local invocation reservation"
    );
    assert!(engram_session_obligation_locked(&inner, &session).is_some());
    assert_eq!(
        record.engram.active_grant_id.as_deref(),
        Some("retained-grant")
    );
    assert!(inner.engram_confirmed_containment_ids.is_empty());
    assert!(transport.requests().is_empty());
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_checkpoint_runtime_replacement_keeps_receipt_without_leaking_flight() {
    let (state, project, session, _root, receiver) = fixture();
    let (step, gate) = gated_engram_step("turn_checkpoint", checkpoint_reply("retained-grant"));
    let transport = GatedEngramControlTransport::new([step]);
    state.install_control_test_transport(transport.clone());
    let closing_state = state.clone();
    let closing_session = session.clone();
    let closing = std::thread::spawn(move || {
        closing_state.checkpoint_engram_turn_off_lock(
            &closing_session,
            EngramCheckpointPurpose::Settlement,
            None,
            None,
            EngramNextIntent::Wait,
            None,
            None,
        )
    });
    let original = gate.wait();
    let successor = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let record = &mut inner.sessions[index];
        let (runtime, _input, process) = test_shared_codex_runtime("new-checkpoint-runtime");
        record.runtime = SessionRuntime::Codex(CodexRuntimeHandle {
            runtime_id: runtime.runtime_id.clone(),
            input_tx: runtime.input_tx.clone(),
            process,
            shared_session: None,
        });
        record.engram.dispatch_generation += 1;
        record.active_turn_generation += 1;
        record.engram.active_grant_id = Some("successor-grant".into());
        let successor = (
            record.engram.dispatch_generation,
            record.active_turn_generation,
            format!("{:?}", record.runtime.runtime_token()),
        );
        state.commit_locked(&mut inner).unwrap();
        successor
    };
    gate.release();
    assert!(matches!(
        closing.join().unwrap(),
        EngramCheckpointOutcome::Succeeded
    ));
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(
        !record.engram.checkpoint_in_progress,
        "an ended old caller must not leave its local flight on the replacement row"
    );
    assert_eq!(
        (
            record.engram.dispatch_generation,
            record.active_turn_generation,
            format!("{:?}", record.runtime.runtime_token())
        ),
        successor
    );
    assert_eq!(
        record.engram.active_grant_id.as_deref(),
        Some("successor-grant")
    );
    let operation = &inner.engram_session_obligations.checkpoint_operations[0];
    assert_eq!(
        serde_json::to_value(&operation.original.request).unwrap(),
        original.request
    );
    assert!(matches!(
        operation.progress,
        EngramExistingCheckpointProgress::Accepted { .. }
    ));
    assert!(
        engram_session_obligation_locked(&inner, &session).is_some(),
        "the old receipt cannot discharge the new runtime/grant's independent custody"
    );
    let debt = serde_json::to_value(&inner.engram_session_obligations).unwrap();
    drop(inner);
    let no_rpc = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(no_rpc.clone());
    for reload in [false, true] {
        if reload {
            state.shutdown_persist_blocking();
            *state.inner.lock().unwrap() = load_state(state.persistence_path.as_path()).unwrap().unwrap();
            select_scripted_engram_budget_clock_before_enable(&state);
        }
        let mut disabled = settings(&state, &project, false);
        disabled.enabled = false;
        assert_eq!(state.update_project_engram_settings(&project, disabled).err().unwrap().status,
            StatusCode::CONFLICT);
        assert_eq!(state.kill_session(&session).err().unwrap().status, StatusCode::CONFLICT);
        assert_eq!(state.delete_project(&project).err().unwrap().status, StatusCode::CONFLICT);
        let inner = state.inner.lock().unwrap();
        assert_eq!(inner.sessions[inner.find_session_index(&session).unwrap()]
            .engram.active_grant_id.as_deref(), Some("successor-grant"));
        assert_eq!(serde_json::to_value(&inner.engram_session_obligations).unwrap(), debt);
    }
    assert!(no_rpc.requests().is_empty());
    assert_eq!(transport.requests().len(), 1);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_checkpoint_old_cleanup_preserves_successor_attempt_reservation() {
    let (state, _project, session, _root, _receiver) = fixture();
    let (step, gate) = gated_engram_step("turn_checkpoint", checkpoint_reply("retained-grant"));
    let transport = GatedEngramControlTransport::new([step]);
    state.install_control_test_transport(transport.clone());
    let closing_state = state.clone();
    let closing_session = session.clone();
    let closing = std::thread::spawn(move || {
        closing_state.checkpoint_engram_turn_off_lock(
            &closing_session,
            EngramCheckpointPurpose::Settlement,
            None,
            None,
            EngramNextIntent::Wait,
            None,
            None,
        )
    });
    gate.wait();
    let successor_id = {
        let mut inner = state.inner.lock().unwrap();
        let index = inner.find_session_index(&session).unwrap();
        let mut operation = inner.sessions[index]
            .engram
            .checkpoint_operation
            .clone()
            .unwrap();
        let mut successor = operation.attempts.last().unwrap().clone();
        successor.predecessor = Some(successor.id.clone());
        successor.id = Uuid::new_v4().to_string();
        let id = successor.id.clone();
        operation.attempts.push(successor);
        operation.validate().unwrap();
        inner.sessions[index].engram.checkpoint_operation = Some(operation.clone());
        inner.engram_session_obligations.checkpoint_operations[0] = operation;
        state.commit_locked(&mut inner).unwrap();
        id
    };
    gate.release();
    assert!(matches!(
        closing.join().unwrap(),
        EngramCheckpointOutcome::Succeeded
    ));
    let inner = state.inner.lock().unwrap();
    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
    assert!(
        record.engram.checkpoint_in_progress,
        "old cleanup cannot clear a newer attempt"
    );
    let operation = record.engram.checkpoint_operation.as_ref().unwrap();
    assert_eq!(operation.attempts.last().unwrap().id, successor_id);
    assert!(operation.attempts.last().unwrap().result.is_none());
    assert!(engram_session_obligation_locked(&inner, &session).is_some());
    assert_eq!(transport.requests().len(), 1);
}

#[test]
fn project_containment_checkpoint_teardown_unknown_keeps_deleted_session_duty_through_reload() {
    let (state, project, session, _root, receiver) = fixture();
    let transport =
        ScriptedEngramControlTransport::new([ScriptedEngramControlResponse::Reply(Err(
            EngramTransportError::deadline("fixture teardown reply lost"),
        ))]);
    state.install_control_test_transport(transport.clone());
    // This starts unexcluded and exercises the already-supported removal lifecycle.
    state.kill_session(&session).unwrap();
    let request = transport.requests()[0].request.clone();
    assert_eq!(request["operation"], "turn_checkpoint");
    assert_eq!(request["next_intent"], "exit");
    state.shutdown_persist_blocking();
    let loaded = load_state(state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(loaded.find_session_index(&session).is_none());
    let duty = engram_session_obligation_locked(&loaded, &session).unwrap();
    assert_eq!(duty.project_id, project);
    assert!(contains_exact_json(&duty.frozen_request, &request));
    assert!(!loaded.engram_confirmed_containment_ids.contains(&duty.id));
    assert_eq!(engram_control_recovery_summaries(&loaded).len(), 1);
    assert_eq!(transport.requests().len(), 1);
    assert!(receiver.try_recv().is_err());
    let debt = serde_json::to_value(&loaded.engram_session_obligations).unwrap();
    state.delete_project(&project).unwrap();
    let after = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    assert!(after.find_project(&project).is_none());
    assert_eq!(serde_json::to_value(&after.engram_session_obligations).unwrap(), debt);
    assert_eq!(transport.requests().len(), 1);
}

#[test]
fn project_containment_checkpoint_administrative_removal_retains_original_debt_after_reload() {
    for outcome in ["retained", "unknown", "certified"] {
        for action in ["disable", "session_delete", "project_delete"] {
            let (state, project, session, _root, receiver) = fixture();
            let response = if outcome == "certified" {
                EngramTransportError::remote(EngramControlErrorBody {
                    code: "control_store_busy".into(), message: "fixture not applied".into(),
                })
            } else {
                EngramTransportError::deadline("fixture original close reply unavailable")
            };
            let transport = ScriptedEngramControlTransport::new([
                ScriptedEngramControlResponse::Reply(Err(response)),
            ]);
            state.install_control_test_transport(transport.clone());
            if outcome == "retained" {
                TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(false));
            }
            let result = state.checkpoint_engram_turn_off_lock(
                &session, EngramCheckpointPurpose::Settlement, None, None,
                EngramNextIntent::Wait, None, None,
            );
            TEST_ENGRAM_AUTHORITY_MANUAL_WRITER.with(|allowed| allowed.set(true));
            assert!(matches!(result, EngramCheckpointOutcome::Failed(_)));
            if outcome == "retained" {
                let image = {
                    let inner = state.inner.lock().unwrap();
                    EngramProjectContainmentFence::capture(&inner, &project, &[session.clone()]).unwrap()
                };
                state.confirm_engram_containment(image).unwrap();
            }
            state.shutdown_persist_blocking();
            let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
            let index = loaded.find_session_index(&session).unwrap();
            assert!(!loaded.sessions[index].engram.checkpoint_in_progress);
            let operation = &loaded.engram_session_obligations.checkpoint_operations[0];
            match outcome {
                "retained" => {
                    assert!(matches!(operation.progress, EngramExistingCheckpointProgress::Retained));
                    assert!(operation.attempts.last().unwrap().result.is_none());
                }
                "unknown" => {
                    assert!(matches!(operation.progress, EngramExistingCheckpointProgress::PossiblySent));
                    assert!(matches!(operation.attempts.last().unwrap().result,
                        Some(EngramCheckpointAttemptResult::Unknown { .. })));
                }
                "certified" => {
                    assert!(matches!(operation.progress, EngramExistingCheckpointProgress::PossiblySent));
                    assert!(matches!(operation.attempts.last().unwrap().result,
                        Some(EngramCheckpointAttemptResult::CertifiedNotApplied { .. })));
                }
                _ => unreachable!(),
            }
            let before = serde_json::to_value(&loaded.engram_session_obligations).unwrap();
            assert!(require_no_engram_admission_obligation(&loaded, &session).is_err());
            *state.inner.lock().unwrap() = loaded;
            select_scripted_engram_budget_clock_before_enable(&state);
            let no_rpc = ScriptedEngramControlTransport::new([]);
            state.install_control_test_transport(no_rpc.clone());
            match action {
                "disable" => {
                    let mut disabled = settings(&state, &project, true);
                    disabled.enabled = false;
                    state.update_project_engram_settings(&project, disabled).unwrap();
                }
                "session_delete" => { state.kill_session(&session).unwrap(); }
                "project_delete" => { state.delete_project(&project).unwrap(); }
                _ => unreachable!(),
            }
            state.shutdown_persist_blocking();
            let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
            assert_eq!(serde_json::to_value(&loaded.engram_session_obligations).unwrap(), before,
                "{outcome}/{action} must not settle, rewrite or prune the original report");
            assert!(engram_session_obligation_locked(&loaded, &session).is_some());
            assert!(require_no_engram_admission_obligation(&loaded, &session).is_err());
            match action {
                "disable" => {
                    assert!(!loaded.find_project(&project).unwrap().engram.as_ref().unwrap().enabled);
                    assert!(loaded.sessions[loaded.find_session_index(&session).unwrap()]
                        .engram.checkpoint_operation.is_some());
                }
                "session_delete" => assert!(loaded.find_session_index(&session).is_none()),
                "project_delete" => assert!(loaded.find_project(&project).is_none()),
                _ => unreachable!(),
            }
            assert!(no_rpc.requests().is_empty());
            assert_eq!(transport.requests().len(), usize::from(outcome != "retained"));
            assert!(receiver.try_recv().is_err());
        }
    }
}

fn assert_administrative_checkpoint_action(state: &AppState, project: &str, session: &str, action: &str) {
    let (debt, mirror, grant, token, confirmed_off) = {
        let inner = state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(session).unwrap()];
        (serde_json::to_value(&inner.engram_session_obligations).unwrap(),
            serde_json::to_value(&record.engram.checkpoint_operation).unwrap(),
            record.engram.active_grant_id.clone(), record.engram.routing_token.clone(),
            inner.engram_session_obligations.entries.iter().any(|entry|
                entry.basis == EngramSessionObligationBasis::CheckpointUnsettled
                    && entry.phase == EngramSessionObligationPhase::Confirmed))
    };
    let no_rpc = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(no_rpc.clone());
    match action {
        "disable" => {
            let mut disabled = settings(state, project, false);
            disabled.enabled = false;
            state.update_project_engram_settings(project, disabled).unwrap();
        }
        "session_delete" => { state.kill_session(session).unwrap(); }
        "project_delete" => { state.delete_project(project).unwrap(); }
        _ => unreachable!(),
    }
    state.shutdown_persist_blocking();
    let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    assert_eq!(serde_json::to_value(&loaded.engram_session_obligations).unwrap(), debt);
    if confirmed_off && action == "session_delete" {
        // Existing boot semantics may materialize actual exact receipt evidence
        // after its owner is absent. Administrative deletion adds no evidence.
        let entry = loaded.engram_session_obligations.entries.iter().find(|entry|
            entry.basis == EngramSessionObligationBasis::CheckpointUnsettled).unwrap();
        assert!(engram_containment_settlement_evidence_matches(entry));
        assert!(loaded.engram_confirmed_containment_ids.contains(&entry.id));
        assert!(require_no_engram_admission_obligation(&loaded, session).is_ok());
    } else {
        assert!(require_no_engram_admission_obligation(&loaded, session).is_err());
    }
    if let Some(index) = loaded.find_session_index(session) {
        assert_eq!(loaded.sessions[index].engram.active_grant_id, grant);
        assert_eq!(loaded.sessions[index].engram.routing_token, token);
    }
    match action {
        "disable" => {
            assert!(!loaded.find_project(project).unwrap().engram.as_ref().unwrap().enabled);
            assert_eq!(serde_json::to_value(&loaded.sessions[loaded.find_session_index(session).unwrap()]
                .engram.checkpoint_operation).unwrap(), mirror);
        }
        "session_delete" => assert!(loaded.find_session_index(session).is_none()),
        "project_delete" => assert!(loaded.find_project(project).is_none()),
        _ => unreachable!(),
    }
    assert!(no_rpc.requests().is_empty());
}

#[test]
fn project_containment_successful_reset_drops_its_closed_grant_identity() {
    for action in ["disable", "project_delete"] {
        let (state, project, session, _root, receiver) = fixture();
        let transport = ScriptedEngramControlTransport::new([
            checkpoint_reply("retained-grant"),
        ]);
        state.install_control_test_transport(transport.clone());
        if action == "disable" {
            let mut disabled = settings(&state, &project, true);
            disabled.enabled = false;
            state.update_project_engram_settings(&project, disabled).unwrap();
        } else {
            state.delete_project(&project).unwrap();
        }
        state.shutdown_persist_blocking();
        let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
        let record = &loaded.sessions[loaded.find_session_index(&session).unwrap()];
        assert!(record.engram.active_grant_id.is_none());
        assert!(record.engram.routing_token.is_none());
        assert!(record.engram.checkpoint_operation.is_none());
        assert!(loaded.engram_session_obligations.all_protections().next().is_none());
        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].request["operation"], "turn_checkpoint");
        assert_eq!(requests[0].request["next_intent"], "exit");
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn project_containment_off_report_debt_allows_only_fresh_unmediated_local_turns() {
    for reload in [false, true] {
        for disabled in [false, true] {
            let (state, project, session, receiver, transport, _, _) = checkpoint_overlapped_by_off(true);
            if disabled {
                let mut next = settings(&state, &project, false);
                next.enabled = false;
                state.update_project_engram_settings(&project, next).unwrap();
            }
            if reload {
                state.shutdown_persist_blocking();
                *state.inner.lock().unwrap() = load_state(state.persistence_path.as_path()).unwrap().unwrap();
                select_scripted_engram_budget_clock_before_enable(&state);
            }
            let debt = serde_json::to_value(&state.inner.lock().unwrap().engram_session_obligations).unwrap();
            let no_rpc = ScriptedEngramControlTransport::new([]);
            state.install_control_test_transport(no_rpc.clone());
            let mut fresh = prompt();
            fresh.text = "A fresh local turn, never the original prompt.".into();
            let dispatch = match state.dispatch_turn(&session, fresh).unwrap() {
                DispatchTurnResult::Dispatched(dispatch) => dispatch,
                DispatchTurnResult::Queued | DispatchTurnResult::DispatchedAfterQueue(_) =>
                    panic!("inactive OFF report debt must dispatch the fresh prompt directly"),
            };
            assert!(dispatch.engram_dispatch_generation().is_none());
            let token = dispatch.runtime_token().clone();
            assert!(matches!(deliver_turn_dispatch(&state, dispatch), TurnDispatchDeliveryOutcome::Delivered));
            assert!(matches!(receiver.recv_timeout(DEADLOCK_GUARD).unwrap(), CodexRuntimeCommand::Prompt { .. }));
            state.finish_turn_ok_if_runtime_matches(&session, &token).unwrap();
            let inner = state.inner.lock().unwrap();
            assert_eq!(serde_json::to_value(&inner.engram_session_obligations).unwrap(), debt);
            assert_eq!(inner.sessions[inner.find_session_index(&session).unwrap()]
                .engram.active_grant_id.as_deref(), Some("retained-grant"));
            assert!(engram_session_obligation_locked(&inner, &session).is_some());
            assert!(AppState::engram_binding_target_for_session_shape_locked(&inner, &session, true)
                .unwrap().is_none());
            drop(inner);
            assert!(no_rpc.requests().is_empty());
            assert_eq!(transport.requests().len(), 1);
        }
    }
}

#[test]
fn project_containment_off_report_dispatch_rechecks_on_and_independent_duties() {
    for blocker in ["on_before", "on_handoff", "disabled_on_before", "disabled_on_handoff",
        "live", "begin", "successor"] {
        let (state, project, session, receiver, _transport, _, _) = checkpoint_overlapped_by_off(true);
        let no_rpc = ScriptedEngramControlTransport::new([]);
        state.install_control_test_transport(no_rpc.clone());
        let debt = serde_json::to_value(&state.inner.lock().unwrap().engram_session_obligations).unwrap();
        if !matches!(blocker, "on_handoff" | "disabled_on_handoff") {
            let mut inner = state.inner.lock().unwrap();
            if matches!(blocker, "on_before" | "disabled_on_before") {
                inner.projects.iter_mut().find(|p| p.id == project).unwrap()
                    .engram.as_mut().unwrap().turn_gated_control = true;
                if blocker == "disabled_on_before" {
                    AppState::record_engram_transport_failure_locked(&mut inner, &session,
                        &EngramTransportError::protocol("fixture invalid checkpoint result"));
                    assert!(inner.sessions[inner.find_session_index(&session).unwrap()]
                        .engram.disabled_reason.is_some());
                    assert!(AppState::engram_session_requires_dispatch_card_locked(&inner, &session));
                }
            } else {
                let record = inner.session_mut(&session).unwrap();
                match blocker {
                    "live" => record.session.status = SessionStatus::Active,
                    "begin" => record.engram.begins_recorded = false,
                    "successor" => record.engram.active_grant_id = Some("independent-grant".into()),
                    _ => unreachable!(),
                }
            }
            state.commit_locked(&mut inner).unwrap();
            assert!(!engram_unmediated_checkpoint_dispatch_allowed(&inner, &session, None));
            drop(inner);
            let result = state.dispatch_turn(&session, prompt());
            assert!(!matches!(result, Ok(DispatchTurnResult::Dispatched(_)
                | DispatchTurnResult::DispatchedAfterQueue(_))));
        } else {
            let dispatch = match state.dispatch_turn(&session, prompt()).unwrap() {
                DispatchTurnResult::Dispatched(dispatch) => dispatch,
                DispatchTurnResult::Queued | DispatchTurnResult::DispatchedAfterQueue(_) =>
                    panic!("fresh OFF turn must dispatch directly"),
            };
            {
                let mut inner = state.inner.lock().unwrap();
                inner.projects.iter_mut().find(|p| p.id == project).unwrap()
                    .engram.as_mut().unwrap().turn_gated_control = true;
                if blocker == "disabled_on_handoff" {
                    AppState::record_engram_transport_failure_locked(&mut inner, &session,
                        &EngramTransportError::protocol("fixture invalid checkpoint result"));
                    assert!(inner.sessions[inner.find_session_index(&session).unwrap()]
                        .engram.disabled_reason.is_some());
                }
                state.commit_locked(&mut inner).unwrap();
            }
            assert!(matches!(deliver_turn_dispatch(&state, dispatch), TurnDispatchDeliveryOutcome::Held { .. }));
        }
        assert!(receiver.try_recv().is_err());
        assert!(no_rpc.requests().is_empty());
        assert_eq!(serde_json::to_value(&state.inner.lock().unwrap().engram_session_obligations).unwrap(), debt);
    }
}

#[test]
fn project_containment_checkpoint_accepted_unowned_debt_allows_administration_without_settlement() {
    for reload in [false, true] {
        for action in ["disable", "session_delete", "project_delete"] {
            let (state, project, session, _root, receiver) = fixture();
            let (step, gate) = gated_engram_step("turn_checkpoint", checkpoint_reply("retained-grant"));
            let transport = GatedEngramControlTransport::new([step]);
            state.install_control_test_transport(transport.clone());
            let closing_state = state.clone();
            let closing_session = session.clone();
            let closing = std::thread::spawn(move || closing_state.checkpoint_engram_turn_off_lock(
                &closing_session, EngramCheckpointPurpose::Settlement, None, None,
                EngramNextIntent::Wait, None, None));
            let request = gate.wait();
            {
                let mut inner = state.inner.lock().unwrap();
                inner.session_mut(&session).unwrap().active_turn_generation += 1;
                state.commit_locked(&mut inner).unwrap();
            }
            gate.release();
            assert!(matches!(closing.join().unwrap(), EngramCheckpointOutcome::Succeeded));
            {
                let inner = state.inner.lock().unwrap();
                let operation = &inner.engram_session_obligations.checkpoint_operations[0];
                assert!(matches!(operation.progress, EngramExistingCheckpointProgress::Accepted { .. }));
                assert!(matches!(operation.attempts.last().unwrap().result,
                    Some(EngramCheckpointAttemptResult::Accepted)));
                assert_ne!(operation.protection.phase, EngramSessionObligationPhase::Confirmed);
                assert_eq!(serde_json::to_value(&operation.original.request).unwrap(), request.request);
                assert!(inner.engram_pending_checkpoint_receipts.entries.is_empty());
                assert!(engram_session_obligation_locked(&inner, &session).is_some());
            }
            if reload {
                state.shutdown_persist_blocking();
                *state.inner.lock().unwrap() = load_state(state.persistence_path.as_path()).unwrap().unwrap();
                select_scripted_engram_budget_clock_before_enable(&state);
            }
            assert_administrative_checkpoint_action(&state, &project, &session, action);
            assert_eq!(transport.requests().len(), 1);
            assert!(receiver.try_recv().is_err());
        }
    }
}

#[test]
fn project_containment_off_checkpoint_inactive_debt_allows_administration_without_settlement() {
    for outcome in ["unknown", "refused", "capacity"] {
        for reload in [false, true] {
            for action in ["disable", "session_delete", "project_delete"] {
                let (state, project, session, _root, receiver) = fixture();
                let response = if outcome == "refused" { checkpoint_refusal_reply("GrantScopeMismatch") }
                    else { ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline("fixture OFF reply lost"))) };
                let transport = ScriptedEngramControlTransport::new([
                    response,
                    super::super::recovery::grant_read(&session, "retained-grant", "begun"),
                ]);
                install_control_only_transport(&state, transport.clone());
                if outcome == "capacity" {
                    state.inner.lock().unwrap().engram_pending_checkpoint_receipts.reserved_bytes = ENGRAM_CHECKPOINT_PENDING_MAX_BYTES;
                }
                state.update_project_engram_settings(&project, settings(&state, &project, false)).unwrap();
                let initial_requests = transport.requests().iter()
                    .map(|request| request.request.clone()).collect::<Vec<_>>();
                let operations = initial_requests.iter().map(|request|
                    request["operation"].as_str().unwrap()).collect::<Vec<_>>();
                assert_eq!(operations, if outcome == "capacity" { vec![] }
                    else { vec!["turn_checkpoint", "turn_grant_read"] });
                {
                    let mut inner = state.inner.lock().unwrap();
                    inner.engram_pending_checkpoint_receipts.reserved_bytes = 0;
                    assert!(inner.engram_pending_checkpoint_receipts.entries.is_empty());
                    let entry = &inner.engram_session_obligations.entries[0];
                    assert_eq!(entry.basis, EngramSessionObligationBasis::CheckpointUnsettled);
                    assert_eq!(entry.phase, EngramSessionObligationPhase::Sent);
                    assert!(inner.engram_session_obligations.checkpoint_operations.is_empty());
                    assert!(entry.settlement_evidence.is_null());
                }
                if reload {
                    state.shutdown_persist_blocking();
                    *state.inner.lock().unwrap() = load_state(state.persistence_path.as_path()).unwrap().unwrap();
                    select_scripted_engram_budget_clock_before_enable(&state);
                }
                assert_administrative_checkpoint_action(&state, &project, &session, action);
                assert_eq!(transport.requests().iter().map(|request| request.request.clone())
                    .collect::<Vec<_>>(), initial_requests,
                    "administration must not add a checkpoint or grant read");
                assert!(receiver.try_recv().is_err());
            }
        }
    }
}

#[test]
fn project_containment_exact_off_receipt_is_the_only_administrative_reload_release() {
    for action in ["disable", "session_delete", "project_delete"] {
        let (state, project, session, transport, receiver) = super::receipts::pending_off_receipt();
        let before = serde_json::to_value(&state.inner.lock().unwrap().engram_session_obligations).unwrap();
        let refused = match action {
            "disable" => {
                let mut disabled = settings(&state, &project, false);
                disabled.enabled = false;
                state.update_project_engram_settings(&project, disabled)
            }
            "session_delete" => state.kill_session(&session),
            "project_delete" => state.delete_project(&project),
            _ => unreachable!(),
        };
        assert_eq!(refused.err().expect("live receipt owner retains its row").status, StatusCode::CONFLICT);
        {
            let mut inner = state.inner.lock().unwrap();
            assert_eq!(serde_json::to_value(&inner.engram_session_obligations).unwrap(), before);
            let key = inner.engram_pending_checkpoint_receipts.entries.keys().next().unwrap().clone();
            assert!(!inner.engram_pending_checkpoint_receipts.entries[&key].accounting);
            inner.session_mut(&session).unwrap().active_turn_generation += 1;
            assert!(!checkpoint_receipt_has_live_owner(&inner, &key.0));
            inner.engram_pending_checkpoint_receipts.entries.get_mut(&key).unwrap().accounting = true;
            assert!(checkpoint_receipt_has_live_owner(&inner, &key.0));
            inner.engram_pending_checkpoint_receipts.entries.get_mut(&key).unwrap().accounting = false;
            state.commit_locked(&mut inner).unwrap();
        }
        assert_administrative_checkpoint_action(&state, &project, &session, action);
        assert_eq!(transport.requests().len(), 1);
        assert!(receiver.try_recv().is_err());
    }
}
