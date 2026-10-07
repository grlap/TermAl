// Foreground focused Node recognition, real native-runner counts and host
// feedback. Reuses the parent recorder/owned temporary-root fixtures; does
// not open a live store or reproduce a historical caller's run.
use super::*;

const NODE_TEST: &str = "node scripts/test-launcher.mjs focused -- node --test --test-name-pattern \"selected case\" fixture.test.mjs";

#[test]
fn engram_node_focused_recognition_preserves_quoted_filter_and_test_kind() {
    let check = engram_check_command(NODE_TEST).expect("a focused native Node test is recognized");
    assert_eq!(check.kind, EngramVerificationKind::Test);
    assert!(check.simple);
    assert_eq!(check.normalized, NODE_TEST);
    assert!(
        engram_shell_words(&check.normalized)
            .unwrap()
            .contains(&"selected case".to_owned())
    );
}

#[test]
fn engram_node_focused_other_commands_do_not_become_tests() {
    for command in [
        "node --test fixture.test.mjs",
        "node fixture.test.mjs",
        "node scripts/test-launcher.mjs focused -- node fixture.test.mjs",
        "node scripts/test-launcher.mjs focused -- node -e 'console.log(1)'",
        "node scripts/test-launcher.mjs focused -- node --test --help",
        "node scripts/test-launcher.mjs focused -- node --test --watch",
        "node scripts/test-launcher.mjs focused -- node --test --test-reporter=json",
        "node scripts/test-launcher.mjs focused -- node --test --test-name-pattern",
        "node scripts/test-launcher.mjs focused -- node --test --test-concurrency=0",
        "node scripts/test-launcher.mjs focused -- node --test fixture.test.mjs --help",
        "node scripts/test-launcher.mjs focused --detach -- node --test fixture.test.mjs",
    ] {
        assert!(engram_check_command(command).is_none(), "{command}");
    }
    assert_eq!(
        engram_check_command("node scripts/test-launcher.mjs focused -- cargo build")
            .unwrap()
            .kind,
        EngramVerificationKind::Build,
    );
    for command in [
        "node scripts/test-launcher.mjs focused -- cargo test",
        "node scripts/test-launcher.mjs full",
        "node scripts/test-launcher.mjs live",
    ] {
        assert_eq!(
            engram_check_command(command).unwrap().kind,
            EngramVerificationKind::Test
        );
    }
}

/// Produce the runner's actual terminal output, then invoke the existing
/// count parser on that output. Both subprocesses finish before this guard
/// drops; NODE_TEST_CONTEXT from a parent Node runner cannot change the child.
fn native_node_counts(reporter: &str, source: &str, expected_exit: i32) -> (Value, String) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let root = TestTempRoot::create("termal-node-count-producer");
    fs::write(root.join("fixture.test.mjs"), source).unwrap();
    let output = crate::host_command::Command::new("node")
        .args([
            "--test",
            &format!("--test-reporter={reporter}"),
            "fixture.test.mjs",
        ])
        .env_remove("NODE_TEST_CONTEXT")
        .current_dir(root.path())
        .output()
        .expect("native Node fixture should run");
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        output.status.code(),
        Some(expected_exit),
        "{text}\n{:?}",
        output.stderr
    );
    let log = root.join("native.log");
    fs::write(&log, &text).unwrap();
    let parser = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/test-counts.mjs");
    let parsed = crate::host_command::Command::new("node")
        .args(["--input-type=module", "-e", "import { readFileSync } from 'node:fs'; import { pathToFileURL } from 'node:url'; const { focusedTestCounts } = await import(pathToFileURL(process.argv[1]).href); console.log(JSON.stringify(focusedTestCounts(readFileSync(process.argv[2], 'utf8')) ?? null));"])
        .arg(parser).arg(log)
        .env_remove("NODE_TEST_CONTEXT")
        .current_dir(root.path())
        .output().expect("count parser fixture should run");
    assert!(parsed.status.success(), "{:?}", parsed.stderr);
    (serde_json::from_slice(&parsed.stdout).unwrap(), text)
}

fn assert_native_counts(reporter: &str, failed: bool) {
    let source = if failed {
        "import test from 'node:test'; test('selected case', () => { throw new Error('fixture failure'); });\n"
    } else {
        "import test from 'node:test'; test('selected case', () => {});\n"
    };
    let (counts, text) = native_node_counts(reporter, source, i32::from(failed));
    assert_eq!(
        counts,
        json!({
            "runner": "node-test", "passed": u32::from(!failed),
            "failed": u32::from(failed), "ignored": 0,
        }),
        "actual native {reporter} summary:\n{text}"
    );
}

#[test]
fn engram_node_counts_native_tap_success() {
    assert_native_counts("tap", false);
}

#[test]
fn engram_node_counts_native_spec_success() {
    assert_native_counts("spec", false);
}

#[test]
fn engram_node_counts_native_tap_failure() {
    assert_native_counts("tap", true);
}

#[test]
fn engram_node_counts_native_spec_failure() {
    assert_native_counts("spec", true);
}

#[test]
fn engram_node_counts_skip_and_todo_are_not_executed_passes() {
    for reporter in ["tap", "spec"] {
        let source = "import test from 'node:test'; test.skip('skipped', () => { throw new Error('must not run'); }); test.todo('pending');\n";
        let (counts, text) = native_node_counts(reporter, source, 0);
        assert_eq!(
            counts,
            json!({ "runner": "node-test", "passed": 0, "failed": 0, "ignored": 2 }),
            "{text}"
        );
    }
}

#[test]
fn engram_node_unsupported_focused_form_announces_no_test_record() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = CheckedTurn::start("node-unsupported-feedback", true);
    let command = "node scripts/test-launcher.mjs focused -- node ordinary.mjs";
    let mut recorder = turn.recorder();
    recorder
        .command_started_in(
            "unsupported",
            command,
            Some(command),
            Some(&turn.root.to_string_lossy()),
        )
        .unwrap();
    assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
    let line = turn
        .record(|record| record.engram.pending_source_root_line.clone())
        .expect("unsupported focused invocation must tell its caller no Test record will follow");
    assert!(line.contains("ordinary.mjs"), "{line}");
    assert!(line.contains("no Test record will follow"), "{line}");
}

#[test]
fn engram_node_ordinary_commands_have_no_launcher_specific_notice() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = CheckedTurn::start("node-ordinary-no-feedback", true);
    let command = "node ordinary.mjs";
    let mut recorder = turn.recorder();
    recorder
        .command_started_in(
            "ordinary",
            command,
            Some(command),
            Some(&turn.root.to_string_lossy()),
        )
        .unwrap();
    assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
    assert_eq!(
        turn.record(|record| record.engram.pending_source_root_line.clone()),
        None
    );
}

#[test]
fn engram_node_launcher_native_result_and_parser_controls() {
    let output = crate::host_command::Command::new("node")
        .args([
            "--test",
            "--test-name-pattern",
            "focused native Node",
            "scripts/test-launcher.test.mjs",
        ])
        .env_remove("NODE_TEST_CONTEXT")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("native launcher controls should run");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Synthetic matched records exercise the actual recorder, check start/end,
/// source captures and checkpoint. They are not live producer/test evidence.
struct FocusedNodeFixture {
    turn: CheckedTurn,
    command: String,
    directory: PathBuf,
    request: Value,
    results: Value,
    input: Value,
    output: String,
}

impl FocusedNodeFixture {
    fn new(label: &str, native: i64) -> Self {
        Self::with_one_call(label, native, false)
    }

    fn with_one_call(label: &str, native: i64, one_call: bool) -> Self {
        let _placement = TestTempRootDirectoryScope::repository_local();
        let turn = CheckedTurn::start(label, true);
        let command = if one_call {
            format!("pushd \"{}\" && {NODE_TEST}", turn.root.display())
        } else {
            NODE_TEST.to_owned()
        };
        turn.recorder()
            .command_started_in(
                "node",
                &command,
                Some(&command),
                Some(&turn.root.to_string_lossy()),
            )
            .unwrap();
        turn.wait_for_snapshots();
        assert!(turn.record(|record| record.engram.active_turn_checks.len() == 1));
        let run = format!("test-{}", Uuid::new_v4());
        let directory = engram_git_run_directory(&turn.root).unwrap().join(&run);
        fs::create_dir_all(&directory).unwrap();
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let fingerprint = "f".repeat(64);
        let state = if native == 0 { "passed" } else { "failed" };
        let args = [
            "--test",
            "--test-name-pattern",
            "selected case",
            "fixture.test.mjs",
        ];
        let request = json!({"runId":run,"root":turn.root,"owner":turn.session_id,
            "started":now,"full":false,"detached":false,"expectedFingerprint":fingerprint,
            "stages":[{"name":"focused","command":"node","args":args}]});
        let results = json!({"runId":run,"owner":turn.session_id,"started":now,"ended":now,
            "state":state,"exitCode":native,"expectedFingerprint":fingerprint,"before":fingerprint,"after":fingerprint,
            "stages":[{"name":"focused","state":state,"code":native,"started":now,"ended":now,
                "cwd":turn.root,"command":[turn.root.join("node.exe"),"--test","--test-name-pattern","selected case","fixture.test.mjs"],
                "tests":{"runner":"node-test","passed":u32::from(native == 0),"failed":u32::from(native != 0),"ignored":0}}]});
        let output = format!(
            "{} {run} exit={native}\nfocused: {state}\nresults: {}\n",
            if native == 0 { "PASS" } else { "FAIL" },
            directory.join("results.json").display()
        );
        Self {
            turn,
            command,
            directory,
            request,
            results,
            input: json!({"fingerprint":fingerprint}),
            output,
        }
    }

    fn finish(self, exit: EngramCommandExit) -> Value {
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
        self.turn
            .recorder()
            .command_completed_with_exit(
                "node",
                &self.command,
                &self.output,
                if matches!(exit, EngramCommandExit::Code(n) if n != 0) {
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

fn one_node_test(checkpoint: &Value, outcome: &str) {
    let rows = checkpoint["verification_evidence"]
        .as_array()
        .expect("typed verification");
    assert_eq!(rows.len(), 1, "{checkpoint:#}");
    assert_eq!(rows[0]["check_kind"], "test");
    let id = &rows[0]["producer_observation"]["observation_id"];
    let producers = observations(checkpoint)
        .into_iter()
        .filter(|row| &row["observation_id"] == id)
        .collect::<Vec<_>>();
    assert_eq!(producers.len(), 1);
    assert_eq!(producers[0]["outcome"], outcome, "{checkpoint:#}");
    assert_eq!(producers[0]["source_changed"], false);
}

#[test]
fn engram_node_matched_native_pass_and_failure_record_one_test() {
    for native in [0, 1] {
        let fixture = FocusedNodeFixture::new(&format!("node-matched-{native}"), native);
        one_node_test(
            &fixture.finish(EngramCommandExit::Code(native)),
            if native == 0 { "succeeded" } else { "failed" },
        );
    }
}

#[test]
fn engram_node_one_call_matched_pass_and_failure_keep_native_outcome() {
    for native in [0, 1] {
        let fixture = FocusedNodeFixture::with_one_call(
            &format!("node-one-call-matched-{native}"),
            native,
            true,
        );
        one_node_test(
            &fixture.finish(EngramCommandExit::Code(native)),
            if native == 0 { "succeeded" } else { "failed" },
        );
    }
}

#[test]
fn engram_node_one_call_invalid_artifacts_do_not_prove_execution() {
    for native in [0, 1] {
        let mut fixture = FocusedNodeFixture::with_one_call(
            &format!("node-one-call-invalid-{native}"),
            native,
            true,
        );
        let command = engram_check_command(&fixture.command).unwrap();
        assert!(!engram_check_exit_is_the_tests(
            &command,
            EngramCommandExit::Code(1),
            &[],
        ));
        fixture.request["owner"] = json!("foreign");
        one_node_test(&fixture.finish(EngramCommandExit::Code(native)), "unknown");
    }
}

fn assert_wrapped_unsupported_notice(cmd_wrapper: bool) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for late_description in [false, true] {
        let turn = CheckedTurn::start("node-wrapped-feedback", true);
        let inner = "node scripts/test-launcher.mjs focused -- node ordinary.mjs";
        let command = if cmd_wrapper {
            format!("cmd /c {inner}")
        } else {
            format!("pushd \"{}\" && {inner}", turn.root.display())
        };
        assert!(engram_check_command(&command).is_none());
        let mut recorder = turn.recorder();
        let started = if late_description {
            "node ordinary.mjs"
        } else {
            &command
        };
        recorder
            .command_started_in(
                "wrapped",
                started,
                Some(started),
                Some(&turn.root.to_string_lossy()),
            )
            .unwrap();
        if late_description {
            assert!(turn.record(|record| record.engram.pending_source_root_line.is_none()));
            recorder
                .command_described("wrapped", Some(&command), None)
                .unwrap();
        }
        let line = turn
            .record(|record| record.engram.pending_source_root_line.clone())
            .expect("supported envelope must receive the unsupported focused notice");
        assert!(
            line.contains("ordinary.mjs") && line.contains("no Test record will follow"),
            "{line}",
        );
        assert!(
            line.contains(if cmd_wrapper { "cmd /c" } else { "pushd" }),
            "{line}",
        );
        assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
        turn.record_mut(|record| {
            record.engram.pending_source_root_line.take();
        });
        recorder
            .command_described("wrapped", Some(&command), None)
            .unwrap();
        assert!(turn.record(|record| record.engram.pending_source_root_line.is_none()));
        recorder
            .command_completed_with_exit(
                "wrapped",
                &command,
                "",
                CommandStatus::Success,
                EngramCommandExit::Code(0),
            )
            .unwrap();
        recorder
            .command_started_in(
                "wrapped",
                &command,
                Some(&command),
                Some(&turn.root.to_string_lossy()),
            )
            .unwrap();
        assert!(
            turn.record(|record| record.engram.pending_source_root_line.is_some()),
            "a new wrapped command reusing a completed key receives a new notice",
        );
    }
}

#[test]
fn engram_node_cmd_unsupported_feedback_covers_start_description_and_dedup() {
    assert_wrapped_unsupported_notice(true);
}

#[test]
fn engram_node_pushd_unsupported_feedback_covers_start_description_and_dedup() {
    assert_wrapped_unsupported_notice(false);
}

#[test]
fn engram_node_notice_normalization_never_admits_arbitrary_shell_forms() {
    let inner = "node scripts/test-launcher.mjs focused -- node ordinary.mjs";
    for command in [
        format!("cmd /c {inner}"),
        format!("bash -lc '{inner}'"),
        format!("pwsh -Command '{inner}'"),
    ] {
        assert!(engram_check_command(&command).is_none());
        assert!(engram_unsupported_focused_line(&command).is_some(), "{command}");
    }
    let root = env!("CARGO_MANIFEST_DIR");
    for command in [
        format!("pushd \"{root}\" && {NODE_TEST}"),
        format!("pushd \"{root}\" && node scripts/test-launcher.mjs focused -- cargo build"),
        format!("cmd /c {NODE_TEST}"),
        "cmd /c node ordinary.mjs".to_owned(),
        format!("cmd /k {inner}"),
        format!("cmd /c cd other && {inner}"),
        format!("pushd \"{root}\"; {inner}"),
        format!("pushd \"{root}\" && cmd /c {inner}"),
        format!("pushd \"{root}/../other\" && {inner}"),
        format!("{inner} | other"),
        format!("{inner}\nnode ordinary.mjs"),
    ] {
        assert!(engram_unsupported_focused_line(&command).is_none(), "{command}");
    }
}

#[test]
fn engram_node_invalid_artifacts_cannot_be_rescued_by_summary_or_counts() {
    type Change = fn(&mut FocusedNodeFixture);
    let cases: &[(&str, Change)] = &[
        ("owner", |f| f.request["owner"] = json!("foreign")),
        ("result-owner", |f| f.results["owner"] = json!("foreign")),
        ("request-root", |f| {
            f.request["root"] = json!(f.turn.root.join("other"))
        }),
        ("argv", |f| {
            f.results["stages"][0]["command"][2] = json!("--help")
        }),
        ("fingerprint", |f| {
            f.results["after"] = json!("a".repeat(64))
        }),
        ("stale", |f| {
            f.request["started"] = json!("2000-01-01T00:00:00.000Z")
        }),
        ("unrun", |f| {
            f.results["stages"][0]["state"] = json!("unrun")
        }),
        ("detached", |f| f.request["detached"] = json!(true)),
        ("error", |f| f.results["error"] = json!("spawn failed")),
        ("signal", |f| {
            f.results["stages"][0]["signal"] = json!("SIGINT")
        }),
        ("missing-counts", |f| {
            f.results["stages"][0]["tests"] = Value::Null
        }),
        ("wrong-runner", |f| {
            f.results["stages"][0]["tests"]["runner"] = json!("cargo-libtest")
        }),
        ("zero", |f| {
            f.results["stages"][0]["tests"]["passed"] = json!(0)
        }),
        ("fractional", |f| {
            f.results["stages"][0]["tests"]["passed"] = json!(1.5)
        }),
        ("failed-with-zero-exit", |f| {
            f.results["stages"][0]["tests"]["failed"] = json!(1)
        }),
        ("summary-run", |f| {
            f.output = f.output.replace("PASS test-", "PASS wrong-")
        }),
    ];
    for (label, change) in cases {
        let mut fixture = FocusedNodeFixture::new(&format!("node-invalid-{label}"), 0);
        change(&mut fixture);
        fixture
            .output
            .push_str("tests: passed=20 failed=0 ignored=0\n");
        one_node_test(&fixture.finish(EngramCommandExit::Code(0)), "unknown");
    }
    for exit in [
        EngramCommandExit::Unknown,
        EngramCommandExit::NotFinished,
        EngramCommandExit::Code(1),
    ] {
        let fixture = FocusedNodeFixture::new("node-no-terminal", 0);
        let checkpoint = fixture.finish(exit);
        if exit == EngramCommandExit::NotFinished {
            assert!(checkpoint.get("verification_evidence").is_none());
        } else {
            one_node_test(&checkpoint, "unknown");
        }
    }
}

#[test]
fn engram_node_scoped_fixture_placement_restores_after_unwind() {
    let before = TEST_TEMP_ROOT_DIRECTORY.with(|slot| slot.borrow().clone());
    let _ = std::panic::catch_unwind(|| {
        let _scope = TestTempRootDirectoryScope::repository_local();
        let root = TestTempRoot::create("termal-node-placement");
        assert!(
            root.path()
                .starts_with(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".tmp"))
        );
        panic!("exercise restoration");
    });
    assert_eq!(
        TEST_TEMP_ROOT_DIRECTORY.with(|slot| slot.borrow().clone()),
        before
    );
}

#[test]
fn engram_node_unsupported_notice_is_deduplicated_across_start_and_description() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = CheckedTurn::start("node-feedback-dedup", true);
    let command = "node scripts/test-launcher.mjs focused -- node ordinary.mjs";
    let mut recorder = turn.recorder();
    recorder
        .command_started_in(
            "unsupported",
            command,
            Some(command),
            Some(&turn.root.to_string_lossy()),
        )
        .unwrap();
    assert!(turn.record(|record| record.engram.pending_source_root_line.is_some()));
    turn.record_mut(|record| {
        record.engram.pending_source_root_line.take();
    });
    recorder
        .command_started_in(
            "unsupported",
            command,
            Some(command),
            Some(&turn.root.to_string_lossy()),
        )
        .unwrap();
    recorder
        .command_described("unsupported", Some(command), None)
        .unwrap();
    assert_eq!(
        turn.record(|record| record.engram.pending_source_root_line.clone()),
        None
    );
    assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
    recorder
        .command_completed_with_exit(
            "unsupported",
            command,
            "",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    recorder
        .command_started_in(
            "unsupported",
            command,
            Some(command),
            Some(&turn.root.to_string_lossy()),
        )
        .unwrap();
    assert!(
        turn.record(|record| record.engram.pending_source_root_line.is_some()),
        "a genuinely new command reusing the key still receives its own notice"
    );
}

#[test]
fn engram_node_source_and_grant_fences_remain_non_passing() {
    let fixture = FocusedNodeFixture::new("node-source-changed", 0);
    fixture.turn.change_readme("changed during Node tests\n");
    assert!(
        fixture
            .finish(EngramCommandExit::Code(0))
            .get("verification_evidence")
            .is_none()
    );
    let fixture = FocusedNodeFixture::new("node-grant-changed", 0);
    fixture
        .turn
        .record_mut(|record| record.engram.active_grant_id = Some("replacement-grant".to_owned()));
    assert!(
        fixture
            .finish(EngramCommandExit::Code(0))
            .get("verification_evidence")
            .is_none()
    );
}

#[test]
fn engram_node_no_claim_starts_no_test_and_stale_feedback_is_suppressed() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = CheckedTurn::start("node-unclaimed", false);
    turn.recorder()
        .command_started_in(
            "node",
            NODE_TEST,
            Some(NODE_TEST),
            Some(&turn.root.to_string_lossy()),
        )
        .unwrap();
    assert!(turn.record(|record| record.engram.active_turn_checks.is_empty()));
    let turn = CheckedTurn::start("node-stale-feedback", true);
    let mut recorder = turn.recorder();
    let provenance = turn.record(|record| {
        EngramObservationProvenance::Claude(ClaudeObservationProvenance {
            token: record
                .runtime
                .runtime_token()
                .expect("matching fixture runtime"),
            origin: ClaudeWorkOrigin::Attempt {
                turn_generation: record.active_turn_generation,
            },
        })
    });
    recorder.set_observation_provenance(provenance.clone());
    let command = "node scripts/test-launcher.mjs focused -- node ordinary.mjs";
    assert_eq!(
        turn.record(|record| claude_observation_disposition(record, &provenance)),
        ClaudeObservationDisposition::Current
    );
    recorder
        .command_started_in(
            "current",
            command,
            Some(command),
            Some(&turn.root.to_string_lossy()),
        )
        .unwrap();
    assert!(
        turn.record(|record| record.engram.pending_source_root_line.is_some()),
        "matching captured token/attempt must actually deliver its notice"
    );
    turn.record_mut(|record| {
        record.engram.pending_source_root_line.take();
        record.active_turn_generation += 1;
    });
    assert_eq!(
        turn.record(|record| claude_observation_disposition(record, &provenance)),
        ClaudeObservationDisposition::Foreign
    );
    recorder
        .command_started_in(
            "stale",
            command,
            Some(command),
            Some(&turn.root.to_string_lossy()),
        )
        .unwrap();
    assert_eq!(
        turn.record(|record| record.engram.pending_source_root_line.clone()),
        None
    );
}
