//! Historical diagnostic visibility and the real SQLite projection.
//! The removed gate publisher is not restarted by these stored-data controls.
use super::*;
use crate::tests::engram_host_adapter::uncertain_begin::OrdinaryRoot;

fn pending_historical_card(turn: &OrdinaryRoot) -> String {
    let mut card: EngramControlCard = serde_json::from_value(json!({
        "schemaVersion":1,"stage":"checkpoint","assurance":"historical",
        "decision":"refuse","dispatch":"withheld","latencyMs":{"total":0},"failMode":"enforced"
    })).unwrap();
    card.causal_failure = Some(EngramCausalFailure::local(
        &EngramTransportError::local_state("historical local write was not acknowledged"),
        "historical_checkpoint", "historical writer"));
    let mut inner = turn.state.inner.lock().unwrap();
    let id = inner.next_message_id();
    push_message_on_record(inner.session_mut(&turn.session).unwrap(), Message::EngramControl {
        id: id.clone(), timestamp: stamp_now(), author: Author::Assistant, card,
    });
    id
}

fn public_causes(turn: &OrdinaryRoot) -> Vec<Value> {
    turn.record(|record| serde_json::to_value(AppState::wire_session_from_record(
        &turn.state.server_instance_id, record)).unwrap()["messages"].as_array().unwrap()
        .iter().filter_map(|message| message.get("causalFailure").cloned()).collect())
}

#[test]
fn historical_causal_details_stay_hidden_until_read_from_actual_committed_rows() {
    let turn = OrdinaryRoot::new("historical-causal-commit");
    turn.state.shutdown_persist_blocking();
    let id = pending_historical_card(&turn);
    assert!(public_causes(&turn).is_empty());
    let tail = turn.record(|record| serde_json::to_value(AppState::wire_session_tail_from_record(
        &turn.state.server_instance_id, record, 100, true)).unwrap());
    assert!(tail["messages"].as_array().unwrap().iter().all(|message| message.get("causalFailure").is_none()));
    let captured = turn.record(|record| PersistedSessionRecord::from_record(record));
    assert!(serde_json::to_value(&captured).unwrap()["session"]["messages"].as_array().unwrap()
        .iter().any(|message| message["id"] == id && message["causalFailure"].is_object()));
    let delta = collect_persist_delta_from_shared_state(&turn.state.inner, 0);
    persist_delta_with_fences(&mut SqlitePersistConnectionCache::new(),
        turn.state.persistence_path.as_path(), &delta, &mut PersistFenceBatch::default()).unwrap();
    let stored = load_persisted_message_range(&turn.state.persistence_path, &turn.session, 0, 100).unwrap();
    let cause = stored.iter().find_map(|(_, message)| {
        let wire = serde_json::to_value(message).unwrap();
        (wire["id"] == id).then(|| wire["causalFailure"].clone())
    }).unwrap();
    assert_eq!(cause["operation"], "historical_checkpoint");
    assert!(cause.get("publicationPending").is_none());
    assert!(public_causes(&turn).is_empty(), "persistence does not recreate a retired publication callback");
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn failed_historical_causal_write_keeps_old_rows_and_does_not_hide_later_text() {
    let turn = OrdinaryRoot::new("historical-causal-write-failure");
    turn.state.shutdown_persist_blocking();
    let id = pending_historical_card(&turn);
    let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
    connection.execute_batch(&format!(
        "CREATE TRIGGER reject_historical_cause BEFORE INSERT ON messages
         WHEN NEW.message_id='{}' BEGIN SELECT RAISE(ABORT,'historical-cause-write-failure'); END;",
        id.replace('\'',"''"))).unwrap();
    let delta = collect_persist_delta_from_shared_state(&turn.state.inner, 0);
    let result = persist_delta_with_fences(&mut SqlitePersistConnectionCache::new(),
        turn.state.persistence_path.as_path(), &delta, &mut PersistFenceBatch::default());
    connection.execute_batch("DROP TRIGGER reject_historical_cause;").unwrap();
    assert!(result.err().is_some_and(|error| format!("{error:#}").contains("historical-cause-write-failure")));
    let count: usize = connection.query_row(
        "SELECT COUNT(*) FROM messages WHERE session_id=?1 AND message_id=?2",
        rusqlite::params![turn.session,id], |row| row.get(0)).unwrap();
    assert_eq!(count, 0);
    assert!(public_causes(&turn).is_empty());
    turn.edit(|record| {
        push_message_on_record(record, Message::Text {
            id: "later-visible-text".into(), timestamp: stamp_now(), author: Author::Assistant,
            text: "Later message remains visible".into(), attachments: Vec::new(),
            expanded_text: None, source: None,
        });
    });
    let wire = turn.record(|record| serde_json::to_value(AppState::wire_session_from_record(
        &turn.state.server_instance_id, record)).unwrap());
    assert!(wire["messages"].as_array().unwrap().iter().any(|message|
        message["text"] == "Later message remains visible"));
    assert!(public_causes(&turn).is_empty());
    turn.assert_no_prompt();
    turn.assert_no_control();
}

#[test]
fn ordinary_completion_does_not_wait_for_or_execute_historical_causal_publication() {
    let turn = OrdinaryRoot::new("ordinary-beside-historical-cause");
    pending_historical_card(&turn);
    let dispatch = turn.prepare("Independent ordinary prompt.");
    let token = dispatch.runtime_token().clone();
    deliver_turn_dispatch(&turn.state, dispatch).into_public_result().unwrap();
    assert!(turn.prompt().prompt.contains("Independent ordinary prompt."));
    turn.state.finish_turn_ok_if_runtime_matches(&turn.session, &token).unwrap();
    assert!(public_causes(&turn).is_empty());
    turn.assert_no_prompt();
    turn.assert_no_control();
}
