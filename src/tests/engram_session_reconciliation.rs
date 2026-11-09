//! Ordinary metadata saves do not inspect or reconcile retired control grants.
//! Real metadata failure preserves every session's original private history.
use super::*;

#[test]
fn an_ordinary_metadata_save_keeps_all_session_images_on_success_and_real_write_failure() {
    for fail_write in [false, true] {
        let turn = super::uncertain_begin::OrdinaryRoot::new("ordinary-metadata-session-isolation");
        let peer =
            create_test_project_session(&turn.state, Agent::Codex, &turn.project, &turn.root);
        let ids = [&turn.session, &peer];
        {
            let mut inner = turn.state.inner.lock().unwrap();
            for (ordinal, id) in ids.iter().enumerate() {
                let record = inner.session_mut(id).unwrap();
                record.engram.historical_control.insert("absenceInspectionReceipt".into(), json!({
                    "schema_version":1, "scope":"control_session_inspect", "mutation_enabled":false,
                    "session_id":id, "retained_grant_id":format!("private-original-{ordinal}"),
                    "session_present":false, "session_grants_present":false, "retained_grant_present":false,
                    "future":{"raw":"żółw 🐢\nno-current-proof"},
                }));
                record.set_auto_dispatch_blocked(true);
            }
            turn.state.commit_locked(&mut inner).unwrap();
        }
        let images = {
            let inner = turn.state.inner.lock().unwrap();
            ids.iter()
                .map(|id| {
                    serde_json::to_value(PersistedSessionRecord::from_record(
                        &inner.sessions[inner.find_session_index(id).unwrap()],
                    ))
                    .unwrap()
                })
                .collect::<Vec<_>>()
        };
        let before = turn
            .state
            .inner
            .lock()
            .unwrap()
            .find_project(&turn.project)
            .unwrap()
            .engram
            .clone()
            .unwrap();
        turn.state.shutdown_persist_blocking();
        if fail_write {
            rusqlite::Connection::open(turn.state.persistence_path.as_path())
                .unwrap()
                .execute_batch(
                    "CREATE TRIGGER refuse_ordinary_metadata BEFORE UPDATE ON state_metadata_units
                 BEGIN SELECT RAISE(ABORT, 'injected metadata failure'); END;",
                )
                .unwrap();
        }
        let mut requested = before.clone();
        requested.execution_observability = !before.execution_observability;
        let result = turn
            .state
            .update_project_engram_settings(&turn.project, requested.clone());
        if fail_write {
            let error = result
                .err()
                .expect("the real metadata transaction must fail");
            assert!(error.message.contains("persist"), "{error:?}");
            rusqlite::Connection::open(turn.state.persistence_path.as_path())
                .unwrap()
                .execute_batch("DROP TRIGGER refuse_ordinary_metadata;")
                .unwrap();
        } else {
            let response = serde_json::to_string(&result.unwrap()).unwrap();
            assert!(!response.contains("private-original"));
            assert!(!response.contains("no-current-proof"));
        }
        let expected = if fail_write { &before } else { &requested };
        let inner = turn.state.inner.lock().unwrap();
        assert_eq!(
            inner.find_project(&turn.project).unwrap().engram.as_ref(),
            Some(expected)
        );
        assert!(!inner.engram_project_resets.contains(&turn.project));
        for (ordinal, id) in ids.iter().enumerate() {
            assert_eq!(
                serde_json::to_value(PersistedSessionRecord::from_record(
                    &inner.sessions[inner.find_session_index(id).unwrap()]
                ))
                .unwrap(),
                images[ordinal]
            );
        }
        drop(inner);
        let loaded = load_state(turn.state.persistence_path.as_path())
            .unwrap()
            .unwrap();
        assert_eq!(
            loaded.find_project(&turn.project).unwrap().engram.as_ref(),
            Some(expected)
        );
        for (ordinal, id) in ids.iter().enumerate() {
            assert_eq!(
                serde_json::to_value(PersistedSessionRecord::from_record(
                    &loaded.sessions[loaded.find_session_index(id).unwrap()]
                ))
                .unwrap(),
                images[ordinal]
            );
        }
        turn.assert_no_prompt();
        turn.assert_no_control();
    }
}
