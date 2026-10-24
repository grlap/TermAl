// Diagnostic retirement must preserve a possibly launched full gate's
// original ambiguity window. Real recorder events and synthetic run records
// exercise correlation and settlement without launching a shell or provider.
use super::*;

#[path = "engram_delayed_retirement_order.rs"]
mod delayed_retirement_order;

fn begin_diagnostic(
    turn: &CheckedTurn,
    worktree: &FsPath,
    recorder: &mut SessionRecorder,
    key: &str,
) -> EngramLaunchDiagnostic {
    let cwd = fs::canonicalize(worktree).unwrap().to_string_lossy().into_owned();
    recorder.command_started_in(key, DETACHED_GATE, Some(DETACHED_GATE), Some(&cwd)).unwrap();
    turn.record(|record| {
        assert!(record.engram.active_turn_checks.is_empty(),
            "missing authority must create a diagnostic, not an eligible check");
        let mark = record.engram.pending_launch_diagnostics.iter()
            .find(|mark| mark.key == key).expect("the recorder retained this start").clone();
        assert_eq!(mark.root.as_deref().map(engram_exact_path_key),
            Some(engram_exact_path_key(&fs::canonicalize(worktree).unwrap())));
        mark
    })
}

fn assert_original_window(turn: &CheckedTurn, mark: &EngramLaunchDiagnostic) {
    let expected = (engram_path_key(mark.root.as_ref().unwrap()), mark.started_at.clone());
    turn.record(|record| {
        assert!(record.engram.carried_unmatched_launches.contains(&expected),
            "retirement lost the original known root/start: {:?}",
            record.engram.carried_unmatched_launches);
        assert!(record.engram.active_turn_checks.is_empty());
        assert!(record.engram.carried_checks.is_empty(),
            "an ambiguity marker grants no carried verification");
    });
}

fn publish_running_run(runs: &FsPath, root: &FsPath, started: &str) -> PathBuf {
    let run_id = "test-earlier-uncredited";
    let directory = runs.join(run_id);
    fs::create_dir_all(&directory).unwrap();
    let fingerprint = "f".repeat(64);
    let stages = ["cargo-check", "typescript", "fingerprint-tests", "rust-tests", "vitest"];
    fs::write(directory.join("request.json"), serde_json::to_vec(&json!({
        "runId": run_id, "root": root.to_string_lossy(),
        "stages": stages.iter().map(|name| json!({ "name": name })).collect::<Vec<_>>(),
        "full": true, "detached": false, "started": started,
        "expectedFingerprint": fingerprint,
    })).unwrap()).unwrap();
    // Match the launcher's initial result: no end, exit or terminal stages.
    fs::write(directory.join("results.json"), serde_json::to_vec(&json!({
        "runId": run_id, "state": "running", "started": started, "preflight": [],
        "stages": stages.iter().map(|name| json!({ "name": name, "state": "unrun" }))
            .collect::<Vec<_>>(),
    })).unwrap()).unwrap();
    assert!(engram_terminal_record_digest(&directory).is_none(),
        "the running fixture must not establish a terminal digest before polling");
    directory
}

fn retired_mismatch_cannot_lend_run(missing_grant: bool, late_publication: bool) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = named_turn(if missing_grant {
        "diagnostic-grantless-late-run"
    } else {
        "diagnostic-unbound-running-run"
    });
    let (binding, grant) = turn.record(|record| (
        record.engram.work_binding.clone(), record.engram.active_grant_id.clone(),
    ));
    turn.record_mut(|record| {
        if missing_grant { record.engram.active_grant_id = None; }
        else { record.engram.work_binding = None; }
    });
    let mut recorder = turn.recorder();
    let mark = begin_diagnostic(&turn, &worktree, &mut recorder, "uncredited");
    recorder.command_completed_with_exit("uncredited",
        &format!("{DETACHED_GATE} --notify session-other"), "STARTED",
        CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
    let expected = (engram_path_key(mark.root.as_ref().unwrap()), mark.started_at.clone());
    let window_retained = turn.record(|record| {
        assert!(record.engram.pending_launch_diagnostics.is_empty());
        record.engram.carried_unmatched_launches.contains(&expected)
    });
    turn.record_mut(|record| {
        record.engram.work_binding = binding;
        record.engram.active_grant_id = grant;
    });

    let cwd = fs::canonicalize(&worktree).unwrap();
    recorder.command_started_in("gate", DETACHED_GATE, Some(DETACHED_GATE),
        Some(&cwd.to_string_lossy())).unwrap();
    turn.wait_for_snapshots();
    // Pin the later observation to the retained timestamp. The older run is
    // exactly one second inside the matcher's slack regardless of scheduling.
    turn.record_mut(|record| {
        let check = record.engram.active_turn_checks.iter_mut()
            .find(|check| check.key == "gate" && check.end.is_none())
            .expect("restored authority starts the later check");
        check.started_at = mark.started_at.clone();
    });
    let earlier_at = (engram_parse_time(&mark.started_at).unwrap()
        - chrono::Duration::seconds(1)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let runs = review_runs(&worktree);
    if !late_publication { publish_running_run(&runs, &cwd, &earlier_at); }
    recorder.command_completed_with_exit("gate", DETACHED_GATE, "STARTED",
        CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
    turn.wait_for_snapshots();
    if late_publication { publish_running_run(&runs, &cwd, &earlier_at); }
    turn.state.poll_engram_carried_runs();
    let borrowed = turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        assert!(carried.fence.is_none(), "an unrelated fence must not satisfy this witness");
        assert!(carried.terminal_digest.is_none() && !carried.terminal_conflict,
            "the borrowed running record must not have been observed as terminal");
        if !carried.ambiguous {
            assert_eq!(carried.run_directory.as_ref(), Some(&runs.join("test-earlier-uncredited")),
                "without an ambiguity marker the fixture must reach run borrowing");
        }
        carried.run_directory.clone()
    });
    let run = write_run(&runs, "test-earlier-uncredited", &cwd,
        &earlier_at, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();
    assert!(checkpoint.get("verification_evidence").is_none(),
        "a later launch borrowed the retired launch's run {run:?} (matched {borrowed:?}): {checkpoint:#}");
    assert_refused(&turn, &checkpoint, "another background gate launched");
    assert!(window_retained, "the original root/start must be retained at retirement");
}

#[test]
fn mismatched_unbound_launch_cannot_lend_its_running_run() {
    retired_mismatch_cannot_lend_run(false, false);
}

#[test]
fn mismatched_grantless_launch_cannot_lend_a_late_published_run() {
    retired_mismatch_cannot_lend_run(true, true);
}

fn pruned_diagnostic_keeps_window(runtime_changed: bool) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = named_turn("diagnostic-pruned-window");
    turn.record_mut(|record| record.engram.work_binding = None);
    let mut recorder = turn.recorder();
    let mark = begin_diagnostic(&turn, &worktree, &mut recorder, "old");
    turn.record_mut(|record| {
        if runtime_changed {
            let SessionRuntime::Codex(runtime) = &mut record.runtime else {
                panic!("the fixture owns a Codex runtime");
            };
            runtime.runtime_id.push_str("-replacement");
        } else {
            record.active_turn_generation += 1;
        }
    });
    // This Current observation drives production pruning; it cannot finish
    // the previous runtime/turn's diagnostic under the reused key.
    recorder.command_completed_with_exit("old", DETACHED_GATE, "STARTED",
        CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
    turn.record(|record| assert!(record.engram.pending_launch_diagnostics.is_empty()));
    assert_original_window(&turn, &mark);
}

#[test]
fn runtime_pruning_retains_the_original_launch_window() {
    pruned_diagnostic_keeps_window(true);
}

#[test]
fn turn_pruning_retains_the_original_launch_window() {
    pruned_diagnostic_keeps_window(false);
}

#[test]
fn pending_cap_eviction_retains_the_oldest_launch_window() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = named_turn("diagnostic-cap-window");
    turn.record_mut(|record| record.engram.work_binding = None);
    let mut recorder = turn.recorder();
    let mark = begin_diagnostic(&turn, &worktree, &mut recorder, "oldest");
    for index in 0..ENGRAM_CARRIED_UNMATCHED_LAUNCH_LIMIT {
        begin_diagnostic(&turn, &worktree, &mut recorder, &format!("pending-{index}"));
    }
    turn.record(|record| {
        assert_eq!(record.engram.pending_launch_diagnostics.len(), ENGRAM_CARRIED_UNMATCHED_LAUNCH_LIMIT);
        assert!(record.engram.pending_launch_diagnostics.iter().all(|mark| mark.key != "oldest"));
    });
    assert_original_window(&turn, &mark);
}

#[test]
fn own_abandonment_retains_the_original_launch_window() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = named_turn("diagnostic-abandoned-window");
    turn.record_mut(|record| record.engram.work_binding = None);
    let mut recorder = turn.recorder();
    let mark = begin_diagnostic(&turn, &worktree, &mut recorder, "abandoned");
    recorder.command_abandoned("abandoned").unwrap();
    turn.record(|record| assert!(record.engram.pending_launch_diagnostics.is_empty()));
    assert_original_window(&turn, &mark);
}

#[test]
fn disabled_abandonment_discards_only_its_own_diagnostic_silently() {
    for project_disabled in [false, true] {
        let _placement = TestTempRootDirectoryScope::repository_local();
        let (turn, worktree) = named_turn("diagnostic-disabled-abandonment");
        turn.record_mut(|record| record.engram.work_binding = None);
        let mut recorder = turn.recorder();
        begin_diagnostic(&turn, &worktree, &mut recorder, "disabled");
        if project_disabled {
            let mut inner = turn.state.inner.lock().unwrap();
            let index = inner.find_session_index(&turn.session_id).unwrap();
            let project_id = inner.sessions[index].session.project_id.clone().unwrap();
            inner.projects.iter_mut().find(|project| project.id == project_id).unwrap().engram = None;
        } else {
            turn.record_mut(|record| record.engram.disabled_reason = Some("fixture disabled".to_owned()));
        }
        recorder.command_abandoned("disabled").unwrap();
        turn.record(|record| {
            assert!(record.engram.pending_launch_diagnostics.is_empty());
            assert!(record.engram.pending_source_root_line.is_none());
            assert!(record.engram.carried_unmatched_launches.is_empty());
            assert!(record.engram.active_turn_checks.is_empty());
            assert!(record.engram.carried_checks.is_empty());
        });
    }
}

#[test]
fn a_matched_authorized_launch_still_credits_its_own_run() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = named_turn("diagnostic-window-clean-control");
    launch_gate(&turn, &worktree, true);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();
    assert_credited(&turn, &checkpoint, &run, "succeeded");
}
