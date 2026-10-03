// Correction witnesses for observation-only linked-worktree correlation and
// full runtime/turn identity at diagnostic scans. Uses the real recorder and
// carried-run matcher; does not invent naming or verification authority.

fn start_at(turn: &CheckedTurn, key: &str, command: &str, cwd: Option<&str>) {
    turn.state.note_engram_command_started(
        &turn.session_id,
        &EngramObservationProvenance::Ambient,
        key,
        Some(command),
        cwd,
    );
}

fn linked_launch_marker_and_relaunch(binding_missing: bool, one_call: bool) {
    let (turn, worktree) = named_turn("correction-linked-launch");
    let directory = engram_exact_path_key(&fs::canonicalize(&worktree).unwrap());
    let (binding, grant) = turn.record(|record| {
        (
            record.engram.work_binding.clone(),
            record.engram.active_grant_id.clone(),
        )
    });
    turn.record_mut(|record| {
        if binding_missing {
            record.engram.work_binding = None;
        } else {
            record.engram.active_grant_id = None;
        }
    });
    let command = if one_call {
        format!("pushd \"{directory}\" && {DETACHED_GATE}")
    } else {
        DETACHED_GATE.to_owned()
    };
    let before = chrono::Utc::now();
    start_at(
        &turn,
        "uncredited",
        &command,
        (!one_call).then_some(directory.as_str()),
    );
    let after = chrono::Utc::now();
    let mark = turn.record(|record| record.engram.pending_launch_diagnostics[0].clone());
    assert_eq!(
        mark.root.as_deref().map(engram_exact_path_key),
        Some(directory.clone()),
        "a known linked-worktree location must not be filtered by credit eligibility"
    );
    let observed_start = engram_parse_time(&mark.started_at).unwrap();
    assert!(observed_start >= before - chrono::Duration::milliseconds(1));
    assert!(observed_start <= after);
    diagnostic_finish(&turn, "uncredited", &command, EngramCommandExit::Code(0));
    turn.record(|record| {
        assert_eq!(record.engram.carried_unmatched_launches.len(), 1);
        assert_eq!(
            record.engram.carried_unmatched_launches[0],
            (engram_path_key(&worktree), mark.started_at.clone())
        );
        assert!(record.engram.carried_checks.is_empty());
        assert!(record.engram.active_turn_checks.is_empty());
    });
    // Both launchers have the same owner and unchanged source. The earlier
    // run lies inside the matcher's slack, but must never be borrowed.
    let earlier = write_run(
        &review_runs(&worktree),
        "test-earlier-uncredited",
        &fs::canonicalize(&worktree).unwrap(),
        &mark.started_at,
        "passed",
        &"f".repeat(64),
    );
    turn.record_mut(|record| {
        record.engram.work_binding = binding;
        record.engram.active_grant_id = grant;
    });
    launch_gate(&turn, &worktree, true);
    turn.record(|record| assert!(record.engram.carried_checks[0].ambiguous));
    turn.state.poll_engram_carried_runs();
    turn.record(|record| {
        assert!(
            record.engram.carried_checks[0].run_directory.is_none(),
            "an ambiguous later launch must not match {earlier:?}"
        )
    });
    let checkpoint = turn.finish();
    assert_refused(&turn, &checkpoint, "another background gate launched");
}

#[test]
fn unbound_linked_reported_cwd_preserves_marker_and_prevents_run_borrowing() {
    linked_launch_marker_and_relaunch(true, false);
}

#[test]
fn grantless_linked_reported_cwd_preserves_marker_and_prevents_run_borrowing() {
    linked_launch_marker_and_relaunch(false, false);
}

#[test]
fn unbound_linked_one_call_preserves_marker_and_prevents_run_borrowing() {
    linked_launch_marker_and_relaunch(true, true);
}

#[test]
fn grantless_linked_one_call_preserves_marker_and_prevents_run_borrowing() {
    linked_launch_marker_and_relaunch(false, true);
}

fn replace_identity(turn: &CheckedTurn, runtime: bool) {
    turn.record_mut(|record| {
        if runtime {
            let SessionRuntime::Codex(current) = &mut record.runtime else {
                panic!("fixture owns a Codex runtime");
            };
            current.runtime_id.push_str("-fresh");
        } else {
            record.active_turn_generation += 1;
        }
    });
}

fn fresh_same_key_start(runtime: bool, bound: bool) {
    let (turn, worktree) = named_turn("correction-fresh-start");
    let directory = engram_exact_path_key(&fs::canonicalize(&worktree).unwrap());
    let binding = turn.record(|record| record.engram.work_binding.clone());
    turn.record_mut(|record| record.engram.work_binding = None);
    start_at(&turn, "reused", DETACHED_GATE, Some(&directory));
    let old = turn.record(|record| record.engram.pending_launch_diagnostics[0].clone());
    replace_identity(&turn, runtime);
    if bound {
        turn.record_mut(|record| record.engram.work_binding = binding);
    }
    start_at(&turn, "reused", DETACHED_GATE, Some(&directory));
    turn.record(|record| {
        if bound {
            assert_eq!(
                record.engram.active_turn_checks.len(),
                1,
                "a fresh authorised start must not be suppressed by an old diagnostic key"
            );
            assert!(record.engram.pending_launch_diagnostics.is_empty());
        } else {
            let marks = &record.engram.pending_launch_diagnostics;
            assert_eq!(marks.len(), 1);
            assert_eq!(marks[0].runtime, record.runtime.runtime_token());
            assert_eq!(marks[0].turn_generation, record.active_turn_generation);
            assert!(
                marks[0].runtime != old.runtime || marks[0].turn_generation != old.turn_generation,
                "a fresh unauthorised start must own its own diagnostic"
            );
        }
        let line = record.engram.pending_source_root_line.as_ref().unwrap();
        assert!(
            line.contains("its runtime or turn ended before its launch result"),
            "{line}"
        );
        assert_eq!(line.matches("diagnostic tracking").count(), 1);
    });
    turn.wait_for_snapshots();
    diagnostic_finish(&turn, "reused", DETACHED_GATE, EngramCommandExit::Code(0));
    turn.record(|record| {
        assert!(record.engram.pending_launch_diagnostics.is_empty());
        if bound {
            assert_eq!(record.engram.carried_checks.len(), 1);
            assert_eq!(record.engram.carried_checks[0].check.key, "reused");
        } else {
            assert!(record.engram.carried_checks.is_empty());
            assert_eq!(record.engram.carried_unmatched_launches.len(), 1);
            assert!(
                record
                    .engram
                    .pending_source_root_line
                    .as_ref()
                    .unwrap()
                    .contains("began without a binding")
            );
        }
    });
}

#[test]
fn fresh_turn_same_key_can_start_and_complete_its_bound_check() {
    fresh_same_key_start(false, true);
}

#[test]
fn fresh_runtime_same_key_can_start_and_complete_its_bound_check() {
    fresh_same_key_start(true, true);
}

#[test]
fn fresh_turn_same_key_can_start_and_complete_its_own_diagnostic() {
    fresh_same_key_start(false, false);
}

#[test]
fn fresh_runtime_same_key_can_start_and_complete_its_own_diagnostic() {
    fresh_same_key_start(true, false);
}

#[test]
fn a_current_finish_scan_retires_stale_identity_once_without_fabricating_outcome() {
    let turn = CheckedTurn::start("correction-stale-finish", false);
    diagnostic_start(&turn, "stale", DETACHED_GATE);
    replace_identity(&turn, false);
    diagnostic_finish(
        &turn,
        "not-the-stale-key",
        DETACHED_GATE,
        EngramCommandExit::Code(0),
    );
    let line = turn.record(|record| {
        assert!(record.engram.pending_launch_diagnostics.is_empty());
        assert!(record.engram.carried_unmatched_launches.is_empty());
        let line = record
            .engram
            .pending_source_root_line
            .clone()
            .expect("stale tracking loss is visible");
        assert!(
            line.contains("its runtime or turn ended before its launch result"),
            "{line}"
        );
        assert_eq!(line.matches("diagnostic tracking").count(), 1);
        assert!(!line.contains("background full gate") && !line.contains("earned no credit"));
        line
    });
    diagnostic_finish(&turn, "stale", DETACHED_GATE, EngramCommandExit::Code(0));
    assert_eq!(
        turn.record(|record| record.engram.pending_source_root_line.clone()),
        Some(line)
    );
    assert_no_verification_state(&turn);
}

#[test]
fn an_enabled_matched_completion_reports_command_mismatch_once() {
    let turn = CheckedTurn::start("correction-mismatch", false);
    diagnostic_start(&turn, "mismatch", DETACHED_GATE);
    let mark = turn.record(|record| record.engram.pending_launch_diagnostics[0].clone());
    diagnostic_finish(&turn, "mismatch", GATE, EngramCommandExit::NotFinished);
    let line = turn.record(|record| {
        assert!(record.engram.pending_launch_diagnostics.is_empty());
        assert!(record.engram.carried_unmatched_launches.is_empty());
        let line = record
            .engram
            .pending_source_root_line
            .clone()
            .expect("mismatched command tracking loss is visible");
        assert!(
            line.contains("its command at completion did not match its start"),
            "{line}"
        );
        assert!(
            line.contains(&engram_check_fingerprint(&mark.command))
                && line.contains(&mark.started_at)
        );
        assert!(!line.contains("background full gate") && !line.contains("earned no credit"));
        line
    });
    diagnostic_finish(&turn, "mismatch", GATE, EngramCommandExit::NotFinished);
    assert_eq!(
        turn.record(|record| record.engram.pending_source_root_line.clone()),
        Some(line)
    );
    assert_no_verification_state(&turn);
}

#[test]
fn unknown_or_disagreeing_candidate_directories_do_not_invent_a_marker_root() {
    let (turn, worktree) = named_turn("correction-root-negative");
    turn.record_mut(|record| {
        record.engram.work_binding = None;
        record.engram.shell_directory = Some(EngramShellDirectory {
            runtime: record.runtime.runtime_token(),
            directory: Some(worktree.to_string_lossy().into_owned()),
            pending: None,
            lost_among: Vec::new(),
            lost_unbounded: false,
        });
    });
    start_at(&turn, "disagreement", DETACHED_GATE, None);
    turn.record(|record| {
        assert!(
            record.engram.pending_launch_diagnostics[0].root.is_none(),
            "the workdir and presumed shell identify different worktrees"
        )
    });
    diagnostic_finish(
        &turn,
        "disagreement",
        DETACHED_GATE,
        EngramCommandExit::Code(0),
    );
    turn.record(|record| assert!(record.engram.carried_unmatched_launches.is_empty()));
    assert_no_verification_state(&turn);
}

#[test]
fn disablement_does_not_become_a_command_mismatch_notice() {
    let turn = CheckedTurn::start("correction-disabled-mismatch", false);
    diagnostic_start(&turn, "disabled", DETACHED_GATE);
    turn.record_mut(|record| record.engram.disabled_reason = Some("disabled fixture".to_owned()));
    diagnostic_finish(&turn, "disabled", GATE, EngramCommandExit::NotFinished);
    turn.record(|record| assert!(record.engram.pending_source_root_line.is_none()));
    assert_no_verification_state(&turn);
}
