// Owns settings mutation races with an independently published checkpoint duty.
// Does not authorize replacement, replay or discharge of that duty.
use super::*;

fn publish_lost_closing_checkpoint(state: &AppState, session: &str) -> Value {
    let (step, gate) = gated_engram_step(
        "turn_checkpoint",
        ScriptedEngramControlResponse::Reply(Err(EngramTransportError::deadline(
            "fixture loses the ordinary closing checkpoint reply",
        ))),
    );
    let transport = GatedEngramControlTransport::new([step]);
    state.install_control_test_transport(transport.clone());
    let closing_state = state.clone();
    let closing_session = session.to_owned();
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
    let request = gate.wait();
    assert_eq!(request.request["operation"], "turn_checkpoint");
    assert_eq!(request.request["next_intent"], "wait");
    assert_eq!(
        request.request["idempotency_key"],
        format!("termal-checkpoint:{session}:retained-grant:wait")
    );
    gate.release();
    assert!(matches!(
        closing.join().unwrap(),
        EngramCheckpointOutcome::Failed(_)
    ));
    assert_eq!(transport.requests().len(), 1);
    let inner = state.inner.lock().unwrap();
    inner.engram_session_obligations.validate().unwrap();
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    assert!(record.engram.checkpoint_operation.is_some());
    assert!(engram_session_obligation_locked(&inner, session).is_some());
    json!({
        "ledger": inner.engram_session_obligations,
        "mirror": record.engram.checkpoint_operation,
    })
}

fn assert_settings_race_refused(
    state: &AppState,
    project: &str,
    session: &str,
    original_settings: &EngramProjectSettings,
    expected: &Value,
    result: Result<StateResponse, ApiError>,
) {
    let error = result
        .err()
        .expect("a newly published duty must prevent settings mutation");
    assert_eq!(error.status, StatusCode::CONFLICT);
    let inner = state.inner.lock().unwrap();
    assert_eq!(
        inner.find_project(project).unwrap().engram.as_ref(),
        Some(original_settings)
    );
    assert!(!inner.engram_project_resets.contains(project));
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    assert!(!record.engram.project_reset_in_progress);
    assert!(!record.engram.checkpoint_in_progress);
    assert_eq!(
        json!({"ledger": inner.engram_session_obligations, "mirror": record.engram.checkpoint_operation}),
        *expected,
        "a refused mutation preserves the complete original request and mirror"
    );
    assert!(engram_session_obligation_locked(&inner, session).is_some());
}

fn assert_disable_preserves_checkpoint_debt(
    state: &AppState,
    project: &str,
    session: &str,
    expected: &Value,
    result: Result<StateResponse, ApiError>,
) {
    result.unwrap();
    let inner = state.inner.lock().unwrap();
    assert!(!inner.find_project(project).unwrap().engram.as_ref().unwrap().enabled);
    let record = &inner.sessions[inner.find_session_index(session).unwrap()];
    assert_eq!(json!({"ledger": inner.engram_session_obligations,
        "mirror": record.engram.checkpoint_operation}), *expected);
    assert!(!record.engram.checkpoint_in_progress);
    assert!(!record.engram.project_reset_in_progress);
    assert!(!inner.engram_project_resets.contains(project));
    assert!(require_no_engram_admission_obligation(&inner, session).is_err());
    drop(inner);
    let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
    assert!(!loaded.find_project(project).unwrap().engram.as_ref().unwrap().enabled);
    let record = &loaded.sessions[loaded.find_session_index(session).unwrap()];
    assert_eq!(json!({"ledger": loaded.engram_session_obligations,
        "mirror": record.engram.checkpoint_operation}), *expected);
    assert!(require_no_engram_admission_obligation(&loaded, session).is_err());
}

#[test]
fn project_containment_settings_disable_keeps_debt_published_after_precheck() {
    let (state, project, session, _root, receiver) = fixture();
    let original = settings(&state, &project, true);
    let expected = Arc::new(Mutex::new(None));
    let saved = expected.clone();
    let closing_state = state.clone();
    let closing_session = session.clone();
    let no_rpc = ScriptedEngramControlTransport::new([]);
    let after_close = no_rpc.clone();
    TEST_ENGRAM_SETTINGS_MUTATION_BOUNDARY.with(|slot| {
        *slot.borrow_mut() = Some((
            "after_precheck",
            Box::new(move || {
                *saved.lock().unwrap() = Some(publish_lost_closing_checkpoint(
                    &closing_state,
                    &closing_session,
                ));
                closing_state.install_control_test_transport(after_close);
            }),
        ));
    });
    let mut disabled = original.clone();
    disabled.enabled = false;
    let result = state.update_project_engram_settings(&project, disabled);
    assert_disable_preserves_checkpoint_debt(
        &state,
        &project,
        &session,
        expected.lock().unwrap().as_ref().unwrap(),
        result,
    );
    assert!(receiver.try_recv().is_err());
    assert!(no_rpc.requests().is_empty());
}

#[test]
fn project_containment_settings_nonreset_commit_rechecks_a_closing_checkpoint() {
    let (state, project, session, root, receiver) = fixture();
    let _readiness = stage_test_engram_readiness_without_launch(&[&root]);
    let original = settings(&state, &project, true);
    let expected = Arc::new(Mutex::new(None));
    let saved = expected.clone();
    let closing_state = state.clone();
    let closing_session = session.clone();
    TEST_ENGRAM_SETTINGS_MUTATION_BOUNDARY.with(|slot| {
        *slot.borrow_mut() = Some((
            "before_commit",
            Box::new(move || {
                let retained = publish_lost_closing_checkpoint(
                    &closing_state,
                    &closing_session,
                );
                // Inactive report debt alone permits settings changes. A
                // distinct unresolved grant must still stop this final commit.
                let mut inner = closing_state.inner.lock().unwrap();
                inner.session_mut(&closing_session).unwrap().engram.uncertain_grant_id =
                    Some("independent-grant".into());
                closing_state.commit_locked(&mut inner).unwrap();
                *saved.lock().unwrap() = Some(retained);
            }),
        ));
    });
    let mut changed = original.clone();
    changed.deadline_ms = Some(changed.deadline_ms.unwrap_or(1_000) + 1);
    let result = state.update_project_engram_settings(&project, changed);
    assert_settings_race_refused(
        &state,
        &project,
        &session,
        &original,
        expected.lock().unwrap().as_ref().unwrap(),
        result,
    );
    assert_eq!(state.inner.lock().unwrap().sessions.iter().find(|record|
        record.session.id == session).unwrap().engram.uncertain_grant_id.as_deref(),
        Some("independent-grant"));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_settings_reenable_preserves_report_debt_and_blocks_admission() {
    for reload in [false, true] {
        let (state, project, session, root, receiver) = fixture();
        let _readiness = stage_test_engram_readiness_without_launch(&[&root]);
        let expected = publish_lost_closing_checkpoint(&state, &session);
        let no_rpc = ScriptedEngramControlTransport::new([]);
        state.install_control_test_transport(no_rpc.clone());
        let mut disabled = settings(&state, &project, true);
        disabled.enabled = false;
        state.update_project_engram_settings(&project, disabled).unwrap();
        if reload {
            state.shutdown_persist_blocking();
            *state.inner.lock().unwrap() = load_state(state.persistence_path.as_path()).unwrap().unwrap();
            select_scripted_engram_budget_clock_before_enable(&state);
        }
        let mut enabled = settings(&state, &project, true);
        enabled.enabled = true;
        state.update_project_engram_settings(&project, enabled).unwrap();
        {
            let inner = state.inner.lock().unwrap();
            assert!(inner.find_project(&project).unwrap().engram.as_ref().unwrap().is_control_enabled());
            let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
            assert_eq!(json!({"ledger": inner.engram_session_obligations,
                "mirror": record.engram.checkpoint_operation}), expected);
            assert!(require_no_engram_admission_obligation(&inner, &session).is_err());
            assert!(AppState::engram_binding_target_for_session_shape_locked(&inner, &session, true)
                .unwrap().is_none());
            assert!(!engram_unmediated_checkpoint_dispatch_allowed(&inner, &session, None));
        }
        assert!(state.resume_local_session_queue(&session, None).is_err());
        assert!(!matches!(state.dispatch_turn(&session, prompt()),
            Ok(DispatchTurnResult::Dispatched(_) | DispatchTurnResult::DispatchedAfterQueue(_))));
        state.shutdown_persist_blocking();
        let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
        assert_eq!(serde_json::to_value(&loaded.engram_session_obligations).unwrap(), expected["ledger"]);
        assert!(require_no_engram_admission_obligation(&loaded, &session).is_err());
        assert!(no_rpc.requests().is_empty(), "re-enable supplies no Bind, Evaluate or Begin authority");
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn project_containment_settings_ownerless_report_debt_does_not_block_deadline_edit() {
    for reload in [false, true] {
        let (state, project, session, root, receiver) = fixture();
        let _readiness = stage_test_engram_readiness_without_launch(&[&root]);
        let expected = publish_lost_closing_checkpoint(&state, &session);
        let no_rpc = ScriptedEngramControlTransport::new([]);
        state.install_control_test_transport(no_rpc.clone());
        state.kill_session(&session).unwrap();
        if reload {
            state.shutdown_persist_blocking();
            *state.inner.lock().unwrap() = load_state(state.persistence_path.as_path()).unwrap().unwrap();
            select_scripted_engram_budget_clock_before_enable(&state);
        }
        let mut changed = settings(&state, &project, true);
        changed.deadline_ms = Some(changed.deadline_ms.unwrap_or(1_000) + 1);
        state.update_project_engram_settings(&project, changed.clone()).unwrap();
        state.shutdown_persist_blocking();
        let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
        assert!(loaded.find_session_index(&session).is_none());
        assert_eq!(loaded.find_project(&project).unwrap().engram.as_ref().unwrap().deadline_ms,
            changed.deadline_ms);
        assert_eq!(serde_json::to_value(&loaded.engram_session_obligations).unwrap(), expected["ledger"]);
        assert!(require_no_engram_admission_obligation(&loaded, &session).is_err());
        assert!(no_rpc.requests().is_empty());
        assert!(receiver.try_recv().is_err());
    }
}

#[test]
fn project_containment_settings_live_and_independent_duties_still_block_changes() {
    for blocker in ["live", "uncertain", "replacement"] {
        for reenable in [false, true] {
            let (state, project, session, root, receiver) = fixture();
            let _readiness = stage_test_engram_readiness_without_launch(&[&root]);
            let expected = publish_lost_closing_checkpoint(&state, &session);
            let no_rpc = ScriptedEngramControlTransport::new([]);
            state.install_control_test_transport(no_rpc.clone());
            if reenable {
                let mut disabled = settings(&state, &project, true);
                disabled.enabled = false;
                state.update_project_engram_settings(&project, disabled).unwrap();
            }
            {
                let mut inner = state.inner.lock().unwrap();
                let record = inner.session_mut(&session).unwrap();
                match blocker {
                    "live" => record.session.status = SessionStatus::Active,
                    "uncertain" => record.engram.uncertain_grant_id = Some("independent-grant".into()),
                    "replacement" => record.engram.active_grant_id = Some("replacement-grant".into()),
                    _ => unreachable!(),
                }
                state.commit_locked(&mut inner).unwrap();
            }
            let original = settings(&state, &project, true);
            let mut changed = original.clone();
            if reenable { changed.enabled = true; }
            else { changed.deadline_ms = Some(changed.deadline_ms.unwrap_or(1_000) + 1); }
            assert_settings_race_refused(&state, &project, &session, &original, &expected,
                state.update_project_engram_settings(&project, changed));
            assert!(no_rpc.requests().is_empty());
            assert!(receiver.try_recv().is_err());
        }
    }
}

#[test]
fn project_containment_settings_reset_final_commit_keeps_a_late_published_duty() {
    let (state, project, session, _root, receiver) = fixture();
    let original = settings(&state, &project, true);
    let expected = publish_lost_closing_checkpoint(&state, &session);
    let no_rpc = ScriptedEngramControlTransport::new([]);
    state.install_control_test_transport(no_rpc.clone());
    let (ledger, mirror) = {
        let mut inner = state.inner.lock().unwrap();
        let ledger = std::mem::replace(
            &mut inner.engram_session_obligations,
            EngramSessionObligationLedger::empty(),
        );
        let index = inner.find_session_index(&session).unwrap();
        let mirror = inner.sessions[index].engram.checkpoint_operation.take();
        inner.sessions[index].engram.active_grant_id = None;
        state.commit_locked(&mut inner).unwrap();
        (ledger, mirror)
    };
    // Model a valid independent metadata publication at the final boundary.
    // The reset already owns a newer generation; this does not lend the old
    // operation current-owner permission, but must preserve its custody.
    let publishing_state = state.clone();
    let publishing_session = session.clone();
    TEST_ENGRAM_SETTINGS_MUTATION_BOUNDARY.with(|slot| {
        *slot.borrow_mut() = Some((
            "before_commit",
            Box::new(move || {
                let mut inner = publishing_state.inner.lock().unwrap();
                inner.engram_session_obligations = ledger;
                let index = inner.find_session_index(&publishing_session).unwrap();
                inner.sessions[index].engram.checkpoint_operation = mirror;
                inner.sessions[index].engram.active_grant_id = Some("retained-grant".into());
                inner.engram_session_obligations.validate().unwrap();
                publishing_state.commit_locked(&mut inner).unwrap();
            }),
        ));
    });
    let mut disabled = original.clone();
    disabled.enabled = false;
    let result = state.update_project_engram_settings(&project, disabled);
    assert_disable_preserves_checkpoint_debt(&state, &project, &session, &expected, result);
    assert!(no_rpc.requests().is_empty());
    assert!(receiver.try_recv().is_err());
}

#[test]
fn project_containment_off_keeps_inactive_report_after_runtime_or_settings_drift() {
    for drift in ["runtime_exit", "reload", "deadline", "reenable"] {
        for independent in [false, true] {
            let (state, project, session, root, receiver) = fixture();
            let _readiness = stage_test_engram_readiness_without_launch(&[&root]);
            let (runtime, _runtime_input, process) = test_shared_codex_runtime("closing-report-runtime");
            {
                let mut inner = state.inner.lock().unwrap();
                inner.session_mut(&session).unwrap().runtime = SessionRuntime::Codex(CodexRuntimeHandle {
                    runtime_id: runtime.runtime_id.clone(),
                    input_tx: runtime.input_tx.clone(),
                    process,
                    shared_session: None,
                });
                state.commit_locked(&mut inner).unwrap();
            }
            let expected = publish_lost_closing_checkpoint(&state, &session);
            let no_rpc = ScriptedEngramControlTransport::new([]);
            state.install_control_test_transport(no_rpc.clone());
            match drift {
                "runtime_exit" => {
                    let mut inner = state.inner.lock().unwrap();
                    inner.session_mut(&session).unwrap().runtime = SessionRuntime::None;
                    state.commit_locked(&mut inner).unwrap();
                }
                "reload" => {
                    state.shutdown_persist_blocking();
                    *state.inner.lock().unwrap() = load_state(state.persistence_path.as_path()).unwrap().unwrap();
                    select_scripted_engram_budget_clock_before_enable(&state);
                }
                "deadline" => {
                    let mut changed = settings(&state, &project, true);
                    changed.deadline_ms = Some(changed.deadline_ms.unwrap_or(1_000) + 1);
                    state.update_project_engram_settings(&project, changed).unwrap();
                }
                "reenable" => {
                    let mut disabled = settings(&state, &project, true);
                    disabled.enabled = false;
                    state.update_project_engram_settings(&project, disabled).unwrap();
                    let mut enabled = settings(&state, &project, true);
                    enabled.enabled = true;
                    state.update_project_engram_settings(&project, enabled).unwrap();
                }
                _ => unreachable!(),
            }
            let original_settings = settings(&state, &project, true);
            {
                let mut inner = state.inner.lock().unwrap();
                let operation = &inner.engram_session_obligations.checkpoint_operations[0];
                assert_ne!(operation.attempts.last().unwrap().runtime_episode, "None");
                if matches!(drift, "runtime_exit" | "reload") {
                    assert_ne!(operation.attempts.last().unwrap().runtime_episode,
                        format!("{:?}", inner.sessions[inner.find_session_index(&session).unwrap()]
                            .runtime.runtime_token()));
                }
                assert!(EngramCheckpointRecoveryView::for_off_adoption(&inner, &session).is_none());
                if independent {
                    inner.session_mut(&session).unwrap().engram.uncertain_grant_id =
                        Some("independent-grant".into());
                    state.commit_locked(&mut inner).unwrap();
                }
            }
            let result = state.update_project_engram_settings(&project, settings(&state, &project, false));
            if independent {
                assert_settings_race_refused(&state, &project, &session,
                    &original_settings, &expected, result);
            } else {
                result.expect("stale inactive report adoption cannot veto project OFF");
                state.shutdown_persist_blocking();
                let loaded = load_state(state.persistence_path.as_path()).unwrap().unwrap();
                for inner in [&*state.inner.lock().unwrap(), &loaded] {
                    assert!(!inner.find_project(&project).unwrap().engram.as_ref().unwrap().turn_gated_control);
                    let record = &inner.sessions[inner.find_session_index(&session).unwrap()];
                    assert_eq!(json!({"ledger": inner.engram_session_obligations,
                        "mirror": record.engram.checkpoint_operation}), expected);
                    assert!(require_no_engram_admission_obligation(inner, &session).is_err());
                }
            }
            assert!(no_rpc.requests().is_empty());
            assert!(receiver.try_recv().is_err());
        }
    }
}

#[test]
fn project_containment_settings_mutations_without_a_duty_still_succeed() {
    for reset in [false, true] {
        let (state, project, session, root, receiver) = fixture();
        let _readiness = stage_test_engram_readiness_without_launch(&[&root]);
        let mut changed = settings(&state, &project, true);
        if reset {
            changed.enabled = false;
            let mut inner = state.inner.lock().unwrap();
            let index = inner.find_session_index(&session).unwrap();
            inner.sessions[index].engram.active_grant_id = None;
            state.commit_locked(&mut inner).unwrap();
        } else {
            changed.deadline_ms = Some(changed.deadline_ms.unwrap_or(1_000) + 1);
        }
        state
            .update_project_engram_settings(&project, changed.clone())
            .unwrap();
        let inner = state.inner.lock().unwrap();
        let applied = inner
            .find_project(&project)
            .unwrap()
            .engram
            .as_ref()
            .unwrap();
        assert_eq!(applied.enabled, changed.enabled);
        assert_eq!(applied.deadline_ms, changed.deadline_ms);
        assert!(!engram_project_has_unresolved_obligations(&inner, &project));
        assert!(!inner.engram_project_resets.contains(&project));
        assert!(receiver.try_recv().is_err());
    }
}
