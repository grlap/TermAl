//! Settings races use current connection and Stop owners; history stays data.
use super::*;

fn publish_original_at_settings_commit(turn: &OrdinaryRoot) -> Value {
    let expected = install_passive_original(turn);
    let ledger: EngramSessionObligationLedger = serde_json::from_value(expected.clone()).unwrap();
    {
        let mut inner = turn.state.inner.lock().unwrap();
        inner.engram_session_obligations = EngramSessionObligationLedger::empty();
        turn.state.commit_locked(&mut inner).unwrap();
    }
    let state = turn.state.clone();
    TEST_ENGRAM_SETTINGS_MUTATION_BOUNDARY.with(|slot| {
        *slot.borrow_mut() = Some(("before_commit", Box::new(move || {
            let mut inner = state.inner.lock().unwrap();
            inner.engram_session_obligations = ledger;
            turn_settings_publish_commit(&state, &mut inner);
        })));
    });
    expected
}

fn turn_settings_publish_commit(state: &AppState, inner: &mut StateInner) {
    inner.engram_session_obligations.validate().unwrap();
    state.commit_locked(inner).unwrap();
}

#[test]
fn ordinary_disable_preserves_history_published_at_the_final_commit() {
    let turn = OrdinaryRoot::new("ordinary-late-disable-data");
    let expected = publish_original_at_settings_commit(&turn);
    let mut requested = ordinary_settings(&turn);
    requested.enabled = false;
    turn.state.update_project_engram_settings(&turn.project, requested).unwrap();
    assert!(!ordinary_settings(&turn).enabled);
    assert!(!turn.state.inner.lock().unwrap().engram_project_resets.contains(&turn.project));
    assert_passive_original(&turn, &expected);
    turn.assert_no_prompt();
}

#[test]
fn ordinary_observation_edit_preserves_a_stop_claimed_during_validation() {
    let turn = OrdinaryRoot::new("ordinary-late-stop-owner");
    let expected = install_passive_original(&turn);
    turn.edit(|record| { record.runtime = SessionRuntime::None; record.session.status = SessionStatus::Active; });
    let selected = Arc::new(Mutex::new(None));
    let saved = selected.clone();
    let state = turn.state.clone();
    let session = turn.session.clone();
    TEST_ENGRAM_SETTINGS_MUTATION_BOUNDARY.with(|slot| {
        *slot.borrow_mut() = Some(("before_commit", Box::new(move || {
            let (_, claim) = state.begin_requested_stop_session(&session, &StopSessionOptions::default()).unwrap();
            *saved.lock().unwrap() = claim;
        })));
    });
    let mut requested = ordinary_settings(&turn);
    requested.execution_observability = true;
    turn.state.update_project_engram_settings(&turn.project, requested).unwrap();
    let claim = selected.lock().unwrap().take().expect("actual public Stop was claimed");
    turn.record(|record| {
        assert!(claim.owns_stop(record));
        assert_eq!(record.session.status, SessionStatus::Stopping);
        assert!(record.engram.operator_paused && record.orchestrator_auto_dispatch_blocked);
    });
    assert_passive_original(&turn, &expected);
    turn.state.stop_local_session_with_options(&turn.session, StopSessionOptions::default(), Some(claim)).unwrap();
    turn.assert_no_prompt();
}

#[test]
fn ordinary_observation_toggles_preserve_history_and_do_not_gate_delivery() {
    for reload in [false, true] {
        let turn = OrdinaryRoot::new("ordinary-toggle-delivery");
        let expected = install_passive_original(&turn);
        if reload {
            turn.state.shutdown_persist_blocking();
            *turn.state.inner.lock().unwrap() = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
        }
        for observing in [true, false] {
            let mut requested = ordinary_settings(&turn);
            requested.execution_observability = observing;
            turn.state.update_project_engram_settings(&turn.project, requested).unwrap();
            let raw = if observing { "ordinary observed turn" } else { "ordinary unobserved turn" };
            let dispatch = turn.prepare(raw);
            let token = dispatch.runtime_token().clone();
            let generation = dispatch.active_turn_generation();
            assert!(matches!(deliver_turn_dispatch(&turn.state, dispatch), TurnDispatchDeliveryOutcome::Delivered));
            let command = turn.prompt();
            assert!(command.prompt == raw || command.prompt.ends_with(&format!("\n\n{raw}")));
            assert_eq!(command.prompt.matches(raw).count(), 1);
            turn.state.finish_turn_ok_if_runtime_and_generation_match(&turn.session, &token, generation).unwrap();
            assert_passive_original(&turn, &expected);
            turn.assert_no_prompt();
        }
    }
}

#[test]
fn ordinary_ownerless_history_does_not_block_connection_disable() {
    let turn = OrdinaryRoot::new("ordinary-ownerless-data");
    let expected = install_passive_original(&turn);
    turn.state.kill_session(&turn.session).unwrap();
    let mut requested = ordinary_settings(&turn);
    requested.enabled = false;
    turn.state.update_project_engram_settings(&turn.project, requested).unwrap();
    turn.state.shutdown_persist_blocking();
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    assert!(loaded.find_session_index(&turn.session).is_none());
    assert!(!loaded.find_project(&turn.project).unwrap().engram.as_ref().unwrap().enabled);
    assert_passive_original(&turn, &expected);
    turn.assert_no_prompt();
}

#[test]
fn ordinary_settings_reject_an_existing_connection_owner_without_stealing_it() {
    let turn = OrdinaryRoot::new("ordinary-existing-connection-owner");
    let expected = install_passive_original(&turn);
    let original = ordinary_settings(&turn);
    let generation = turn.state.inner.lock().unwrap().claim_engram_project_reset(&turn.project).unwrap();
    for disable in [false, true] {
        let mut requested = original.clone();
        if disable { requested.enabled = false; }
        else { requested.execution_observability = !original.execution_observability; }
        assert_eq!(turn.state.update_project_engram_settings(&turn.project, requested).err().expect("the current owner must refuse this operation").status,
            StatusCode::CONFLICT);
        assert_eq!(ordinary_settings(&turn), original);
        assert!(turn.state.inner.lock().unwrap().engram_project_resets.is_owned_by(&turn.project, generation));
    }
    assert!(turn.state.inner.lock().unwrap().engram_project_resets.release(&turn.project, generation));
    assert_passive_original(&turn, &expected);
    turn.assert_no_prompt();
}

#[test]
fn ordinary_settings_final_commit_rechecks_a_new_connection_owner() {
    let turn = OrdinaryRoot::new("ordinary-late-connection-owner");
    let expected = install_passive_original(&turn);
    let original = ordinary_settings(&turn);
    let owner = Arc::new(Mutex::new(None));
    let selected = owner.clone();
    let state = turn.state.clone();
    let project = turn.project.clone();
    TEST_ENGRAM_SETTINGS_MUTATION_BOUNDARY.with(|slot| {
        *slot.borrow_mut() = Some(("before_commit", Box::new(move || {
            *selected.lock().unwrap() = Some(state.inner.lock().unwrap().claim_engram_project_reset(&project).unwrap());
        })));
    });
    let mut requested = original.clone();
    requested.execution_observability = true;
    assert_eq!(turn.state.update_project_engram_settings(&turn.project, requested).err().expect("the current owner must refuse this operation").status,
        StatusCode::CONFLICT);
    assert_eq!(ordinary_settings(&turn), original);
    let generation = owner.lock().unwrap().unwrap();
    assert!(turn.state.inner.lock().unwrap().engram_project_resets.is_owned_by(&turn.project, generation));
    assert!(turn.state.inner.lock().unwrap().engram_project_resets.release(&turn.project, generation));
    assert_passive_original(&turn, &expected);
    turn.assert_no_prompt();
}

#[test]
fn ordinary_runtime_and_settings_drift_never_replay_private_history() {
    for drift in ["runtime", "reload", "disable"] {
        let turn = OrdinaryRoot::new("ordinary-original-drift");
        let expected = install_passive_original(&turn);
        match drift {
            "runtime" => turn.edit(|record| {
                record.runtime = SessionRuntime::None;
                record.active_turn_generation += 1;
            }),
            "reload" => {
                turn.state.shutdown_persist_blocking();
                *turn.state.inner.lock().unwrap() = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
            }
            _ => {
                let mut requested = ordinary_settings(&turn);
                requested.enabled = false;
                turn.state.update_project_engram_settings(&turn.project, requested).unwrap();
            }
        }
        let mut changed = ordinary_settings(&turn);
        changed.execution_observability = !changed.execution_observability;
        turn.state.update_project_engram_settings(&turn.project, changed).unwrap();
        assert_passive_original(&turn, &expected);
        turn.assert_no_prompt();
    }
}

#[test]
fn ordinary_connection_commit_failure_rolls_back_settings_and_reset_ownership() {
    let turn = OrdinaryRoot::new("ordinary-settings-rollback");
    let expected = install_passive_original(&turn);
    let original = ordinary_settings(&turn);
    let original_record = turn.saved();
    turn.state.shutdown_persist_blocking();
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    connection.execute_batch("CREATE TRIGGER refuse_boundary_settings BEFORE UPDATE ON state_metadata_units
        BEGIN SELECT RAISE(ABORT, 'injected connection commit failure'); END;").unwrap();
    let mut requested = original.clone();
    requested.enabled = false;
    let error = turn.state.update_project_engram_settings(&turn.project, requested).err().expect("the current owner must refuse this operation");
    assert!(error.message.contains("persist"));
    assert_eq!(ordinary_settings(&turn), original);
    assert_eq!(turn.saved(), original_record);
    assert!(!turn.state.inner.lock().unwrap().engram_project_resets.contains(&turn.project));
    assert_passive_original(&turn, &expected);
    turn.assert_no_prompt();
    connection.execute_batch("DROP TRIGGER refuse_boundary_settings;").unwrap();
}
