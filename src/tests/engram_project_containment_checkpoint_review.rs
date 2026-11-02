//! Quarantined rows and original history survive unrelated ordinary writes.
use super::*;

#[path = "engram_project_containment_checkpoint_watermark.rs"]
mod watermark;

fn quarantined_checkpoint_survives_unrelated_action(remove_other: bool) {
    for invalid_agent in [false, true] {
        let (turn, original) = passive_checkpoint_turn("checkpoint-quarantined-history");
        let other = create_test_project_session(&turn.state, Agent::Codex, &turn.project, &turn.root);
        turn.state.shutdown_persist_blocking();
        let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
        let sql = if invalid_agent {
            "UPDATE sessions SET value_json=json_set(value_json,'$.session.agent','unsupported_fixture_agent') WHERE id=?1"
        } else {
            "UPDATE sessions SET value_json=json_set(value_json,'$.session.projectId','missing-fixture-project') WHERE id=?1"
        };
        assert_eq!(connection.execute(sql, [&turn.session]).unwrap(), 1);
        let before: String = connection.query_row("SELECT value_json FROM sessions WHERE id=?1",
            [&turn.session], |row| row.get(0)).unwrap();
        drop(connection);
        let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
        assert!(loaded.find_session_index(&turn.session).is_none());
        assert!(loaded.quarantined_persisted_session_ids.contains(&turn.session));
        *turn.state.inner.lock().unwrap() = loaded;
        turn.state.select_test_scripted_engram_budget_clock();
        turn.state.install_control_test_transport(turn.transport.clone());
        if remove_other {
            turn.state.kill_session(&other).unwrap();
        } else {
            let mut inner = turn.state.inner.lock().unwrap();
            inner.session_mut(&other).unwrap().session.preview = "Unrelated ordinary metadata".into();
            turn.state.commit_locked(&mut inner).unwrap();
        }
        let connection = rusqlite::Connection::open(turn.state.persistence_path.as_path()).unwrap();
        let after: String = connection.query_row("SELECT value_json FROM sessions WHERE id=?1",
            [&turn.session], |row| row.get(0)).unwrap();
        assert_eq!(after, before, "unrelated ordinary work must not rewrite a quarantined mirror");
        drop(connection);
        let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
        assert!(loaded.quarantined_persisted_session_ids.contains(&turn.session));
        assert_passive_checkpoint(&turn, &original);
        turn.assert_no_prompt();
    }
}

#[test]
fn historical_quarantined_checkpoint_survives_unrelated_ordinary_write() {
    quarantined_checkpoint_survives_unrelated_action(false);
}

#[test]
fn historical_quarantined_checkpoint_survives_unrelated_public_deletion() {
    quarantined_checkpoint_survives_unrelated_action(true);
}

#[test]
fn historical_checkpoint_deletion_keeps_all_originals_instead_of_compacting_evidence() {
    const SESSIONS: usize = 24;
    let (turn, original) = passive_checkpoint_turn("checkpoint-originals-retained");
    let mut sessions = vec![turn.session.clone()];
    {
        let mut inner = turn.state.inner.lock().unwrap();
        for ordinal in 1..SESSIONS {
            let session = inner.create_session(Agent::Codex, None,
                turn.root.to_string_lossy().into_owned(), Some(turn.project.clone()), None).session.id;
            let mut operation = passive_checkpoint(&turn.root, &session);
            operation.original.id = format!("historical-checkpoint-{ordinal}");
            operation.original.request["idempotency_key"] = json!(format!("historical-key-{ordinal}"));
            operation.original.request["grant_id"] = json!(format!("historical-grant-{ordinal}"));
            operation.protection.id = operation.original.id.clone();
            operation.protection.project_id = turn.project.clone();
            operation.protection.grant_id = format!("historical-grant-{ordinal}");
            operation.protection.frozen_request = serde_json::to_value(&operation.original).unwrap();
            operation.attempts[0].id = operation.original.id.clone();
            operation.validate().unwrap();
            inner.session_mut(&session).unwrap().engram.historical_control
                .insert("engramCheckpointOperation".into(), serde_json::to_value(&operation).unwrap());
            inner.engram_session_obligations.checkpoint_operations.push(operation);
            sessions.push(session);
        }
        inner.engram_session_obligations.validate().unwrap();
        turn.state.commit_locked(&mut inner).unwrap();
    }
    let before = serde_json::to_value(&turn.state.inner.lock().unwrap().engram_session_obligations).unwrap();
    for session in &sessions { turn.state.kill_session(session).unwrap(); }
    let loaded = load_state(turn.state.persistence_path.as_path()).unwrap().unwrap();
    assert!(sessions.iter().all(|session| loaded.find_session_index(session).is_none()));
    assert_eq!(loaded.engram_session_obligations.checkpoint_operations.len(), SESSIONS);
    assert_eq!(serde_json::to_value(&loaded.engram_session_obligations).unwrap(), before,
        "administration adds no receipt and cannot prune imported original evidence");
    assert_passive_checkpoint(&turn, &original);
    turn.assert_no_prompt();
}
