// Build recognition and typed closing-checkpoint evidence. Does not own
// disposable producer admission or the deferred real-task completion matrix.
// New child of engram_turn_checks.rs, reusing its actual recorder fixture.
use super::*;

#[path = "engram_build_outcomes.rs"]
mod outcomes;

fn complete_command(
    turn: &CheckedTurn,
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

fn one_build(checkpoint: &Value, revision: &str, outcome: &str) {
    let rows = checkpoint["verification_evidence"]
        .as_array()
        .expect("evidence array");
    assert_eq!(
        rows.len(),
        1,
        "one Build verification, not zero or dual recording: {checkpoint:#}"
    );
    let evidence = &rows[0];
    assert_eq!(evidence["check_kind"], "build", "{checkpoint:#}");
    let id = evidence["producer_observation"]["observation_id"]
        .as_str()
        .unwrap();
    let producers = observations(checkpoint)
        .into_iter()
        .filter(|observation| observation["observation_id"] == id)
        .collect::<Vec<_>>();
    assert_eq!(
        producers.len(),
        1,
        "exactly one referenced producer: {checkpoint:#}"
    );
    let producer = &producers[0];
    assert_eq!(producer["effect"], "observe");
    assert_eq!(producer["outcome"], outcome);
    assert_eq!(producer["source_changed"], false);
    assert_eq!(producer["source_basis"]["source_revision"], revision);
    let index = evidence["environment"]["index"]
        .as_u64()
        .expect("environment link") as usize;
    let environment = &checkpoint["environment_evidence"][index];
    assert_eq!(environment["source_basis"], producer["source_basis"]);
    assert_eq!(environment["observed_at"], producer["observed_at"]);
    let components: EngramEnvironmentComponents =
        serde_json::from_value(environment["components"].clone()).unwrap();
    assert_eq!(components.toolchain, FIXTURE_TOOLCHAIN);
    assert_eq!(
        environment["environment_fingerprint"],
        engram_environment_fingerprint(&components)
    );
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
    let turn = CheckedTurn::start("build-native-pass", true);
    let revision = turn.revision();
    complete_command(
        &turn,
        "build",
        "cargo build",
        EngramCommandExit::Code(0),
        "",
    );
    let checkpoint = turn.finish();
    one_build(&checkpoint, &revision, "succeeded");
    assert_eq!(
        checkpoint["verification_evidence"][0]["refs"],
        json!(["command:cargo build", "exit:0"])
    );
    assert!(
        checkpoint["verification_evidence"][0]["summary"]
            .as_str()
            .unwrap()
            .contains("exited 0")
    );
}

#[test]
fn engram_build_native_failure_records_failed_producer_without_test_output() {
    let turn = CheckedTurn::start("build-native-fail", true);
    let revision = turn.revision();
    complete_command(
        &turn,
        "build",
        "cargo build",
        EngramCommandExit::Code(101),
        "error: could not compile fixture",
    );
    let checkpoint = turn.finish();
    one_build(&checkpoint, &revision, "failed");
    assert!(
        checkpoint["verification_evidence"][0]["refs"]
            .as_array()
            .unwrap()
            .contains(&json!("exit:101"))
    );
}

#[test]
fn engram_build_and_test_commands_each_emit_only_their_own_kind() {
    let turn = CheckedTurn::start("build-and-test", true);
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
    let checkpoint = turn.finish();
    let rows = checkpoint["verification_evidence"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        2,
        "two commands, each exactly once: {checkpoint:#}"
    );
    assert_eq!(rows[0]["check_kind"], "build");
    assert_eq!(rows[1]["check_kind"], "test");
    assert_ne!(
        rows[0]["producer_observation"],
        rows[1]["producer_observation"]
    );
}

#[test]
fn engram_build_generic_reported_success_is_unknown_even_with_passing_test_text() {
    let turn = CheckedTurn::start("build-no-native-exit", true);
    let revision = turn.revision();
    complete_command(
        &turn,
        "build",
        "cargo build",
        EngramCommandExit::ReportedSuccess,
        "test result: ok. 8 passed; 0 failed",
    );
    let checkpoint = turn.finish();
    one_build(&checkpoint, &revision, "unknown");
    assert!(
        !checkpoint["verification_evidence"][0]["refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str().is_some_and(|text| text.starts_with("exit:")))
    );
}

#[test]
fn engram_build_one_call_failure_is_not_rescued_by_test_output() {
    let turn = CheckedTurn::start("build-pushd-failure", true);
    let revision = turn.revision();
    let command = format!("pushd \"{}\" && cargo build", turn.root.display());
    complete_command(
        &turn,
        "build",
        &command,
        EngramCommandExit::Code(1),
        "test result: FAILED. 0 passed; 1 failed",
    );
    let checkpoint = turn.finish();
    one_build(&checkpoint, &revision, "unknown");
    assert!(
        !checkpoint["verification_evidence"][0]["refs"]
            .as_array()
            .unwrap()
            .contains(&json!("exit:1"))
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
    let turn = CheckedTurn::start("build-test-control", true);
    complete_command(
        &turn,
        "test",
        "cargo test",
        EngramCommandExit::Code(0),
        "test result: ok. 1 passed; 0 failed",
    );
    let checkpoint = turn.finish();
    assert_eq!(
        checkpoint["verification_evidence"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(checkpoint["verification_evidence"][0]["check_kind"], "test");
    let empty = CheckedTurn::start("build-empty-test-control", true);
    complete_command(
        &empty,
        "test",
        "cargo test",
        EngramCommandExit::Code(0),
        "test result: ok. 0 passed; 0 failed",
    );
    assert!(empty.finish().get("verification_evidence").is_none());
}

#[test]
fn engram_build_changed_source_cannot_acquire_passing_credit() {
    let turn = CheckedTurn::start("build-changed-source", true);
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
    assert!(turn.finish().get("verification_evidence").is_none());
}

#[test]
fn engram_build_one_call_native_success_is_build_not_test() {
    let turn = CheckedTurn::start("build-pushd-success", true);
    let revision = turn.revision();
    let command = format!("pushd \"{}\" && cargo build", turn.root.display());
    complete_command(&turn, "build", &command, EngramCommandExit::Code(0), "");
    one_build(&turn.finish(), &revision, "succeeded");
}

const FOCUSED_BUILD: &str = "node scripts/test-launcher.mjs focused -- cargo build --offline";

/// Synthetic local launcher records, not execution evidence for the tracker.
/// Their ownership and interval are captured from the real running check.
struct FocusedBuildFixture {
    turn: CheckedTurn,
    check: EngramTurnCheck,
    directory: PathBuf,
    request: Value,
    results: Value,
    input: Value,
    output: String,
}

impl FocusedBuildFixture {
    fn new(label: &str, code: i64) -> Self {
        let turn = CheckedTurn::start(label, true);
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
        let fingerprint = "f".repeat(64);
        let state = if code == 0 { "passed" } else { "failed" };
        let request = json!({"runId":run,"root":turn.root,"owner":turn.session_id,
            "started":now,"full":false,"detached":false,"expectedFingerprint":fingerprint,
            "stages":[{"name":"focused","command":"cargo","args":["build","--offline"]}]});
        let results = json!({"runId":run,"owner":turn.session_id,"started":now,"ended":now,
            "state":state,"exitCode":code,"expectedFingerprint":fingerprint,
            "before":fingerprint,"after":fingerprint,"stages":[{"name":"focused","state":state,
                "code":code,"started":now,"ended":now,"cwd":turn.root,
                "command":[turn.root.join("cargo.exe"),"build","--offline"]}]});
        let input = json!({"fingerprint":fingerprint});
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

    fn finish(self, exit: EngramCommandExit) -> Value {
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
        self.turn.finish()
    }
}

#[test]
fn engram_build_focused_native_stage_records_pass_and_failure_without_test_counts() {
    for code in [0, 101] {
        let fixture = FocusedBuildFixture::new(&format!("build-focused-{code}"), code);
        let revision = fixture.turn.revision();
        let checkpoint = fixture.finish(EngramCommandExit::Code(code));
        one_build(
            &checkpoint,
            &revision,
            if code == 0 { "succeeded" } else { "failed" },
        );
        assert!(
            checkpoint["verification_evidence"][0]["refs"]
                .as_array()
                .unwrap()
                .contains(&json!(format!("exit:{code}")))
        );
        assert!(!checkpoint.to_string().contains("results.json"));
    }
}

#[test]
fn engram_build_focused_owned_native_stage_can_supply_a_reported_success_exit() {
    let fixture = FocusedBuildFixture::new("build-focused-reported", 0);
    let revision = fixture.turn.revision();
    one_build(
        &fixture.finish(EngramCommandExit::ReportedSuccess),
        &revision,
        "succeeded",
    );
}

#[test]
fn engram_build_focused_summary_alone_does_not_supply_native_evidence() {
    let turn = CheckedTurn::start("build-focused-unbacked", true);
    let revision = turn.revision();
    complete_command(
        &turn,
        "build",
        FOCUSED_BUILD,
        EngramCommandExit::Code(0),
        "PASS test-fake exit=0\nfocused: passed\nfocused build: native exit 0\ntest result: ok. 5 passed",
    );
    let checkpoint = turn.finish();
    one_build(&checkpoint, &revision, "unknown");
    assert!(
        !checkpoint["verification_evidence"][0]["refs"]
            .as_array()
            .unwrap()
            .contains(&json!("exit:0"))
    );
}

#[test]
fn engram_build_focused_rejects_stale_foreign_and_mismatched_artifacts() {
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
        ("input", |f| f.input["fingerprint"] = json!("a".repeat(64))),
        ("drift", |f| f.results["after"] = json!("a".repeat(64))),
        ("malformed-fingerprint", |f| {
            f.request["expectedFingerprint"] = json!("bad")
        }),
        ("summary-run", |f| {
            f.output = f.output.replacen("PASS test-", "PASS test-other-", 1)
        }),
    ];
    for (label, change) in cases {
        let mut fixture = FocusedBuildFixture::new(&format!("build-focused-refuse-{label}"), 0);
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
        let checkpoint = fixture.finish(EngramCommandExit::Code(0));
        one_build(&checkpoint, &revision, "unknown");
        assert!(
            !checkpoint["verification_evidence"][0]["refs"]
                .as_array()
                .unwrap()
                .contains(&json!("exit:0")),
            "{label}"
        );
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
        let turn = CheckedTurn::start("build-hazard", true);
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
        let checkpoint = turn.finish();
        assert!(checkpoint.get("verification_evidence").is_none());
        let line = turn
            .record(|record| record.engram.pending_source_root_line.clone())
            .unwrap();
        assert!(line.contains("cargo build (check"), "{line}");
        assert!(!line.contains("cargo test -q"), "{line}");
    }
}
