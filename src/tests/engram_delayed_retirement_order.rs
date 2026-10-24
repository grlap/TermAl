// Recorder ordering witnesses; synthetic run artifacts never launch a gate.
use super::*;

fn delayed_retirement_cannot_lend_run(
    missing_grant: bool,
    published_before_retirement: bool,
    saved_eligible_carry: bool,
    during_settlement: bool,
) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = named_turn("delayed-diagnostic-retirement");
    let (binding, grant) = turn.record(|record| {
        (
            record.engram.work_binding.clone(),
            record.engram.active_grant_id.clone(),
        )
    });
    turn.record_mut(|record| {
        if missing_grant {
            record.engram.active_grant_id = None;
        } else {
            record.engram.work_binding = None;
        }
    });
    let mut recorder = turn.recorder();
    let mark = begin_diagnostic(&turn, &worktree, &mut recorder, "first-uncredited");
    turn.record_mut(|record| {
        record.engram.work_binding = binding;
        record.engram.active_grant_id = grant;
    });
    let cwd = fs::canonicalize(&worktree).unwrap();
    recorder
        .command_started_in(
            "second-gate",
            DETACHED_GATE,
            Some(DETACHED_GATE),
            Some(&cwd.to_string_lossy()),
        )
        .unwrap();
    turn.wait_for_snapshots();
    turn.record_mut(|record| {
        record
            .engram
            .active_turn_checks
            .iter_mut()
            .find(|check| check.key == "second-gate" && check.end.is_none())
            .unwrap()
            .started_at = mark.started_at.clone();
    });
    // Unlike the earlier retirement tests, the eligible carry exists before
    // the missing-authority command reports its changed completion.
    recorder
        .command_completed_with_exit(
            "second-gate",
            DETACHED_GATE,
            "STARTED",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    turn.wait_for_snapshots();
    turn.record(|record| {
        assert_eq!(record.engram.carried_checks.len(), 1);
        assert!(
            record.engram.carried_checks[0].fence.is_none(),
            "no unrelated fence"
        );
        assert!(record
            .engram
            .pending_launch_diagnostics
            .iter()
            .any(|pending| pending.key == "first-uncredited"));
    });
    if saved_eligible_carry {
        // Exercise retirement independently of the admission-time guard:
        // pin the already-saved eligibility that the old code produced.
        turn.record_mut(|record| record.engram.carried_checks[0].ambiguous = false);
    }
    let earlier_at = (engram_parse_time(&mark.started_at).unwrap() - chrono::Duration::seconds(1))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let runs = review_runs(&worktree);
    let mut closers = Closers::default();
    let mut held_settlement = None;
    if published_before_retirement {
        let run = publish_running_run(&runs, &cwd, &earlier_at);
        turn.state.poll_engram_carried_runs();
        turn.record(|record| {
            let carry = &record.engram.carried_checks[0];
            if !carry.ambiguous {
                assert_eq!(
                    carry.run_directory.as_ref(),
                    Some(&run),
                    "the unprotected witness must reach actual run borrowing"
                );
            }
        });
    }
    if during_settlement {
        write_run(
            &runs,
            "test-earlier-uncredited",
            &cwd,
            &earlier_at,
            "passed",
            &"f".repeat(64),
        );
        turn.state.poll_engram_carried_runs();
        let hold = install_test_engram_settlement_hold(&cwd, false);
        closers.spawn(&turn);
        hold.wait_taken(std::time::Instant::now() + DEADLOCK_GUARD);
        held_settlement = Some(hold);
    }
    recorder
        .command_completed_with_exit(
            "first-uncredited",
            &format!("{DETACHED_GATE} --notify session-other"),
            "STARTED",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    turn.record(|record| {
        assert!(record.engram.pending_launch_diagnostics.is_empty());
        assert!(record.engram.carried_unmatched_launches.contains(&(
            engram_path_key(mark.root.as_ref().unwrap()),
            mark.started_at.clone()
        )));
        if saved_eligible_carry {
            assert!(
                record.engram.carried_checks[0].ambiguous,
                "late retirement must revoke the saved eligible run match"
            );
        }
    });
    if !published_before_retirement {
        publish_running_run(&runs, &cwd, &earlier_at);
        turn.state.poll_engram_carried_runs();
    }
    let checkpoint = if let Some(hold) = held_settlement {
        hold.release();
        closers.join(0);
        checkpoint_requests(&turn)
            .into_iter()
            .last()
            .expect("the closer sent a checkpoint")
    } else {
        write_run(
            &runs,
            "test-earlier-uncredited",
            &cwd,
            &earlier_at,
            "passed",
            &"f".repeat(64),
        );
        turn.finish()
    };
    assert_refused(&turn, &checkpoint, "another background gate launched");
}

#[test]
fn medium_repair_delayed_retirement_blocks_later_published_run() {
    for missing_grant in [false, true] {
        delayed_retirement_cannot_lend_run(missing_grant, false, false, false);
    }
}

#[test]
fn medium_repair_delayed_retirement_blocks_already_matched_running_run() {
    for missing_grant in [false, true] {
        delayed_retirement_cannot_lend_run(missing_grant, true, false, false);
    }
}

#[test]
fn medium_repair_new_diagnostic_retirement_invalidates_an_existing_run_match() {
    delayed_retirement_cannot_lend_run(false, true, true, false);
}

#[test]
fn medium_repair_retirement_after_settlement_copy_cannot_publish_credit() {
    delayed_retirement_cannot_lend_run(false, true, true, true);
}

#[test]
fn medium_repair_later_distant_retirement_keeps_older_terminal_credit() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for seconds in [30, 121, 1800] {
        let (turn, worktree) = named_turn(&format!("distant-retirement-{seconds}"));
        launch_gate(&turn, &worktree, false);
        let run = finish_run(&worktree, "passed", &"f".repeat(64));
        turn.state.poll_engram_carried_runs();
        turn.record_mut(|record| {
            let carry = &record.engram.carried_checks[0];
            assert!(carry.ended() && carry.fence.is_none() && !carry.ambiguous);
            let later = (engram_parse_time(&carry.check.started_at).unwrap()
                + chrono::Duration::seconds(seconds))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            engram_note_unmatched_launch(record, &worktree, &later);
        });
        assert_credited(&turn, &turn.finish(), &run, "succeeded");
    }
}

#[test]
fn medium_repair_nearby_later_retirement_still_refuses_confusable_run() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = named_turn("nearby-retirement");
    launch_gate(&turn, &worktree, false);
    // Pin a coherent synthetic timeline: the later launch and its running
    // artifact are still before the current clock, with no terminal read.
    let started = (chrono::Utc::now() - chrono::Duration::seconds(60))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    turn.record_mut(|record| record.engram.carried_checks[0].check.started_at = started.clone());
    let later = (engram_parse_time(&started).unwrap() + chrono::Duration::seconds(30))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let cwd = fs::canonicalize(&worktree).unwrap();
    let runs = review_runs(&worktree);
    publish_running_run(&runs, &cwd, &later);
    turn.state.poll_engram_carried_runs();
    turn.record_mut(|record| engram_note_unmatched_launch(record, &worktree, &later));
    write_run(
        &runs,
        "test-earlier-uncredited",
        &cwd,
        &later,
        "passed",
        &"f".repeat(64),
    );
    assert_refused(&turn, &turn.finish(), "another background gate launched");
}

#[test]
fn medium_repair_distant_later_retirement_keeps_running_gate_eligible() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = named_turn("distant-retirement-running");
    launch_gate(&turn, &worktree, false);
    let started = turn.record(|record| record.engram.carried_checks[0].check.started_at.clone());
    let later = (engram_parse_time(&started).unwrap() + chrono::Duration::seconds(121))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let cwd = fs::canonicalize(&worktree).unwrap();
    let runs = review_runs(&worktree);
    let run = publish_running_run(&runs, &cwd, &started);
    turn.state.poll_engram_carried_runs();
    turn.record(|record| assert!(!record.engram.carried_checks[0].ended()));
    turn.record_mut(|record| engram_note_unmatched_launch(record, &worktree, &later));
    write_run(
        &runs,
        "test-earlier-uncredited",
        &cwd,
        &started,
        "passed",
        &"f".repeat(64),
    );
    // This artifact has a different synthetic id from assert_credited's
    // standard fixture, so follow its exact evidence producer directly.
    let checkpoint = turn.finish();
    let rows = checkpoint["verification_evidence"]
        .as_array()
        .expect("gate evidence");
    assert_eq!(rows.len(), 1, "{checkpoint:#}");
    let id = &rows[0]["producer_observation"]["observation_id"];
    let producer = checkpoint["observations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| &row["observation_id"] == id)
        .expect("exact producer");
    assert_eq!(producer["outcome"], "succeeded", "{checkpoint:#}");
    assert!(rows[0]["summary"]
        .as_str()
        .unwrap()
        .contains("test-earlier-uncredited"));
    assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
    assert!(run.exists());
}

#[test]
fn medium_repair_ambiguous_saved_match_is_not_described_as_pending_credit() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = named_turn("ambiguous-restart-risk");
    launch_gate(&turn, &worktree, true);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let path = run.join("request.json");
    let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    request["owner"] = json!(turn.session_id);
    request["detached"] = json!(true);
    fs::write(path, serde_json::to_vec(&request).unwrap()).unwrap();
    turn.state.poll_engram_carried_runs();
    let view = || {
        turn.state
            .refresh_test_runs_with(&|_, _| false, &|inner, event| {
                turn.state.publish_delta_locked(inner, event.clone())
            });
        turn.state
            .test_run_summaries(None)
            .into_iter()
            .find(|row| row.run_id == "test-carried")
            .expect("the exact artifact")
            .registration
            .restart_evidence_reason
    };
    assert!(view().contains("Exact carried run is awaiting host verification settlement"));
    turn.record_mut(|record| record.engram.carried_checks[0].ambiguous = true);
    assert!(!view().contains("Exact carried run is awaiting host verification settlement"));
    assert_refused(&turn, &turn.finish(), "another background gate launched");
}
