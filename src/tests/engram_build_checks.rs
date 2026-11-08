// Build recognition, diagnostic captures and uncredited ordinary observations. Does not own
// disposable producer admission or the deferred real-task completion matrix.
// New child of engram_turn_checks.rs, reusing its actual recorder fixture.
use super::*;

#[path = "engram_build_outcomes.rs"]
mod outcomes;

#[path = "engram_focused_intervals.rs"]
mod intervals;

fn complete_command(
    turn: &EOCheckedTurn,
    key: &str,
    command: &str,
    exit: EngramCommandExit,
    output: &str,
) {
    let mut recorder = turn.recorder();
    let cwd = turn.root.to_string_lossy();
    recorder
        .command_started_in(key, command, Some(command), Some(&cwd))
        .unwrap();
    turn.wait_for_snapshots();
    recorder
        .command_completed_with_exit(
            key,
            command,
            output,
            if matches!(exit, EngramCommandExit::Code(code) if code != 0) {
                CommandStatus::Error
            } else {
                CommandStatus::Success
            },
            exit,
        )
        .unwrap();
    turn.wait_for_snapshots();
}

// Closing source measurement and check facts have separate meanings: a
// matching sighting is not proof that the build ran against that revision.
fn one_build(request: &Value, revision: &str, outcome: &str) {
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1, "one build fact, never dual recording: {request:#}");
    assert_eq!(checks[0]["check_kind"], "build", "{request:#}");
    assert_eq!(checks[0]["observed_result"], outcome, "{request:#}");
    assert!(checks[0].get("source_basis").is_none(), "no automatic source verification");
    assert_eq!(request["occurrence"]["source_change"]["sighting"]["source_basis"]["source_revision"], revision);
    assert!(request.get("verification_evidence").is_none());
    assert!(request.get("environment_evidence").is_none());
}

fn diagnostic(turn: &EOCheckedTurn) -> EngramTurnCheck {
    turn.record(|record| record.engram.active_turn_checks.last().unwrap().clone())
}

fn diagnostic_refs(turn: &EOCheckedTurn) -> Vec<String> {
    let check = diagnostic(turn);
    let end = check.end.as_ref().unwrap();
    engram_check_refs(&check.command, end.exit, &end.result_lines)
}

fn diagnostic_summary(turn: &EOCheckedTurn) -> String {
    let check = diagnostic(turn);
    let end = check.end.as_ref().unwrap();
    end.build_diagnostic.as_ref().map_or_else(
        || engram_check_summary(&check.command, end.exit, &end.result_lines),
        |diagnostic| diagnostic.summary(&check.command))
}

fn finish_observation(turn: &EOCheckedTurn) -> Value {
    let request = turn.finish();
    assert_ordinary_observation_has_no_native_credit(turn, &request);
    request
}

fn assert_native_capture(turn: &EOCheckedTurn, revision: &str) {
    let check = diagnostic(turn);
    let (root, _) = content_revision(&turn.root).unwrap();
    for capture in [&check.start_basis, &check.end.as_ref().unwrap().end_basis] {
        let basis = capture.wait_until(std::time::Instant::now() + DEADLOCK_GUARD)
            .expect("the real diagnostic capture settles").expect("the source was measured");
        assert_eq!(basis.workspace_id, root.to_string_lossy());
        assert_eq!(basis.source_revision, revision);
    }
    assert_eq!(check.toolchain.wait_until(std::time::Instant::now() + DEADLOCK_GUARD)
        .flatten().as_deref(), Some(FIXTURE_TOOLCHAIN));
}

#[test]
fn engram_build_native_commands_are_recognized_without_substring_matching() {
    for command in [
        "cargo build",
        "cargo +stable build --release",
        "cargo build --offline --bin termal",
    ] {
        let check = engram_check_command(command)
            .unwrap_or_else(|| panic!("actual Build missing: {command}"));
        assert_eq!(check.program, "cargo");
        assert!(check.simple);
    }
}

#[test]
fn engram_build_native_pass_records_source_and_environment_without_test_output() {
    let turn = EOCheckedTurn::start("build-native-pass", true);
    let revision = turn.revision();
    complete_command(
        &turn,
        "build",
        "cargo build",
        EngramCommandExit::Code(0),
        "",
    );
    assert_native_capture(&turn, &revision);
    let request = finish_observation(&turn);
    one_build(&request, &revision, "passed");
    assert_eq!(
        diagnostic_refs(&turn),
        ["command:cargo build", "exit:0"]
    );
    let check = diagnostic(&turn);
    let end = check.end.as_ref().unwrap();
    assert!(
        engram_check_summary(&check.command, end.exit, &end.result_lines).contains("exited 0")
    );
}

#[test]
fn engram_build_native_failure_records_failed_producer_without_test_output() {
    let turn = EOCheckedTurn::start("build-native-fail", true);
    let revision = turn.revision();
    complete_command(
        &turn,
        "build",
        "cargo build",
        EngramCommandExit::Code(101),
        "error: could not compile fixture",
    );
    let request = finish_observation(&turn);
    one_build(&request, &revision, "failed");
    assert!(
        diagnostic_refs(&turn).contains(&"exit:101".to_owned())
    );
}

#[test]
fn engram_build_and_test_commands_each_emit_only_their_own_kind() {
    let turn = EOCheckedTurn::start("build-and-test", true);
    complete_command(
        &turn,
        "build",
        "cargo build",
        EngramCommandExit::Code(0),
        "",
    );
    complete_command(
        &turn,
        "test",
        "cargo test",
        EngramCommandExit::Code(0),
        "test result: ok. 1 passed; 0 failed",
    );
    let request = finish_observation(&turn);
    let rows = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        2,
        "two commands, each exactly once: {request:#}"
    );
    assert_eq!(rows[0]["check_kind"], "build");
    assert_eq!(rows[1]["check_kind"], "test");
    assert_ne!(
        rows[0]["host_check_id"],
        rows[1]["host_check_id"]
    );
}

#[test]
fn engram_build_generic_reported_success_is_unknown_even_with_passing_test_text() {
    let turn = EOCheckedTurn::start("build-no-native-exit", true);
    let revision = turn.revision();
    complete_command(
        &turn,
        "build",
        "cargo build",
        EngramCommandExit::ReportedSuccess,
        "test result: ok. 8 passed; 0 failed",
    );
    let request = finish_observation(&turn);
    one_build(&request, &revision, "indeterminate");
    assert!(
        !diagnostic_refs(&turn)
            .iter()
            .any(|value| value.starts_with("exit:"))
    );
}

#[test]
fn engram_build_one_call_failure_is_not_rescued_by_test_output() {
    let turn = EOCheckedTurn::start("build-pushd-failure", true);
    let revision = turn.revision();
    let command = format!("pushd \"{}\" && cargo build", turn.root.display());
    complete_command(
        &turn,
        "build",
        &command,
        EngramCommandExit::Code(1),
        "test result: FAILED. 0 passed; 1 failed",
    );
    let request = finish_observation(&turn);
    one_build(&request, &revision, "indeterminate");
    assert!(
        !diagnostic_refs(&turn).contains(&"exit:1".to_owned())
    );
}

#[test]
fn engram_build_non_build_help_and_planning_commands_have_no_build_check() {
    for command in [
        "echo cargo build",
        "rg build src",
        "cargo check",
        "cargo test --no-run",
        "cargo build --help",
        "cargo build -h",
        "cargo build --version",
        "cargo build --build-plan",
        "cargo build --unit-graph",
        "cargo build --dry-run",
        "npm run build",
        "node scripts/test-launcher.mjs focused -- echo cargo build",
        "node scripts/test-launcher.mjs focused --detach -- cargo build",
    ] {
        assert!(
            engram_check_command(command).is_none(),
            "not a genuine supported Build: {command}"
        );
    }
}

#[test]
fn engram_build_preserves_test_kind_and_its_positive_count_requirement() {
    let turn = EOCheckedTurn::start("build-test-control", true);
    complete_command(
        &turn,
        "test",
        "cargo test",
        EngramCommandExit::Code(0),
        "test result: ok. 1 passed; 0 failed",
    );
    let request = finish_observation(&turn);
    assert_eq!(
        request["occurrence"]["observed_checks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(request["occurrence"]["observed_checks"][0]["check_kind"], "test");
    assert!(diagnostic(&turn).end.as_ref().unwrap().showed_passing_tests);
    let empty = EOCheckedTurn::start("build-empty-test-control", true);
    complete_command(
        &empty,
        "test",
        "cargo test",
        EngramCommandExit::Code(0),
        "test result: ok. 0 passed; 0 failed",
    );
    assert!(!diagnostic(&empty).end.as_ref().unwrap().showed_passing_tests,
        "zero-test output never supplies positive diagnostic test counts");
    let empty_request = finish_observation(&empty);
    assert_eq!(empty_request["occurrence"]["observed_checks"][0]["check_kind"], "test");
    assert!(empty_request.get("verification_evidence").is_none());
}

#[test]
fn engram_build_changed_source_cannot_acquire_passing_credit() {
    let turn = EOCheckedTurn::start("build-changed-source", true);
    let mut recorder = turn.recorder();
    recorder.command_started("build", "cargo build").unwrap();
    assert_eq!(
        turn.record(|record| record.engram.active_turn_checks.len()),
        1,
        "the negative control must actually exercise a recognized Build"
    );
    turn.wait_for_snapshots();
    turn.change_readme("changed during the build\n");
    recorder
        .command_completed_with_exit(
            "build",
            "cargo build",
            "",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    turn.wait_for_snapshots();
    let check = diagnostic(&turn);
    let start = check.start_basis.wait_until(std::time::Instant::now() + DEADLOCK_GUARD).unwrap().unwrap();
    let end = check.end.as_ref().unwrap().end_basis
        .wait_until(std::time::Instant::now() + DEADLOCK_GUARD).unwrap().unwrap();
    assert_ne!(start.source_revision, end.source_revision);
    let request = finish_observation(&turn);
    assert!(request.get("verification_evidence").is_none());
    assert!(request["occurrence"]["observed_checks"][0].get("source_basis").is_none(),
        "an observed successful exit cannot verify source that changed during the build");
}

#[test]
fn engram_build_one_call_native_success_is_build_not_test() {
    let turn = EOCheckedTurn::start("build-pushd-success", true);
    let revision = turn.revision();
    let command = format!("pushd \"{}\" && cargo build", turn.root.display());
    complete_command(&turn, "build", &command, EngramCommandExit::Code(0), "");
    one_build(&finish_observation(&turn), &revision, "passed");
}

const FOCUSED_BUILD: &str = "node scripts/test-launcher.mjs focused -- cargo build --offline";

/// Synthetic local launcher records, not execution evidence for the tracker.
/// Their ownership and interval are captured from the real running check.
struct FocusedBuildFixture {
    turn: EOCheckedTurn,
    check: EngramTurnCheck,
    directory: PathBuf,
    request: Value,
    results: Value,
    input: Value,
    output: String,
}

impl FocusedBuildFixture {
    fn new(label: &str, code: i64) -> Self {
        let turn = EOCheckedTurn::start(label, true);
        turn.recorder()
            .command_started_in(
                "build",
                FOCUSED_BUILD,
                Some(FOCUSED_BUILD),
                Some(&turn.root.to_string_lossy()),
            )
            .unwrap();
        turn.wait_for_snapshots();
        let check = turn.record(|record| record.engram.active_turn_checks[0].clone());
        let run = format!("test-{}", uuid::Uuid::new_v4());
        let directory = engram_git_run_directory(&turn.root).unwrap().join(&run);
        fs::create_dir_all(&directory).unwrap();
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let source_id = launcher_source_id_fixture();
        let state = if code == 0 { "passed" } else { "failed" };
        let request = json!({"runId":run,"root":turn.root,"owner":turn.session_id,
            "started":now,"full":false,"detached":false,"sourceId":source_id,
            "stages":[{"name":"focused","command":"cargo","args":["build","--offline"]}]});
        let results = json!({"runId":run,"owner":turn.session_id,"started":now,"ended":now,
            "state":state,"exitCode":code,"sourceIdBefore":source_id,
            "sourceIdAfter":source_id,"stages":[{"name":"focused","state":state,
                "code":code,"started":now,"ended":now,"cwd":turn.root,
                "command":[turn.root.join("cargo.exe"),"build","--offline"]}]});
        let input = json!({"root":turn.root,"sourceId":source_id});
        let output = format!(
            "{} {run} exit={code}\nfocused: {state}\nresults: {}\n",
            if code == 0 { "PASS" } else { "FAIL" },
            directory.join("results.json").display()
        );
        Self {
            turn,
            check,
            directory,
            request,
            results,
            input,
            output,
        }
    }

    fn save(&self) {
        for (name, value) in [
            ("request.json", &self.request),
            ("results.json", &self.results),
            ("input.json", &self.input),
        ] {
            fs::write(
                self.directory.join(name),
                serde_json::to_vec(value).unwrap(),
            )
            .unwrap();
        }
    }

    fn finish(&self, exit: EngramCommandExit) -> Value {
        self.save();
        self.turn
            .recorder()
            .command_completed_with_exit(
                "build",
                FOCUSED_BUILD,
                &self.output,
                if matches!(exit, EngramCommandExit::Code(code) if code != 0) {
                    CommandStatus::Error
                } else {
                    CommandStatus::Success
                },
                exit,
            )
            .unwrap();
        self.turn.wait_for_snapshots();
        finish_observation(&self.turn)
    }
}

#[test]
fn engram_build_focused_native_stage_records_pass_and_failure_without_test_counts() {
    for code in [0, 101] {
        let fixture = FocusedBuildFixture::new(&format!("build-focused-{code}"), code);
        let revision = fixture.turn.revision();
        let request = fixture.finish(EngramCommandExit::Code(code));
        one_build(
            &request,
            &revision,
            if code == 0 { "passed" } else { "failed" },
        );
        assert!(
            diagnostic_refs(&fixture.turn).contains(&format!("exit:{code}"))
        );
        assert!(!request.to_string().contains("results.json"));
    }
}

#[test]
fn engram_build_focused_owned_native_stage_can_supply_a_reported_success_exit() {
    let fixture = FocusedBuildFixture::new("build-focused-reported", 0);
    let revision = fixture.turn.revision();
    one_build(
        &fixture.finish(EngramCommandExit::ReportedSuccess),
        &revision,
        "passed",
    );
}

#[test]
fn engram_build_focused_summary_alone_does_not_supply_native_evidence() {
    let turn = EOCheckedTurn::start("build-focused-unbacked", true);
    let revision = turn.revision();
    complete_command(
        &turn,
        "build",
        FOCUSED_BUILD,
        EngramCommandExit::Code(0),
        "PASS test-fake exit=0\nfocused: passed\nfocused build: native exit 0\ntest result: ok. 5 passed",
    );
    let request = finish_observation(&turn);
    one_build(&request, &revision, "indeterminate");
    assert!(
        !diagnostic_refs(&turn).contains(&"exit:0".to_owned())
    );
}

#[test]
fn engram_build_focused_rejects_stale_foreign_and_mismatched_artifacts() {
    let timings = FixtureTimings::start("build_invalid_artifacts");
    let _repository_seed = EORepositorySeedScope::new();
    type Change = fn(&mut FocusedBuildFixture);
    let cases: &[(&str, Change)] = &[
        ("owner", |f| f.request["owner"] = json!("another-session")),
        ("result-owner", |f| {
            f.results["owner"] = json!("another-session")
        }),
        ("request-run", |f| f.request["runId"] = json!("test-other")),
        ("result-run", |f| f.results["runId"] = json!("test-other")),
        ("root", |f| {
            f.request["root"] = json!(f.turn.root.join("other"))
        }),
        ("detached", |f| f.request["detached"] = json!(true)),
        ("full", |f| f.request["full"] = json!(true)),
        ("request-argv", |f| {
            f.request["stages"][0]["args"] = json!(["test"])
        }),
        ("stage-argv", |f| {
            f.results["stages"][0]["command"][1] = json!("test")
        }),
        ("stage-executable", |f| {
            f.results["stages"][0]["command"][0] = json!("cargo.exe")
        }),
        ("stage-cwd", |f| {
            f.results["stages"][0]["cwd"] = json!(f.turn.root.join("other"))
        }),
        ("request-cwd", |f| {
            f.request["stages"][0]["cwd"] = json!("other")
        }),
        ("stale", |f| {
            let old = "2000-01-01T00:00:00.000Z";
            f.request["started"] = json!(old);
            for key in ["started", "ended"] {
                f.results[key] = json!(old);
                f.results["stages"][0][key] = json!(old);
            }
        }),
        ("future", |f| {
            f.results["ended"] = json!("2100-01-01T00:00:00.000Z")
        }),
        ("missing-time", |f| {
            f.results["stages"][0]["started"] = Value::Null
        }),
        ("preflight", |f| {
            f.results["error"] = json!("preflight failed")
        }),
        ("spawn", |f| {
            f.results["stages"][0]["error"] = json!("spawn failed")
        }),
        ("unrun", |f| {
            f.results["stages"][0]["state"] = json!("unrun")
        }),
        ("interrupted", |f| {
            f.results["stages"][0]["signal"] = json!("SIGTERM")
        }),
        ("missing-native", |f| {
            f.results["stages"][0]["code"] = Value::Null
        }),
        ("stage-code", |f| {
            f.results["stages"][0]["code"] = json!(101)
        }),
        ("outer-code", |f| f.results["exitCode"] = json!(1)),
        ("input", |f| f.request["sourceId"]["tree"] = json!("c".repeat(40))),
        ("drift", |f| f.results["sourceIdAfter"]["commit"] = json!("c".repeat(40))),
        ("malformed-source", |f| {
            f.request["sourceId"] = json!("bad")
        }),
        ("summary-run", |f| {
            f.output = f.output.replacen("PASS test-", "PASS test-other-", 1)
        }),
    ];
    for (label, change) in cases {
        timings.phase("fixture_setup");
        let mut fixture = FocusedBuildFixture::new(&format!("build-focused-refuse-{label}"), 0);
        timings.phase("case_operations");
        fixture.save();
        assert_eq!(
            engram_focused_build_exit(
                &fixture.check,
                &fixture.turn.session_id,
                &fixture.output,
                EngramCommandExit::Code(0)
            ),
            Some(0),
            "positive control: {label}"
        );
        change(&mut fixture);
        fixture.save();
        assert_eq!(
            engram_focused_build_exit(
                &fixture.check,
                &fixture.turn.session_id,
                &fixture.output,
                EngramCommandExit::Code(0)
            ),
            None,
            "must refuse: {label}"
        );
        let revision = fixture.turn.revision();
        let request = fixture.finish(EngramCommandExit::Code(0));
        one_build(&request, &revision, "indeterminate");
        assert!(
            !diagnostic_refs(&fixture.turn).contains(&"exit:0".to_owned()),
            "{label}"
        );
        timings.phase("scope_cleanup");
    }
}

#[test]
fn engram_build_focused_runtime_failure_or_interrupt_cannot_borrow_a_pass() {
    for exit in [
        EngramCommandExit::Code(1),
        EngramCommandExit::Unknown,
        EngramCommandExit::NotFinished,
    ] {
        let fixture = FocusedBuildFixture::new("build-focused-runtime-end", 0);
        fixture.save();
        assert_eq!(
            engram_focused_build_exit(
                &fixture.check,
                &fixture.turn.session_id,
                &fixture.output,
                exit
            ),
            None
        );
    }
}

#[test]
fn engram_build_overlapping_command_and_outstanding_work_still_withhold_success() {
    for outstanding in [false, true] {
        let turn = EOCheckedTurn::start("build-hazard", true);
        let mut recorder = turn.recorder();
        recorder.command_started("build", "cargo build").unwrap();
        turn.wait_for_snapshots();
        if outstanding {
            turn.record_mut(|record| {
                record.engram.active_turn_checks[0].overlapped = true;
                record.engram.active_turn_checks[0].fenced_by_outstanding =
                    Some(ClaudeHazardCause::OwnSession);
            });
        } else {
            recorder.command_started("other", "cargo check").unwrap();
        }
        recorder
            .command_completed_with_exit(
                "build",
                "cargo build",
                "",
                CommandStatus::Success,
                EngramCommandExit::Code(0),
            )
            .unwrap();
        turn.wait_for_snapshots();
        let request = finish_observation(&turn);
        assert!(request.get("verification_evidence").is_none());
        let check = diagnostic(&turn);
        assert_eq!(check.command.kind, EngramVerificationKind::Build);
        assert!(check.overlapped);
        if outstanding {
            assert!(check.fenced_by_outstanding.is_some());
        } else {
            assert!(check.overlap_cause.is_some());
        }
    }
}
