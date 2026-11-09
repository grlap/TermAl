//! Ordinary public settings cycles and durable passive-history identity.
use super::*;

#[test]
fn ordinary_settings_cycles_and_reload_preserve_history_without_creating_new_duties() {
    let (turn, history) = ordinary_with_history("ordinary-observation-cycles");
    for cycle in 0..2 {
        for observing in [true, false] {
            let mut settings = observation_settings(&turn);
            settings.execution_observability = observing;
            turn.state
                .update_project_engram_settings(&turn.project, settings)
                .unwrap();
        }
        let text = format!("ordinary cycle {cycle}");
        let dispatch = turn.prepare(&text);
        let token = dispatch.runtime_token().clone();
        let generation = dispatch.active_turn_generation();
        assert!(matches!(
            deliver_turn_dispatch(&turn.state, dispatch),
            TurnDispatchDeliveryOutcome::Delivered
        ));
        assert!(turn.prompt().prompt.contains(&text));
        turn.state
            .finish_turn_ok_if_runtime_and_generation_match(&turn.session, &token, generation)
            .unwrap();
        turn.assert_no_prompt();
        assert_history(&turn, &history);
        reload_fixture(&turn);
        assert_history(&turn, &history);
    }
    assert_eq!(
        history["entries"].as_array().unwrap().len(),
        1,
        "ordinary cycles never mint replacement historical obligations"
    );
}

#[test]
fn passive_history_rejects_duplicate_wire_identity_without_excluding_ordinary_dispatch() {
    let (turn, history) = ordinary_with_history("ordinary-historical-collision");
    let mut duplicate = history.clone();
    let mut copied = duplicate["entries"][0].clone();
    copied["id"] = json!("different-wrapper-id");
    copied["ownerGeneration"] = json!(99);
    duplicate["entries"].as_array_mut().unwrap().push(copied);
    assert!(
        serde_json::from_value::<EngramSessionObligationLedger>(duplicate).is_err(),
        "a new wrapper cannot disguise the same historical request"
    );
    let dispatch = turn.prepare("current ordinary work remains independent");
    assert!(matches!(
        deliver_turn_dispatch(&turn.state, dispatch),
        TurnDispatchDeliveryOutcome::Delivered
    ));
    assert!(
        turn.prompt()
            .prompt
            .contains("current ordinary work remains independent")
    );
    turn.assert_no_prompt();
    assert_history(&turn, &history);
}

#[test]
fn ordinary_settings_sql_failure_rolls_back_the_setting_and_preserves_historical_bytes() {
    let (turn, history) = ordinary_with_history("ordinary-settings-sql-refusal");
    let before = observation_settings(&turn);
    turn.state.shutdown_persist_blocking();
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_ref()).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER refuse_containment_slice_settings BEFORE UPDATE ON state_metadata_units
        BEGIN SELECT RAISE(ABORT, 'injected ordinary settings failure'); END;",
        )
        .unwrap();
    let mut requested = before.clone();
    requested.execution_observability = !requested.execution_observability;
    let error = turn
        .state
        .update_project_engram_settings(&turn.project, requested)
        .err()
        .expect("real SQL failure must not become a successful settings update");
    assert!(error.message.contains("persist"));
    assert_eq!(observation_settings(&turn), before);
    assert!(
        !turn
            .state
            .inner
            .lock()
            .unwrap()
            .engram_project_resets
            .contains(&turn.project)
    );
    turn.assert_no_prompt();
    assert_history(&turn, &history);
    connection
        .execute_batch("DROP TRIGGER refuse_containment_slice_settings")
        .unwrap();
}
