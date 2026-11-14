// Recorder ordering witnesses; synthetic run artifacts never launch a gate.
use super::*;

fn delayed_retirement_cannot_lend_run(
    missing_trace: bool,
    published_before_retirement: bool,
    saved_eligible_carry: bool,
) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = ordinary_carried_turn("delayed-location-retirement");
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
    let mark = begin_unmeasured_launch(&turn, &worktree, &mut recorder, "first-uncredited");
    if saved_eligible_carry {
        // Unit seam: defer only the existing factual window publication.
        // The original execution is real and remains immutable; no gate,
        // source owner or pending diagnostic is fabricated.
        turn.record_mut(|record| {
            record
                .engram
                .carried_unmatched_launches
                .retain(|(root, at)| {
                    *root != engram_path_key(&worktree) || at != launch_start(&mark)
                })
        });
    }
    turn.record_mut(|record| {
        record.engram.work_binding = binding;
        record.engram.unadmitted_active_turn = trace;
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
            .started_at = launch_start(&mark).to_owned();
    });
    // Unlike the earlier retirement tests, the eligible carry exists before
    // the earlier unmeasured command reports its changed completion.
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
        assert_eq!(
            record.engram.carried_checks[0].ambiguous,
            !saved_eligible_carry
        );
    });
    assert_launch_identity(&turn, &mark);
    let earlier_at = (engram_parse_time(launch_start(&mark)).unwrap()
        - chrono::Duration::seconds(1))
    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let runs = review_runs(&worktree);
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
    recorder
        .command_completed_with_exit(
            "first-uncredited",
            &format!("{DETACHED_GATE} --notify session-other"),
            "STARTED",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    if saved_eligible_carry {
        // Publish the captured original root/time through the genuine
        // current exclusion owner after the real poll saved a match.
        turn.record_mut(|record| {
            engram_note_unmatched_launch(record, &worktree, launch_start(&mark))
        });
    }
    assert_launch_identity(&turn, &mark);
    turn.record(|record| {
        assert!(
            record
                .engram
                .carried_unmatched_launches
                .contains(&(engram_path_key(&worktree), launch_start(&mark).to_owned()))
        );
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
    write_run(
        &runs,
        "test-earlier-uncredited",
        &cwd,
        &earlier_at,
        "passed",
        &"f".repeat(64),
    );
    assert_carried_ambiguity(&turn);
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    assert_carried_ambiguity(&turn);
}

#[test]
fn original_location_window_blocks_a_run_published_after_completion() {
    for missing_trace in [false, true] {
        delayed_retirement_cannot_lend_run(missing_trace, false, false);
    }
}

#[test]
fn early_location_window_blocks_a_run_published_before_completion() {
    for missing_trace in [false, true] {
        delayed_retirement_cannot_lend_run(missing_trace, true, false);
    }
}

#[test]
fn late_factual_location_publication_invalidates_an_existing_run_match() {
    delayed_retirement_cannot_lend_run(false, true, true);
}

#[test]
fn retirement_during_an_in_flight_artifact_read_keeps_the_saved_match_ambiguous() {
    let (turn, worktree) = ordinary_carried_turn("retirement-in-flight-artifact");
    ordinary_launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let started = turn.record(|record| record.engram.carried_checks[0].check.started_at.clone());
    let (paused, release) = install_test_engram_carried_poll_pause(&turn.state);
    let state = turn.state.clone();
    let poll = std::thread::spawn(move || state.poll_engram_carried_runs());
    let inspected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::tests::phase_sync::receive(&paused, "the real artifact read pauses before install");
        turn.record_mut(|record| engram_note_unmatched_launch(record, &worktree, &started));
        turn.record(|record| assert!(record.engram.carried_checks[0].ambiguous));
    }));
    let _ = release.send(());
    poll.join()
        .expect("the artifact read always releases and joins");
    if let Err(panic) = inspected {
        std::panic::resume_unwind(panic);
    }
    assert_retired_run(&turn, &run);
    let used = turn.record(|record| record.engram.carried_consumed_runs.clone());
    let (found, _, _) = engram_find_carried_run(
        &turn.session_id,
        &worktree,
        &started,
        &used,
        &Default::default(),
    );
    assert!(
        found.is_none(),
        "a completed ambiguous observation cannot lend its run"
    );
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn later_distant_retirement_keeps_the_exact_older_terminal_artifact() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for seconds in [30, 121, 1800] {
        let (turn, worktree) = ordinary_carried_turn(&format!("distant-retirement-{seconds}"));
        ordinary_launch_gate(&turn, &worktree, false);
        let run = finish_run(&worktree, "passed", &"f".repeat(64));
        let started =
            turn.record(|record| record.engram.carried_checks[0].check.started_at.clone());
        turn.state.poll_engram_carried_runs();
        turn.record_mut(|record| {
            let later = (engram_parse_time(&started).unwrap() + chrono::Duration::seconds(seconds))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            engram_note_unmatched_launch(record, &worktree, &later);
        });
        assert_carried_artifact_custody(&turn, &run, true);
    }
}

#[test]
fn medium_repair_nearby_later_retirement_still_refuses_confusable_run() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = ordinary_carried_turn("nearby-retirement");
    ordinary_launch_gate(&turn, &worktree, false);
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
    assert_carried_ambiguity(&turn);
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}

#[test]
fn medium_repair_distant_later_retirement_keeps_running_gate_eligible() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = ordinary_carried_turn("distant-retirement-running");
    ordinary_launch_gate(&turn, &worktree, false);
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
    assert_carried_artifact_custody(&turn, &run, true);
    assert!(run.exists());
}

#[test]
fn a_later_ambiguous_launch_cannot_give_a_retired_artifact_a_credit_obligation() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = ordinary_carried_turn("ambiguous-restart-risk");
    ordinary_launch_gate(&turn, &worktree, true);
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
        let registration = turn
            .state
            .test_run_summaries(None)
            .into_iter()
            .find(|row| row.run_id == "test-carried")
            .expect("the exact artifact")
            .registration;
        (
            registration.restart_would_void_evidence,
            registration.restart_evidence_reason,
        )
    };
    let (risk, reason) = view();
    assert_eq!(
        risk, None,
        "artifact custody alone establishes no native evidence obligation"
    );
    assert!(
        !reason.contains("Exact carried run is awaiting host verification settlement"),
        "ordinary artifacts never promise automatic native settlement"
    );
    launch_detached(&turn, &worktree, "later-ambiguous");
    turn.record_mut(|record| record.engram.carried_checks[0].ambiguous = true);
    let (risk, reason) = view();
    assert_eq!(risk, None);
    assert!(!reason.contains("Exact carried run is awaiting host verification settlement"));
    assert_carried_ambiguity(&turn);
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
}
