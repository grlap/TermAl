// Registered launcher notices use real recorder replies and independent run
// artifacts. Ordinary observations never confer native verification credit.
use super::*;

const COMMAND: &str = "node scripts/test-launcher.mjs focused -- node --test scripts/test-launcher.test.mjs";
const KEY: &str = "live-launcher";

struct RegisteredLauncherTurn {
    turn: EOCheckedTurn,
    recorder: SessionRecorder,
    run_id: String,
    directory: PathBuf,
    started: String,
}

impl RegisteredLauncherTurn {
    fn start() -> Self {
        Self::with_turn(EOCheckedTurn::start("registered-launcher-close", true))
    }

    fn with_turn(turn: EOCheckedTurn) -> Self {
        let mut recorder = turn.recorder();
        let cwd = turn.record(|record| record.engram.observed_source_root()
            .map(|root| root.root.clone()));
        recorder.command_started_in(KEY, COMMAND, Some(COMMAND), cwd.as_deref()).unwrap();
        turn.wait_for_snapshots();
        let open = turn.record(|record| record.engram.active_turn_checks.iter()
            .filter(|check| check.end.is_none()).count());
        assert_eq!(open, 1);
        let started = engram_command_started_at();
        let run_id = format!("test-{}", Uuid::new_v4());
        let root = turn.record(|record| record.engram.active_turn_checks.iter()
            .find(|check| check.key == KEY).unwrap().target.root.clone());
        let directory = EngramHost::run_artifact_directory(&root).unwrap().join(&run_id);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("request.json"), serde_json::to_vec(&json!({
            "runId": run_id, "root": root.to_string_lossy(),
            "owner": turn.session_id, "creatorPid": std::process::id(),
            "started": started, "full": false, "detached": false,
            "stages": [{ "name": "focused", "command": "node",
                "args": ["--test", "scripts/test-launcher.test.mjs"] }],
        })).unwrap()).unwrap();
        let fixture = Self { turn, recorder, run_id, directory, started };
        fixture.write_results("running");
        assert_eq!(TestRunDisk::read(&fixture.directory, None).unwrap().state(&|_, _| false),
            TestRunState::Running);
        let ack = fixture.turn.state.register_test_run(&fixture.turn.session_id, RegisterTestRunRequest {
            run_id: fixture.run_id.clone(), run_directory: test_run_display_path(&fixture.directory),
        }).expect("durable registration precedes terminal publication");
        assert!(ack.registered);
        fixture.assert_unindexed();
        fixture
    }

    fn write_results(&self, state: &str) {
        let results = if state == "running" {
            json!({ "runId": self.run_id, "state": state, "started": self.started,
                "pid": std::process::id(), "heartbeat": { "at": test_run_timestamp(), "everyMs": 60_000 },
                "stages": [{ "name": "focused", "state": state }] })
        } else {
            json!({ "runId": self.run_id, "state": state, "started": self.started,
                "ended": test_run_timestamp(), "exitCode": if state == "passed" { 0 } else { 1 },
                "stages": [{ "name": "focused", "state": state, "code": if state == "passed" { 0 } else { 1 } }] })
        };
        fs::write(self.directory.join("results.json"), serde_json::to_vec(&results).unwrap()).unwrap();
    }

    fn background_reply(&mut self) {
        // The provider's real reply carries a locator and explicitly says the
        // process has not ended. No direct artifact-owner injection.
        self.recorder.command_completed_with_exit(KEY, COMMAND,
            &format!("RUN {}", test_run_display_path(&self.directory)),
            CommandStatus::Running, EngramCommandExit::NotFinished).unwrap();
        let (associated, open) = {
            let mut inner = self.turn.state.inner.lock().unwrap();
            let associated = inner.test_run_observations.values().any(|row|
                row.artifact.as_ref().is_some_and(|artifact| artifact.run_id == self.run_id));
            let record = &inner.sessions[inner.find_session_index(&self.turn.session_id).unwrap()];
            let open = record.engram.active_turn_checks.iter().any(|check| check.key == KEY && check.end.is_none());
            // Only the process-local projection is absent at the publication
            // boundary; durable registration and original observation remain.
            inner.test_runs.entries.clear();
            (associated, open)
        };
        assert!(associated);
        assert!(open, "a NotFinished launcher reply cannot end its check");
        self.assert_unindexed();
    }

    fn assert_unindexed(&self) {
        let indexed = self.turn.state.inner.lock().unwrap().test_runs.find(&self.run_id).is_some();
        assert!(!indexed);
    }

    fn line(&self) -> (String, bool) {
        self.turn.record(|record| (
            record.engram.pending_source_root_line.clone().unwrap_or_default(),
            record.engram.unended_check_closures.iter().any(|closed| closed.key == KEY),
        ))
    }

    fn finish(&self) -> (String, bool) {
        let observation = self.turn.finish();
        assert_ordinary_observation_has_no_native_credit(&self.turn, &observation);
        let checks = observation["occurrence"]["observed_checks"].as_array().unwrap();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0]["observed_result"], "indeterminate");
        assert!(checks[0].get("finished_at").is_none());
        self.line()
    }

    fn assert_advice(&self, line: &str) {
        assert!(line.contains(&self.run_id), "{line}");
        assert!(!line.to_ascii_lowercase().contains("run it again"), "{line}");
        assert!(!line.to_ascii_lowercase().contains("run the check again"), "{line}");
    }
}

fn registered_launcher_before_index(passed: bool, correlated: bool) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::start();
    if correlated { fixture.background_reply(); }
    if passed { fixture.write_results("passed"); }
    fixture.assert_unindexed();
    let (line, closed) = fixture.finish();
    fixture.assert_advice(&line);
    if correlated {
        assert!(!closed && !line.contains("closed with outcome Unknown"), "{line}");
        assert!(line.contains(if passed { "passed" } else { "still running" }), "{line}");
    } else {
        assert!(closed && line.contains("root") && line.contains("input"), "{line}");
    }
    let results: Value = serde_json::from_slice(&fs::read(fixture.directory.join("results.json")).unwrap()).unwrap();
    assert_eq!(results["state"], if passed { "passed" } else { "running" });
    assert_eq!(results.get("ended").is_some(), passed);
}

#[test]
fn a_registered_live_launcher_is_pending_at_turn_end_before_the_index_catches_up() {
    registered_launcher_before_index(false, true);
}

#[test]
fn a_registered_passed_launcher_names_its_terminal_result_at_turn_end_before_the_index_catches_up() {
    registered_launcher_before_index(true, true);
}

#[test]
fn a_registered_live_launcher_without_a_locator_is_disclosed_without_rerun_advice() {
    registered_launcher_before_index(false, false);
}

#[test]
fn a_registered_passed_launcher_without_a_locator_is_disclosed_without_rerun_advice() {
    registered_launcher_before_index(true, false);
}

#[test]
fn a_registered_failed_launcher_names_its_terminal_result_without_duplicating_it() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::start();
    fixture.background_reply();
    fixture.write_results("failed");
    let (line, closed) = fixture.finish();
    fixture.assert_advice(&line);
    assert!(line.contains("failed") && !closed, "{line}");
}

#[test]
fn a_registered_terminal_launcher_keeps_its_matching_retained_digest() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for terminal in ["passed", "failed"] {
        let mut fixture = RegisteredLauncherTurn::start();
        fixture.background_reply();
        fixture.write_results(terminal);
        fixture.turn.state.refresh_test_runs();
        let retained = fixture.turn.state.inner.lock().unwrap().test_run_observations.values().any(|row|
            row.artifact.as_ref().is_some_and(|artifact| artifact.run_id == fixture.run_id
                && artifact.terminal_digest.is_some()));
        assert!(retained);
        let (line, closed) = fixture.finish();
        fixture.assert_advice(&line);
        assert!(line.contains(terminal) && !closed, "{line}");
    }
}

#[test]
fn a_registered_launcher_running_card_without_a_nonterminal_exit_cannot_associate_output() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::start();
    fixture.recorder.command_completed_with_exit(KEY, COMMAND,
        &format!("RUN {}", test_run_display_path(&fixture.directory)),
        CommandStatus::Running, EngramCommandExit::Code(0)).unwrap();
    let associated = fixture.turn.state.inner.lock().unwrap().test_run_observations.values()
        .any(|row| row.artifact.is_some());
    assert!(!associated, "a running transcript card alone cannot supply an artifact or end");
    let (line, closed) = fixture.finish();
    fixture.assert_advice(&line);
    assert!(closed && !line.contains("correlated with"), "{line}");
}

#[test]
fn a_non_launcher_running_reply_keeps_its_original_unfinished_observation_unchanged() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = EOCheckedTurn::start("non-launcher-running-reply", true);
    let mut recorder = turn.recorder();
    recorder.command_started(KEY, SIZE_TEST).unwrap();
    turn.wait_for_snapshots();
    let before = turn.state.inner.lock().unwrap().test_run_observations.values().cloned().collect::<Vec<_>>();
    assert_eq!(before.len(), 1);
    recorder.command_completed_with_exit(KEY, SIZE_TEST,
        &format!("RUN {}", test_run_display_path(&turn.root.join("not-a-run"))),
        CommandStatus::Running, EngramCommandExit::NotFinished).unwrap();
    let after = turn.state.inner.lock().unwrap().test_run_observations.values().cloned().collect::<Vec<_>>();
    assert_eq!(after, before, "a non-launcher card still enters no artifact or end owner");
    let open = turn.record(|record| record.engram.active_turn_checks.iter()
        .any(|check| check.key == KEY && check.end.is_none()));
    assert!(open);
    assert!(!EngramHost::is_nonterminal_launcher_reply(SIZE_TEST, Some(EngramCommandExit::NotFinished)));
}

#[test]
fn a_full_launcher_running_reply_preserves_the_prior_native_carry_path() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    const FULL: &str = "node scripts/test-launcher.mjs full";
    for named in [false, true] {
        let turn = if named { EOCheckedTurn::start_named("full-running-control") }
            else { EOCheckedTurn::start("full-running-control", true) };
        let mut recorder = turn.recorder();
        let cwd = turn.record(|record| record.engram.observed_source_root()
            .map(|root| root.root.clone()));
        recorder.command_started_in(KEY, FULL, Some(FULL), cwd.as_deref()).unwrap();
        turn.wait_for_snapshots();
        let before = turn.state.inner.lock().unwrap().test_run_observations.values().cloned().collect::<Vec<_>>();
        assert_eq!(before.len(), 1);
        recorder.command_completed_with_exit(KEY, FULL,
            &format!("RUN {}", test_run_display_path(&turn.root.join("not-a-run"))),
            CommandStatus::Running, EngramCommandExit::NotFinished).unwrap();
        let after = turn.state.inner.lock().unwrap().test_run_observations.values().cloned().collect::<Vec<_>>();
        assert_eq!(after, before, "full Running replies stay outside the launch-result path");
        let open = turn.record(|record| record.engram.active_turn_checks.iter()
            .any(|check| check.key == KEY && check.end.is_none()));
        assert!(open);
        let observation = turn.finish();
        assert_ordinary_observation_has_no_native_credit(&turn, &observation);
        let (running, carried) = turn.record(|record| (
            record.engram.running_codex_tests.iter().any(|test| test.key == KEY),
            record.engram.carried_checks.iter().any(|carry| carry.check.key == KEY && carry.ends_by_report),
        ));
        assert!(running, "the original process remains a possible writer");
        assert_eq!(carried, named, "only the prior named-root native carry is eligible");
        assert!(!EngramHost::is_nonterminal_launcher_reply(FULL, Some(EngramCommandExit::NotFinished)));
    }
}

#[test]
fn a_non_launcher_closure_keeps_its_original_advice_at_each_boundary() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let command = EngramHost::recognized_check(SIZE_TEST).unwrap();
    for boundary in ["turn-end", "next-start"] {
        let turn = EOCheckedTurn::start("non-launcher-closure", true);
        let mut recorder = turn.recorder();
        recorder.command_started(KEY, SIZE_TEST).unwrap();
        turn.wait_for_snapshots();
        if boundary == "turn-end" { let _ = turn.finish(); }
        else { turn.record_mut(EngramHost::turn_prompt_composing); }
        let line = turn.record(|record| record.engram.pending_source_root_line.clone().unwrap_or_default());
        assert!(line.contains("run it again to record it"), "{boundary}: {line}");
        assert!(!line.contains("registered launcher runs"), "{boundary}: {line}");
    }
    for line in [engram_closed_unended_carry_line(&command, "runtime cleared"),
        engram_reported_end_lost_to_restart_line(&command, false),
        engram_reported_end_lost_to_restart_line(&command, true),
        engram_late_end_line(&command, SIZE_TEST, Some(EngramCommandExit::Code(0)))]
    {
        assert!(line.contains("again to record it"), "{line}");
        assert!(!line.contains("registered launcher runs"), "{line}");
    }
}

#[test]
fn a_nonterminal_launcher_reply_preserves_the_closure_for_the_real_end_once() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::start();
    let (_, closed) = fixture.finish();
    assert!(closed);
    fixture.turn.record_mut(|record| record.engram.pending_source_root_line = None);
    let output = format!("RUN {}", test_run_display_path(&fixture.directory));
    fixture.recorder.command_completed_with_exit(KEY, COMMAND, &output,
        CommandStatus::Running, EngramCommandExit::NotFinished).unwrap();
    let (line, closed) = fixture.line();
    assert!(closed && !line.contains("an end ("), "{line}");
    fixture.recorder.command_completed_with_exit(KEY, COMMAND, &output,
        CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
    let (line, closed) = fixture.line();
    assert!(!closed && line.contains("an end (exit code 0)"), "{line}");
    assert!(!line.contains("run the check again"), "{line}");
    fixture.turn.record_mut(|record| record.engram.pending_source_root_line = None);
    fixture.recorder.command_completed_with_exit(KEY, COMMAND, &output,
        CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
    let (line, closed) = fixture.line();
    assert!(!closed && !line.contains("an end ("), "the real end is told exactly once: {line}");
}

#[test]
fn a_carried_launchers_eviction_and_settings_advice_cannot_invite_a_duplicate() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::with_turn(EOCheckedTurn::start_named("carried-launcher-advice"));
    fixture.background_reply();
    let (line, _) = fixture.finish();
    fixture.assert_advice(&line);
    let lines = fixture.turn.record(|record| {
        let carried = record.engram.carried_checks.iter().find(|carry| carry.check.key == KEY).unwrap();
        [carried.refusal_line("carried capacity exceeded"), carried.association_reset_line("settings changed")]
    });
    for line in lines {
        assert!(line.contains("registered launcher runs") && line.contains("rerun only if none"), "{line}");
        assert!(!line.contains("Run the test again") && !line.contains("run it again"), "{line}");
    }
    let turn = EOCheckedTurn::start_named("carried-non-launcher-advice");
    let mut recorder = turn.recorder();
    let cwd = turn.record(|record| record.engram.observed_source_root()
        .map(|root| root.root.clone()));
    recorder.command_started_in(KEY, SIZE_TEST, Some(SIZE_TEST), cwd.as_deref()).unwrap();
    turn.wait_for_snapshots();
    let _ = turn.finish();
    let lines = turn.record(|record| {
        let carried = record.engram.carried_checks.iter().find(|carry| carry.check.key == KEY).unwrap();
        [carried.refusal_line("carried capacity exceeded"), carried.association_reset_line("settings changed")]
    });
    assert!(lines[0].contains("Run the test again once it has ended"));
    assert!(lines[1].contains("run it again once the new settings apply"));
    assert!(lines.iter().all(|line| !line.contains("registered launcher runs")));
}

#[test]
fn a_registered_live_launcher_keeps_codex_writer_bookkeeping_and_successor_overlap() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::start();
    fixture.background_reply();
    let (line, closed) = fixture.finish();
    fixture.assert_advice(&line);
    assert!(!closed);
    let running = fixture.turn.record(|record| record.engram.running_codex_tests.iter()
        .any(|test| test.key == KEY));
    assert!(running, "refusing carry evidence cannot end the process");
    fixture.turn.begin("Run a successor check.");
    let mut recorder = fixture.turn.recorder();
    recorder.command_started("successor", SIZE_TEST).unwrap();
    let overlap = fixture.turn.record(|record| record.engram.active_turn_checks.iter()
        .find(|check| check.key == "successor").map(|check| check.overlap_cause.clone()));
    assert!(overlap.is_some_and(|cause| cause.is_some()), "the original process can still write");
}

#[test]
fn a_registered_live_launcher_in_a_named_root_keeps_native_reported_end_custody() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::with_turn(EOCheckedTurn::start_named("registered-named-carry"));
    fixture.background_reply();
    let (line, closed) = fixture.finish();
    fixture.assert_advice(&line);
    assert!(!closed, "{line}");
    let (running, carried) = fixture.turn.record(|record| (
        record.engram.running_codex_tests.iter().any(|test| test.key == KEY),
        record.engram.carried_checks.iter().any(|carry|
            carry.check.key == KEY && carry.ends_by_report && carry.check.end.is_none()),
    ));
    assert!(running && carried, "registered advice cannot replace native late-end custody");
}

#[test]
fn a_registered_launcher_on_each_terminal_path_keeps_its_exact_run_notice() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for terminal in ["failed", "cancelled", "exit", "marked", "atomic"] {
        let mut fixture = RegisteredLauncherTurn::start();
        fixture.background_reply();
        let (token, generation) = fixture.turn.record(|record|
            (record.runtime.runtime_token().unwrap(), record.active_turn_generation));
        let state = &fixture.turn.state;
        let session = &fixture.turn.session_id;
        match terminal {
            "failed" => state.fail_turn_if_runtime_matches(session, &token, "provider failed").unwrap(),
            "cancelled" => { state.stop_session(session).unwrap(); }
            "exit" => state.handle_runtime_exit_if_matches(session, &token, None).unwrap(),
            "marked" => state.mark_turn_error_if_runtime_matches(session, &token, "provider failed").unwrap(),
            _ => { assert!(state.fail_turn_if_runtime_matches_or_missing(session, &token, generation, "provider failed").unwrap()); }
        }
        let (line, closed) = fixture.line();
        fixture.assert_advice(&line);
        assert!(line.contains("still running") && !closed, "{terminal}: {line}");
    }
}

#[test]
fn a_registered_launcher_in_a_named_root_never_gets_unconditional_rerun_advice_on_terminal_close() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for correlated in [true, false] {
        for terminal in ["failed", "cancelled", "exit", "marked", "atomic"] {
            let mut fixture = RegisteredLauncherTurn::with_turn(EOCheckedTurn::start_named("registered-named-close"));
            if correlated { fixture.background_reply(); }
            let (token, generation) = fixture.turn.record(|record|
                (record.runtime.runtime_token().unwrap(), record.active_turn_generation));
            let state = &fixture.turn.state;
            let session = &fixture.turn.session_id;
            match terminal {
                "failed" => state.fail_turn_if_runtime_matches(session, &token, "provider failed").unwrap(),
                "cancelled" => { state.stop_session(session).unwrap(); }
                "exit" => state.handle_runtime_exit_if_matches(session, &token, None).unwrap(),
                "marked" => state.mark_turn_error_if_runtime_matches(session, &token, "provider failed").unwrap(),
                _ => { assert!(state.fail_turn_if_runtime_matches_or_missing(session, &token, generation, "provider failed").unwrap()); }
            }
            let (line, _) = fixture.line();
            fixture.assert_advice(&line);
            if correlated {
                assert!(line.contains("still running"), "{terminal}: {line}");
            } else {
                assert!(line.contains("registered") && line.contains("root") && line.contains("input"), "{terminal}: {line}");
            }
            let results: Value = serde_json::from_slice(&fs::read(fixture.directory.join("results.json")).unwrap()).unwrap();
            assert_eq!(results["state"], "running", "a provider clear cannot end the launcher run");
        }
    }
}

#[test]
fn a_registered_launcher_runtime_gone_restart_and_late_end_wording_never_invites_a_duplicate() {
    let command = EngramHost::recognized_check(COMMAND).unwrap();
    for line in [engram_closed_unended_carry_line(&command, "runtime cleared"),
        engram_reported_end_lost_to_restart_line(&command, false),
        engram_reported_end_lost_to_restart_line(&command, true),
        engram_late_end_line(&command, COMMAND, Some(EngramCommandExit::Code(0)))]
    {
        assert!(line.contains("registered launcher runs") && line.contains("rerun only if none"), "{line}");
        assert!(!line.contains("run it again") && !line.contains("run the check again"), "{line}");
    }
}

#[test]
fn a_registered_launcher_with_a_dead_writer_keeps_honest_unknown_closure() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::start();
    fixture.background_reply();
    // A reaped child supplies a real, inspectable dead pid on every platform.
    // An out-of-range pid cannot establish process death on Unix.
    let dead_writer = test_exit_success_child();
    fs::write(fixture.directory.join("results.json"), serde_json::to_vec(&json!({
        "runId": fixture.run_id, "state": "running", "started": fixture.started,
        "pid": dead_writer.id(), "heartbeat": { "at": "1970-01-01T00:00:00Z", "everyMs": 1000 },
        "stages": [{ "name": "focused", "state": "running" }],
    })).unwrap()).unwrap();
    assert_eq!(TestRunDisk::read(&fixture.directory, None).unwrap()
        .state(&test_run_process_writer_may_be_alive), TestRunState::Unknown);
    let (line, closed) = fixture.finish();
    fixture.assert_advice(&line);
    assert!(closed && !line.contains("correlated with"), "{line}");
}

#[test]
fn a_registered_launcher_with_a_line_key_cannot_claim_exact_correlation() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::start();
    fixture.background_reply();
    fixture.turn.record_mut(|record| record.engram.active_turn_checks.iter_mut()
        .find(|check| check.key == KEY).unwrap().keyed_by_line = true);
    let prepared = fixture.turn.state.prepare_registered_check_notices(&fixture.turn.session_id);
    assert_eq!(prepared.len(), 1);
    assert!(prepared[0].observation.is_none());
    let (line, closed) = fixture.finish();
    fixture.assert_advice(&line);
    assert!(closed && !line.contains("correlated with"), "{line}");
}

#[test]
fn a_registered_launcher_with_a_replaced_request_keeps_honest_unknown_closure() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::start();
    fixture.background_reply();
    let path = fixture.directory.join("request.json");
    let mut request: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    request["owner"] = json!("another-session");
    fs::write(path, serde_json::to_vec(&request).unwrap()).unwrap();
    let (line, closed) = fixture.finish();
    fixture.assert_advice(&line);
    assert!(closed && !line.contains("correlated with"), "{line}");
}

#[test]
fn a_registered_launcher_with_a_changed_terminal_digest_keeps_honest_unknown_closure() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::start();
    fixture.background_reply();
    fixture.write_results("passed");
    fixture.turn.state.refresh_test_runs();
    let retained = fixture.turn.state.inner.lock().unwrap().test_run_observations.values().any(|row|
        row.artifact.as_ref().is_some_and(|artifact| artifact.terminal_digest.is_some()));
    assert!(retained, "the existing owner retained the exact terminal digest");
    fixture.write_results("failed");
    let (line, closed) = fixture.finish();
    fixture.assert_advice(&line);
    assert!(closed && !line.contains("correlated with"), "{line}");
}

#[test]
fn a_registered_launcher_locator_cannot_be_borrowed_by_a_same_key_successor() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = RegisteredLauncherTurn::start();
    fixture.background_reply();
    fixture.write_results("passed");
    fixture.recorder.command_completed_with_exit(KEY, COMMAND,
        &format!("RUN {}", test_run_display_path(&fixture.directory)),
        CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
    fixture.recorder.command_started(KEY, COMMAND).unwrap();
    fixture.turn.wait_for_snapshots();
    let prepared = fixture.turn.state.prepare_registered_check_notices(&fixture.turn.session_id);
    assert_eq!(prepared.len(), 2, "preparation retains the completed check and its successor");
    let completed = prepared.iter().find(|notice| notice.check.end.is_some()).unwrap();
    let successor = prepared.iter().find(|notice| notice.check.end.is_none()).unwrap();
    assert_ne!(completed.check.sequence, successor.check.sequence);
    assert!(completed.observation.is_some(), "the original invocation retains its locator");
    assert!(successor.observation.is_none(), "the previous start's locator is not this invocation's");
    let _ = fixture.turn.finish();
    let (line, closed) = fixture.line();
    fixture.assert_advice(&line);
    assert!(closed && !line.contains("correlated with"), "{line}");
}

#[test]
fn a_registered_launcher_preparation_rejects_changed_generation_runtime_and_owner_rows() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for change in ["generation", "runtime", "registration", "observation"] {
        let mut fixture = RegisteredLauncherTurn::start();
        fixture.background_reply();
        let prepared = fixture.turn.state.prepare_registered_check_notices(&fixture.turn.session_id);
        assert_eq!(prepared.len(), 1);
        let (lines, correlated) = {
            let mut inner = fixture.turn.state.inner.lock().unwrap();
            let index = inner.find_session_index(&fixture.turn.session_id).unwrap();
            match change {
                "generation" => inner.sessions[index].active_turn_generation += 1,
                "runtime" => inner.sessions[index].runtime = SessionRuntime::None,
                "registration" => { inner.test_run_registrations.remove(&fixture.run_id); }
                _ => { inner.test_run_observations.remove(&prepared[0].observation.as_ref().unwrap().execution_id); }
            }
            registered_check_notices_locked(&mut inner, index, &prepared)
        };
        assert!(correlated.is_empty(), "{change}");
        assert!(!lines.iter().any(|line| line.contains("correlated with")), "{change}");
    }
}
