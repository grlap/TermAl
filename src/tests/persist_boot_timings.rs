//! Disposable-state boot measurements and observable restore invariants.
//! These runs warm the fixture by writing it; they are never a cold-OS sample.
use super::*;

fn boot_measurement_fixture(path: &FsPath, sessions: usize) -> Vec<String> {
    let mut inner = StateInner::new();
    let mut ids = Vec::new();
    let workdir = path.parent().unwrap().to_string_lossy().into_owned();
    for _ in 0..sessions {
        let id = inner
            .create_session(Agent::Claude, None, workdir.clone(), None, None)
            .session
            .id;
        let index = inner.find_session_index(&id).unwrap();
        let record = inner.session_mut_by_index(index).unwrap();
        record.session.messages = (0..130)
            .map(|index| Message::Text {
                attachments: Vec::new(),
                id: format!("message-{index:03}"),
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: "synthetic boot fixture ".repeat(24),
                expanded_text: None,
                source: None,
            })
            .collect::<Vec<_>>()
            .into();
        record.session.message_count = 130;
        record.session.messages_loaded = true;
        record.message_start_index = 0;
        record.message_positions = build_message_positions(&record.session.messages);
        record.session.prompt_history = vec!["synthetic composer history".to_owned()];
        ids.push(id);
    }
    persist_state(path, &inner).expect("the synthetic state persists");
    ids
}

fn assert_boot_fixture_restored(inner: &StateInner, ids: &[String]) {
    assert_eq!(inner.sessions.len(), ids.len());
    for (record, id) in inner.sessions.iter().zip(ids) {
        assert!(
            record.session.id == *id,
            "session order and identities stay intact"
        );
        assert_eq!(record.session.message_count, 130);
        assert_eq!(record.message_start_index, 66);
        assert_eq!(record.session.messages.len(), SQLITE_SESSION_TAIL_MESSAGES);
        assert!(!record.session.messages_loaded);
        assert_eq!(record.session.messages[0].id(), "message-066");
        assert_eq!(record.session.messages[63].id(), "message-129");
        assert!(record.session.prompt_history == ["synthetic composer history"]);
    }
}

#[test]
fn boot_measurements_preserve_restore_and_quarantine() {
    let root = TestTempRoot::create("boot-measurement-restore");
    let path = root.join("termal.sqlite");
    let ids = boot_measurement_fixture(&path, 3);
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO sessions(id,value_json) VALUES('corrupt-fixture-row','{')",
            [],
        )
        .unwrap();
    for enabled in [false, true] {
        let mut timings = BootStateLoadTimings::new(enabled);
        let (loaded, connection) = load_state_from_sqlite_with_timings(&path, true, &mut timings)
            .expect("a bad row does not prevent restoring healthy sessions");
        let loaded = loaded.unwrap();
        assert_boot_fixture_restored(&loaded, &ids);
        assert!(
            loaded
                .quarantined_persisted_session_ids
                .contains("corrupt-fixture-row")
        );
        let retained: String = connection
            .query_row(
                "SELECT value_json FROM sessions WHERE id='corrupt-fixture-row'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(retained, "{", "loading never overwrites a quarantined row");
        if enabled {
            // The malformed row is a fourth decode attempt, but never reaches
            // transcript/history loading. No duration threshold is asserted.
            for (phase, calls) in [
                (BootStateLoadPhase::SessionDecode, 4),
                (BootStateLoadPhase::TranscriptTails, 3),
                (BootStateLoadPhase::PromptHistories, 3),
            ] {
                assert_eq!(
                    timings.measurements.get(&phase).map(|m| m.calls),
                    Some(calls)
                );
            }
            assert!(
                !timings
                    .measurements
                    .contains_key(&BootStateLoadPhase::Delegations)
            );
        } else {
            assert!(timings.measurements.is_empty());
        }
    }
}

#[test]
fn boot_measurements_include_failed_reconstruction_phases() {
    let root = TestTempRoot::create("boot-measurement-invalid-rebuild");
    let mut inner = StateInner::new();
    inner.create_session(
        Agent::Claude,
        None,
        root.to_string_lossy().into_owned(),
        None,
        None,
    );
    PersistedState::from_inner(&inner)
        .into_inner()
        .expect("healthy metadata restores");
    for malformed_checkpoint in [true, false] {
        let mut persisted = PersistedState::from_inner(&inner);
        if malformed_checkpoint {
            persisted.sessions[0].historical_control.insert(
                "engramCheckpointOperation".to_owned(),
                json!({"invalid": true}),
            );
        } else {
            persisted.sessions[0].session.project_id = Some("missing-project".to_owned());
        }
        let mut timings = BootStateLoadTimings::new(true);
        assert!(
            persisted
                .into_inner_with_boot_timings(&mut timings)
                .is_err()
        );
        assert_eq!(
            timings
                .measurements
                .get(&BootStateLoadPhase::SessionRebuild)
                .map(|m| m.calls),
            Some(1),
        );
        assert_eq!(
            timings
                .measurements
                .get(&BootStateLoadPhase::DerivedRebuild)
                .map(|m| m.calls),
            (!malformed_checkpoint).then_some(1),
        );
    }
}

#[test]
#[ignore = "fixture-only boot measurement; report timings without latency assertions"]
fn report_fixture_boot_load_subphases() {
    for sessions in [16, 512] {
        let root = TestTempRoot::create("boot-measurement-scale");
        let path = root.join("termal.sqlite");
        let ids = boot_measurement_fixture(&path, sessions);
        // Creation just wrote every row, so even the first load here has a
        // warm OS cache. A cold sample must come from an actual OS-reboot log.
        for (sample, enabled) in [
            ("fixture_first_load", true),
            ("fixture_unprofiled_reopen", false),
            ("fixture_profiled_reopen", true),
        ] {
            let mut timings = BootStateLoadTimings::new(enabled);
            let started = std::time::Instant::now();
            let (loaded, connection) =
                load_state_from_sqlite_with_timings(&path, true, &mut timings)
                    .expect("the synthetic measurement state loads");
            let elapsed = started.elapsed();
            eprintln!(
                "boot_fixture> sample={sample} cache=write_warm sessions={sessions} messages_per_session=130 elapsed_us={}",
                elapsed.as_micros()
            );
            timings.log();
            assert_boot_fixture_restored(&loaded.unwrap(), &ids);
            drop(connection);
        }
    }
}
