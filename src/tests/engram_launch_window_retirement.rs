// Ordinary launch retirement preserves the actual invocation's known root
// and original start, independently of source-credit eligibility. Real recorder
// events and synthetic run records exercise ordinary correlation without
// launching a shell or test process.
use super::*;

#[path = "engram_delayed_retirement_order.rs"]
mod delayed_retirement_order;

fn begin_unmeasured_launch(
    turn: &EOCheckedTurn,
    worktree: &FsPath,
    recorder: &mut SessionRecorder,
    key: &str,
) -> TestRunCommandObservation {
    let cwd = fs::canonicalize(worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let (provenance, id) = {
        let inner = turn.state.inner.lock().unwrap();
        let index = inner.find_session_index(&turn.session_id).unwrap();
        let record = &inner.sessions[index];
        let provenance = EngramObservationProvenance::Runtime {
            token: record.runtime.runtime_token().unwrap(),
            turn_generation: record.active_turn_generation,
        };
        let id = TestRunCommandOrigin::capture(&inner, index, &provenance, key).execution_id();
        (provenance, id)
    };
    recorder.set_observation_provenance(provenance);
    recorder
        .command_started_in(key, DETACHED_GATE, Some(DETACHED_GATE), Some(&cwd))
        .unwrap();
    let row = turn
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_observations
        .get(&id)
        .unwrap()
        .clone();
    assert_original_window(turn, worktree, &row);
    row
}

fn launch_start(row: &TestRunCommandObservation) -> &str {
    row.summary
        .started_at
        .as_deref()
        .expect("the original execution start")
}

fn assert_original_window(turn: &EOCheckedTurn, root: &FsPath, row: &TestRunCommandObservation) {
    let expected = (engram_path_key(root), launch_start(row).to_owned());
    turn.record(|record| {
        assert!(
            record.engram.carried_unmatched_launches.contains(&expected),
            "the independently observed root/start remains an exclusion"
        );
        assert!(record.engram.active_turn_checks.is_empty());
        assert!(
            record.engram.carried_checks.is_empty(),
            "a location fact creates no carried proof"
        );
    });
    assert_launch_identity(turn, row);
}

fn assert_launch_identity(turn: &EOCheckedTurn, row: &TestRunCommandObservation) {
    let inner = turn.state.inner.lock().unwrap();
    let now = inner.test_run_observations.get(&row.execution_id).unwrap();
    assert_eq!(
        (
            &now.execution_id,
            &now.runtime_identity,
            &now.tool_identity,
            now.turn_generation,
            &now.grant_identity,
            &now.command_fingerprint,
            &now.work_binding,
            &now.source_root,
            &now.summary.started_at,
            &now.summary.worktree
        ),
        (
            &row.execution_id,
            &row.runtime_identity,
            &row.tool_identity,
            row.turn_generation,
            &row.grant_identity,
            &row.command_fingerprint,
            &row.work_binding,
            &row.source_root,
            &row.summary.started_at,
            &row.summary.worktree
        )
    );
    assert!(now.artifact.is_none() && now.evidence.is_empty() && now.final_unbound_proof.is_none());
    drop(inner);
    assert_ordinary_turn_has_no_native_requests(turn);
}

fn publish_running_run(runs: &FsPath, root: &FsPath, started: &str) -> PathBuf {
    let run_id = "test-earlier-uncredited";
    let directory = runs.join(run_id);
    fs::create_dir_all(&directory).unwrap();
    let fingerprint = "f".repeat(64);
    let stages = [
        "cargo-check",
        "typescript",
        "fingerprint-tests",
        "rust-tests",
        "vitest",
    ];
    fs::write(
        directory.join("request.json"),
        serde_json::to_vec(&json!({
            "runId": run_id, "root": root.to_string_lossy(),
            "stages": stages.iter().map(|name| json!({ "name": name })).collect::<Vec<_>>(),
            "full": true, "detached": false, "started": started,
            "expectedFingerprint": fingerprint,
        }))
        .unwrap(),
    )
    .unwrap();
    // Match the launcher's initial result: no end, exit or terminal stages.
    fs::write(
        directory.join("results.json"),
        serde_json::to_vec(&json!({
            "runId": run_id, "state": "running", "started": started, "preflight": [],
            "stages": stages.iter().map(|name| json!({ "name": name, "state": "unrun" }))
                .collect::<Vec<_>>(),
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(
        engram_terminal_record_digest(&directory).is_none(),
        "the running fixture must not establish a terminal digest before polling"
    );
    directory
}

fn retired_mismatch_cannot_lend_run(missing_trace: bool, late_publication: bool) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = ordinary_carried_turn(if missing_trace {
        "unmeasured-unowned-late-run"
    } else {
        "unmeasured-unbound-running-run"
    });
    let (binding, trace) = turn.record(|record| {
        (
            record.engram.work_binding.clone(),
            record.engram.unadmitted_active_turn.clone(),
        )
    });
    turn.record_mut(|record| {
        if missing_trace {
            record.engram.unadmitted_active_turn = None;
        } else {
            record.engram.work_binding = None;
        }
    });
    let mut recorder = turn.recorder();
    let mark = begin_unmeasured_launch(&turn, &worktree, &mut recorder, "uncredited");
    recorder
        .command_completed_with_exit(
            "uncredited",
            &format!("{DETACHED_GATE} --notify session-other"),
            "STARTED",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    let expected = (engram_path_key(&worktree), launch_start(&mark).to_owned());
    let window_retained =
        turn.record(|record| record.engram.carried_unmatched_launches.contains(&expected));
    turn.record_mut(|record| {
        record.engram.work_binding = binding;
        record.engram.unadmitted_active_turn = trace;
    });

    let cwd = fs::canonicalize(&worktree).unwrap();
    recorder
        .command_started_in(
            "gate",
            DETACHED_GATE,
            Some(DETACHED_GATE),
            Some(&cwd.to_string_lossy()),
        )
        .unwrap();
    turn.wait_for_snapshots();
    // Pin the later observation to the retained timestamp. The older run is
    // exactly one second inside the matcher's slack regardless of scheduling.
    turn.record_mut(|record| {
        let check = record
            .engram
            .active_turn_checks
            .iter_mut()
            .find(|check| check.key == "gate" && check.end.is_none())
            .expect("the restored original ordinary owner starts the later check");
        check.started_at = launch_start(&mark).to_owned();
    });
    let earlier_at = (engram_parse_time(launch_start(&mark)).unwrap()
        - chrono::Duration::seconds(1))
    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let runs = review_runs(&worktree);
    if !late_publication {
        publish_running_run(&runs, &cwd, &earlier_at);
    }
    recorder
        .command_completed_with_exit(
            "gate",
            DETACHED_GATE,
            "STARTED",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    turn.wait_for_snapshots();
    if late_publication {
        publish_running_run(&runs, &cwd, &earlier_at);
    }
    turn.state.poll_engram_carried_runs();
    let borrowed = turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        assert!(
            carried.fence.is_none(),
            "an unrelated fence must not satisfy this witness"
        );
        assert!(
            carried.terminal_digest.is_none() && !carried.terminal_conflict,
            "the borrowed running record must not have been observed as terminal"
        );
        if !carried.ambiguous {
            assert_eq!(
                carried.run_directory.as_ref(),
                Some(&runs.join("test-earlier-uncredited")),
                "without an ambiguity marker the fixture must reach run borrowing"
            );
        }
        carried.run_directory.clone()
    });
    let run = write_run(
        &runs,
        "test-earlier-uncredited",
        &cwd,
        &earlier_at,
        "passed",
        &"f".repeat(64),
    );
    assert!(
        run.exists(),
        "the artifact really exists, previously matched {borrowed:?}"
    );
    assert_carried_ambiguity(&turn);
    let observation = turn.finish();
    assert_ordinary_observation_has_no_native_credit(&turn, &observation);
    assert_carried_ambiguity(&turn);
    assert!(
        window_retained,
        "the original root/start must be retained at retirement"
    );
}

#[test]
fn mismatched_unbound_launch_cannot_lend_its_running_run() {
    retired_mismatch_cannot_lend_run(false, false);
}

#[test]
fn mismatched_unowned_launch_cannot_lend_a_late_published_run() {
    retired_mismatch_cannot_lend_run(true, true);
}

fn replaced_owner_keeps_window(runtime_changed: bool) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = ordinary_carried_turn("retained-original-window");
    turn.record_mut(|record| record.engram.work_binding = None);
    let mut recorder = turn.recorder();
    let original = begin_unmeasured_launch(&turn, &worktree, &mut recorder, "old");
    // These are existing runtime-selector unit inputs, not fabricated grants.
    turn.record_mut(|record| {
        if runtime_changed {
            let SessionRuntime::Codex(runtime) = &mut record.runtime else {
                panic!("Codex fixture");
            };
            runtime.runtime_id.push_str("-replacement");
        } else {
            record.active_turn_generation += 1;
        }
    });
    recorder
        .command_completed_with_exit(
            "old",
            DETACHED_GATE,
            "STARTED",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    assert_original_window(&turn, &worktree, &original);
}

#[test]
fn runtime_replacement_retains_the_original_launch_window() {
    replaced_owner_keeps_window(true);
}

#[test]
fn later_turn_retains_the_original_launch_window() {
    replaced_owner_keeps_window(false);
}

#[test]
fn bounded_window_eviction_keeps_newest_locations_and_old_execution_identity() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = ordinary_carried_turn("bounded-location-window");
    let binding = turn.record(|record| record.engram.work_binding.clone());
    turn.record_mut(|record| record.engram.work_binding = None);
    let now = chrono::Utc::now();
    let at = |i| {
        (now + chrono::Duration::milliseconds(i))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    };
    let clock = LaunchCustodyClock::at(&at(0));
    let mut recorder = turn.recorder();
    let oldest = begin_unmeasured_launch(&turn, &worktree, &mut recorder, "oldest");
    let mut retained = Vec::new();
    for index in 1..=ENGRAM_CARRIED_UNMATCHED_LAUNCH_LIMIT {
        clock.advance_to(&at(index as i64));
        let row =
            begin_unmeasured_launch(&turn, &worktree, &mut recorder, &format!("next-{index}"));
        retained.push((engram_path_key(&worktree), launch_start(&row).to_owned()));
    }
    turn.record(|record| assert_eq!(record.engram.carried_unmatched_launches, retained));
    let inner = turn.state.inner.lock().unwrap();
    assert_eq!(
        inner.test_run_observations.get(&oldest.execution_id),
        Some(&oldest),
        "bounded location retention never rewrites the old original execution"
    );
    drop(inner);
    // The obsolete pending queue kept the evicted entry itself. The current
    // bounded owner keeps newer same-root starts: they must still exclude
    // this oldest run, even though its individual window is no longer kept.
    let cwd = fs::canonicalize(&worktree).unwrap();
    let run = publish_running_run(&review_runs(&worktree), &cwd, launch_start(&oldest));
    turn.record_mut(|record| record.engram.work_binding = binding);
    let later = at((ENGRAM_CARRIED_UNMATCHED_LAUNCH_LIMIT + 1) as i64);
    clock.advance_to(&later);
    recorder
        .command_started_in(
            "after-cap",
            DETACHED_GATE,
            Some(DETACHED_GATE),
            Some(&cwd.to_string_lossy()),
        )
        .unwrap();
    turn.wait_for_snapshots();
    recorder
        .command_completed_with_exit(
            "after-cap",
            DETACHED_GATE,
            "STARTED",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    assert_eq!(
        engram_find_carried_run(
            &turn.session_id,
            &cwd,
            &later,
            &[],
            &std::collections::BTreeSet::new()
        )
        .0,
        Some(run)
    );
    turn.state.poll_engram_carried_runs();
    turn.record(|record| {
        assert_eq!(record.engram.carried_checks.len(), 1);
        let carried = &record.engram.carried_checks[0];
        assert!(
            carried.fence.is_none(),
            "no unrelated fence masks the overflow witness"
        );
        assert!(
            carried.ambiguous && carried.run_directory.is_none(),
            "newer same-root windows still prevent borrowing the oldest running artifact"
        );
    });
    assert_launch_identity(&turn, &oldest);
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn own_abandonment_retains_the_original_launch_window() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = ordinary_carried_turn("abandoned-original-window");
    turn.record_mut(|record| record.engram.work_binding = None);
    let mut recorder = turn.recorder();
    let original = begin_unmeasured_launch(&turn, &worktree, &mut recorder, "abandoned");
    recorder.command_abandoned("abandoned").unwrap();
    assert_original_window(&turn, &worktree, &original);
    let inner = turn.state.inner.lock().unwrap();
    assert_eq!(
        inner
            .test_run_observations
            .get(&original.execution_id)
            .unwrap()
            .summary
            .state,
        TestRunState::Unknown
    );
}

#[test]
fn disabled_abandonment_keeps_only_the_original_location_without_a_notice() {
    for project_disabled in [false, true] {
        let _placement = TestTempRootDirectoryScope::repository_local();
        let (turn, worktree) = ordinary_carried_turn("disabled-abandoned-window");
        turn.record_mut(|record| record.engram.work_binding = None);
        let mut recorder = turn.recorder();
        let original = begin_unmeasured_launch(&turn, &worktree, &mut recorder, "disabled");
        if project_disabled {
            let mut inner = turn.state.inner.lock().unwrap();
            let index = inner.find_session_index(&turn.session_id).unwrap();
            let project_id = inner.sessions[index].session.project_id.clone().unwrap();
            inner
                .projects
                .iter_mut()
                .find(|project| project.id == project_id)
                .unwrap()
                .engram = None;
        } else {
            turn.set_observability(false);
        }
        recorder.command_abandoned("disabled").unwrap();
        assert_original_window(&turn, &worktree, &original);
        turn.record(|record| assert!(record.engram.pending_source_root_line.is_none()));
    }
}

#[test]
fn a_matched_ordinary_launch_keeps_its_own_artifact_without_native_verification() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = ordinary_carried_turn("ordinary-window-clean-control");
    ordinary_launch_gate(&turn, &worktree, true);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_artifact_custody(&turn, &run, true);
}
