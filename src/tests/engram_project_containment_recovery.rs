//! Passive original metadata survives ordinary settings, delivery and restart.
//! No grant lookup, containment accounting or replay executor is reconstructed.
use super::*;
use super::super::uncertain_begin::OrdinaryRoot;

pub(super) fn install_passive_original(turn: &OrdinaryRoot) -> Value {
    let mut original = super::obligations::entry(&turn.root);
    original.project_id = turn.project.clone();
    original.local_session_id = turn.session.clone();
    original.connection.session_id = turn.session.clone();
    original.external_ref = format!("termal:session:{}", turn.session);
    let mut retirement = original.clone();
    retirement.id = "retirement-two".to_owned();
    retirement.kind = EngramSessionObligationKind::IssuedGrantRetirement;
    retirement.phase = EngramSessionObligationPhase::Intended;
    retirement.basis = EngramSessionObligationBasis::ProvenNeverSentBegin;
    retirement.prompt_id = Some("original-owner-head".to_owned());
    retirement.frozen_request = json!({"operation":"session_bind",
        "external_ref":retirement.external_ref, "title":"Historical original",
        "assurance":"turn_gated", "mediated_effects":[], "capability_map_revision":1,
        "idempotency_key":"distinct-retirement-key"});
    let ledger = EngramSessionObligationLedger {
        schema_version: ENGRAM_SESSION_OBLIGATION_LEDGER_VERSION,
        entries: vec![original, retirement], checkpoint_operations: Vec::new(),
    };
    ledger.validate().unwrap();
    let expected = serde_json::to_value(&ledger).unwrap();
    let mut inner = turn.state.inner.lock().unwrap();
    inner.engram_session_obligations = ledger;
    turn.state.commit_locked(&mut inner).unwrap();
    expected
}

pub(super) fn assert_passive_original(turn: &OrdinaryRoot, expected: &Value) {
    assert_eq!(serde_json::to_value(&turn.state.inner.lock().unwrap().engram_session_obligations).unwrap(), *expected);
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    assert_eq!(serde_json::to_value(&loaded.engram_session_obligations).unwrap(), *expected,
        "SQLite retains the original identity, frozen request and settlement evidence as data");
    turn.assert_no_control();
}

fn ordinary_settings(turn: &OrdinaryRoot) -> EngramProjectSettings {
    turn.state.inner.lock().unwrap().find_project(&turn.project).unwrap().engram.clone().unwrap()
}

#[test]
fn ordinary_settings_and_completion_preserve_private_original_metadata() {
    let turn = OrdinaryRoot::new("ordinary-passive-original");
    let expected = install_passive_original(&turn);
    for observing in [true, false] {
        let mut settings = ordinary_settings(&turn);
        settings.execution_observability = observing;
        let response = turn.state.update_project_engram_settings(&turn.project, settings).unwrap();
        let public = serde_json::to_string(&response).unwrap();
        for private in ["old-token", "old-grant", "frozen-checkpoint-key", "distinct-retirement-key", "historical.sqlite", "settlementEvidence"] {
            assert!(!public.contains(private), "host-private original escaped into the current public response: {private}");
        }
        assert!(!public.contains("engramControlRecovery"));
        assert_passive_original(&turn, &expected);
    }
    let dispatch = turn.prepare("Distinct ordinary input beside passive original metadata.");
    let token = dispatch.runtime_token().clone();
    let generation = dispatch.active_turn_generation();
    assert!(matches!(deliver_turn_dispatch(&turn.state, dispatch), TurnDispatchDeliveryOutcome::Delivered));
    assert!(turn.prompt().prompt.contains("Distinct ordinary input beside passive original metadata."));
    turn.state.finish_turn_ok_if_runtime_and_generation_match(&turn.session, &token, generation).unwrap();
    turn.assert_no_prompt();
    assert_passive_original(&turn, &expected);
}

#[test]
fn a_failed_ordinary_settings_commit_keeps_the_setting_and_passive_original() {
    let turn = OrdinaryRoot::new("ordinary-settings-persist-refusal");
    let expected = install_passive_original(&turn);
    let before = ordinary_settings(&turn);
    turn.state.shutdown_persist_blocking();
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    connection.execute_batch(
        "CREATE TRIGGER refuse_ordinary_settings BEFORE UPDATE ON state_metadata_units
         BEGIN SELECT RAISE(ABORT, 'injected ordinary settings failure'); END;",
    ).unwrap();
    drop(connection);
    let mut requested = before.clone();
    requested.execution_observability = !before.execution_observability;
    let error = turn.state.update_project_engram_settings(&turn.project, requested)
        .err().expect("the real settings metadata commit must fail");
    assert!(error.message.contains("persist"), "{error:?}");
    assert_eq!(ordinary_settings(&turn), before);
    assert!(!turn.state.inner.lock().unwrap().engram_project_resets.contains(&turn.project));
    turn.assert_no_prompt();
    assert_passive_original(&turn, &expected);
    rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap()
        .execute_batch("DROP TRIGGER refuse_ordinary_settings;").unwrap();
}

#[test]
fn ordinary_cancel_and_local_recreation_do_not_settle_passive_original_metadata() {
    let turn = OrdinaryRoot::new("ordinary-original-cancel-reload");
    let expected = install_passive_original(&turn);
    let head = turn.enqueue("Possibly delivered original content: żółw 🐢", QueuedPromptSource::User);
    turn.edit(|record| {
        record.queued_prompts[0].engram_ordinary_handoff = true;
        record.queued_prompts[0].promotion_disposition_known = false;
        record.set_auto_dispatch_blocked(true);
    });
    let unknown = turn.record(|record| serde_json::to_value(&record.queued_prompts[0]).unwrap());
    turn.state.resume_session_queue(&turn.session).unwrap();
    turn.assert_no_prompt();
    assert_eq!(turn.record(|record| serde_json::to_value(&record.queued_prompts[0]).unwrap()), unknown,
        "an ordinary Resume is no permission to replay an ambiguous provider handoff");
    assert_passive_original(&turn, &expected);
    turn.state.cancel_queued_prompt(&turn.session, &head).unwrap();
    turn.edit(|record| record.engram = EngramSessionState::default());
    assert_passive_original(&turn, &expected);
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    *turn.state.inner.lock().unwrap() = loaded;
    turn.state.resume_session_queue(&turn.session).unwrap();
    turn.assert_no_prompt();
    assert_passive_original(&turn, &expected);
}

#[test]
fn an_ordinary_settings_edit_preserves_the_existing_stop_owner() {
    let turn = OrdinaryRoot::new("ordinary-settings-stop-owner");
    let expected = install_passive_original(&turn);
    turn.edit(|record| {
        record.runtime = SessionRuntime::None;
        record.session.status = SessionStatus::Active;
    });
    let options = StopSessionOptions::default();
    let (_, claim) = turn.state.begin_requested_stop_session(&turn.session, &options).unwrap();
    let claim = claim.expect("the actual Stop request owns the missing-runtime cleanup");
    let generation = turn.record(|record| {
        assert!(record.runtime_stop_in_progress);
        record.runtime_stop_generation
    });
    let mut settings = ordinary_settings(&turn);
    settings.execution_observability = true;
    turn.state.update_project_engram_settings(&turn.project, settings).unwrap();
    turn.record(|record| {
        assert!(record.runtime_stop_in_progress);
        assert_eq!(record.runtime_stop_generation, generation);
        assert_eq!(record.session.status, SessionStatus::Stopping);
    });
    turn.assert_no_prompt();
    assert_passive_original(&turn, &expected);
    turn.state.stop_local_session_with_options(&turn.session, options, Some(claim)).unwrap();
    turn.record(|record| {
        assert!(!record.runtime_stop_in_progress);
        assert!(record.engram.operator_paused && record.orchestrator_auto_dispatch_blocked);
    });
    assert_passive_original(&turn, &expected);
}
