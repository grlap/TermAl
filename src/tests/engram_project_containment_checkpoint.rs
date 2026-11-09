//! Historical checkpoint bytes remain private data, never executable work.
use super::super::turn_checks::checkpoint_result_recovery::{
    accepted_history, append_historical_result, install_historical_checkpoint, passive_checkpoint,
};
use super::super::uncertain_begin::OrdinaryRoot;
use super::*;

#[path = "engram_project_containment_checkpoint_flight.rs"]
mod flight;
#[path = "engram_project_containment_checkpoint_intake.rs"]
mod intake;
#[path = "engram_project_containment_checkpoint_receipts.rs"]
mod receipts;
#[path = "engram_project_containment_checkpoint_review.rs"]
mod review;

fn passive_checkpoint_turn(label: &str) -> (OrdinaryRoot, EngramCheckpointOperation) {
    let turn = OrdinaryRoot::new(label);
    let mut operation = passive_checkpoint(&turn.root, &turn.session);
    operation.protection.project_id = turn.project.clone();
    store_passive_checkpoint(&turn, &operation);
    (turn, operation)
}

fn store_passive_checkpoint(turn: &OrdinaryRoot, operation: &EngramCheckpointOperation) {
    let mut inner = turn.state.inner.lock().unwrap();
    install_historical_checkpoint(&mut inner, &turn.session, operation);
    turn.state.commit_locked(&mut inner).unwrap();
}

fn assert_passive_checkpoint(turn: &OrdinaryRoot, expected: &EngramCheckpointOperation) {
    let loaded = load_state(turn.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    let canonical = loaded
        .engram_session_obligations
        .checkpoint_operations
        .iter()
        .find(|operation| operation.original.id == expected.original.id)
        .unwrap();
    assert_eq!(
        serde_json::to_value(canonical).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    if let Some(index) = loaded.find_session_index(&turn.session) {
        assert_eq!(
            loaded.sessions[index].engram.historical_control["engramCheckpointOperation"],
            serde_json::to_value(expected).unwrap()
        );
    }
    assert!(
        loaded.test_run_observations.is_empty(),
        "historical receipts are not native test credit"
    );
    turn.assert_no_control();
}

#[test]
fn historical_checkpoint_original_and_uncertain_attempt_survive_local_reload() {
    for progress in ["retained", "unknown", "accepted"] {
        let (turn, mut operation) = passive_checkpoint_turn("checkpoint-history-image");
        if progress == "retained" {
            operation.progress = EngramExistingCheckpointProgress::Retained;
            operation.attempts[0].result = None;
        } else if progress == "accepted" {
            accepted_history(&mut operation);
        }
        operation.validate().unwrap();
        store_passive_checkpoint(&turn, &operation);
        assert_passive_checkpoint(&turn, &operation);
        turn.assert_no_prompt();
    }
}

#[test]
fn historical_checkpoint_metadata_survives_missing_or_quarantined_session() {
    for remove in [false, true] {
        let (turn, operation) = passive_checkpoint_turn("checkpoint-missing-row");
        turn.state.shutdown_persist_blocking();
        let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
        if remove {
            connection
                .execute("DELETE FROM sessions WHERE id=?1", [&turn.session])
                .unwrap();
        } else {
            connection.execute("UPDATE sessions SET value_json=json_set(value_json,'$.session.agent','unsupported_fixture_agent') WHERE id=?1", [&turn.session]).unwrap();
        }
        drop(connection);
        let loaded = load_state(turn.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        assert!(loaded.find_session_index(&turn.session).is_none());
        assert_eq!(
            serde_json::to_value(&loaded.engram_session_obligations.checkpoint_operations).unwrap(),
            json!([operation])
        );
        turn.assert_no_control();
    }
}

#[test]
fn historical_checkpoint_malformed_or_missing_metadata_is_not_defaulted_or_rewritten() {
    for defect in ["missing", "future_progress", "mirror_mismatch"] {
        let (turn, _) = passive_checkpoint_turn("checkpoint-rejected-metadata");
        turn.state.shutdown_persist_blocking();
        let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
        let mut metadata: Value =
            serde_json::from_str(&read_persisted_metadata_json(&connection).unwrap().unwrap())
                .unwrap();
        match defect {
            "missing" => {
                metadata["engramSessionObligations"]
                    .as_object_mut()
                    .unwrap()
                    .remove("checkpointOperations");
            }
            "future_progress" => {
                metadata["engramSessionObligations"]["checkpointOperations"][0]["progress"]["state"] =
                    json!("future_state")
            }
            "mirror_mismatch" => {
                connection.execute("UPDATE sessions SET value_json=json_set(value_json,'$.engramCheckpointOperation.original.id','different-operation') WHERE id=?1", [&turn.session]).unwrap();
            }
            _ => unreachable!(),
        }
        replace_metadata_units_for_test(&connection, metadata);
        let before = read_metadata_units_unchecked(&connection).unwrap();
        drop(connection);
        assert!(
            load_state(turn.state.persistence_path.as_path()).is_err(),
            "{defect}"
        );
        let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
        assert_eq!(
            read_metadata_units_unchecked(&connection).unwrap(),
            before,
            "failed decoding cannot repair historical identity or fabricate a default ledger"
        );
        turn.assert_no_control();
    }
}
