// Lifecycle regressions for carried launcher observations. Uses the ordinary
// turn and launcher-artifact fixtures in engram_carried_checks.rs; exercises
// polling, prompt notices and persisted reload, without granting verification.
use super::*;

fn assert_no_finished_gate_rerun_notice(record: &mut SessionRecord) {
    EngramHost::turn_prompt_composing(record);
    let notice = record
        .engram
        .pending_source_root_line
        .as_deref()
        .unwrap_or("");
    assert!(
        !notice.contains("background full gate")
            || (!notice.contains("earned no credit")
                && !notice.contains("Run the gate again")
                && !notice.contains("a newer background gate took its place")),
        "the next prompt must not report a completed gate as lost or invite a duplicate: {notice}"
    );
}

#[test]
fn a_finished_gate_retires_on_poll_and_stops_counting_as_open() {
    let (turn, worktree) = ordinary_carried_turn("finished-gate-retirement");
    ordinary_launch_gate(&turn, &worktree, true);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));

    turn.state.poll_engram_carried_runs();

    turn.record_mut(|record| {
        assert_no_finished_gate_rerun_notice(record);
        assert!(
            record.engram.carried_checks.is_empty(),
            "terminal launcher custody must retire without waiting for another launch"
        );
        assert!(!engram_has_open_check(record));
        assert!(record.engram.carried_consumed_runs.contains(&run));
    });
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn a_fifth_launch_after_four_finished_gates_does_not_prompt_a_rerun() {
    let (turn, worktree) = ordinary_carried_turn("finished-gate-capacity");
    for index in 0..ENGRAM_CARRIED_CHECK_LIMIT {
        launch_detached(&turn, &worktree, &format!("finished-{index}"));
        write_named_run(&worktree, &format!("test-finished-{index}"), "passed");
        turn.state.poll_engram_carried_runs();
    }

    launch_detached(&turn, &worktree, "still-running");

    turn.record_mut(|record| {
        assert_no_finished_gate_rerun_notice(record);
        let keys = record
            .engram
            .carried_checks
            .iter()
            .map(|carried| carried.check.key.as_str())
            .collect::<Vec<_>>();
        assert_eq!(keys, ["still-running"]);
        assert_eq!(
            record.engram.carried_consumed_runs.len(),
            ENGRAM_CARRIED_CHECK_LIMIT
        );
    });
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn a_reload_after_a_finished_gate_does_not_prompt_a_rerun() {
    let (turn, worktree) = ordinary_carried_turn("finished-gate-reload");
    ordinary_launch_gate(&turn, &worktree, true);
    finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();

    let persisted = turn.record(|record| {
        serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap()
    });
    let loaded: PersistedSessionRecord = serde_json::from_value(persisted).unwrap();
    let mut record = loaded.into_record().unwrap();

    assert_no_finished_gate_rerun_notice(&mut record);
    assert!(record.engram.carried_checks.is_empty());
    assert!(!engram_has_open_check(&record));
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn a_finished_idle_gate_removes_its_restart_marker_through_delta_persistence() {
    let (turn, worktree) = ordinary_carried_turn("finished-idle-gate-persistence");
    ordinary_launch_gate(&turn, &worktree, true);
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    let guard = phase_sync::PollGuard::new();
    while turn.record(|record| {
        record.engram.unadmitted_active_turn.is_some() || record.engram.unadmitted_worker_running
    }) {
        guard.wait("ordinary launch observation settles before the idle persistence cut");
    }
    turn.state.shutdown_persist_blocking();
    let path = turn.state.persistence_path.as_path();
    {
        let inner = turn.state.inner.lock().unwrap();
        let index = inner.find_session_index(&turn.session_id).unwrap();
        assert_eq!(
            PersistedSessionRecord::from_record(&inner.sessions[index])
                .engram_carried_launches
                .len(),
            1,
            "the idle session begins with a durable restart marker"
        );
        persist_state(path, &inner).unwrap();
    }
    let before = turn.record(|record| record.mutation_stamp);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));

    turn.state.poll_engram_carried_runs();

    assert_retired_run(&turn, &run);
    let delta = collect_persist_delta_from_shared_state(&turn.state.inner, before);
    assert!(
        delta.changed_sessions.iter().any(|record| {
            record.session.id == turn.session_id && record.engram_carried_launches.is_empty()
        }),
        "idle terminal retirement must persist the marker removal"
    );
    let mut cache = SqlitePersistConnectionCache::new();
    persist_delta_via_cache_timed(&mut cache, path, &delta, &mut PersistTickTimings::default())
        .unwrap();
    let mut loaded = load_state(path).unwrap().unwrap();
    let index = loaded.find_session_index(&turn.session_id).unwrap();
    assert_no_finished_gate_rerun_notice(&mut loaded.sessions[index]);
    assert!(!engram_has_open_check(&loaded.sessions[index]));
}
