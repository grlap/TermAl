// Registered launcher runs a Claude Bash call reports only through a cut
// output or a background task end. Drives the real recorder and frame-work
// owners with independent run artifacts; the receipt's settlement is the
// observed outcome. Child of engram_registered_launcher_closures.rs.
use super::*;

const FULL_COMMAND: &str = "node scripts/test-launcher.mjs full --input commit";
const CARGO_COMMAND: &str =
    "node scripts/test-launcher.mjs focused --input commit -- cargo test --bin termal locators";

/// A registered committed focused cargo-test run, still running, started by
/// a Claude Bash call whose observations carry Claude provenance.
fn claude_registered_cargo(label: &str) -> RegisteredLauncherTurn {
    claude_registered_focused(
        label,
        CARGO_COMMAND,
        "cargo",
        &["test", "--bin", "termal", "locators"],
    )
}

/// A registered committed focused run of `program` with `args`, still
/// running, started by the Claude Bash call `command`.
fn claude_registered_focused(
    label: &str,
    command: &str,
    program: &str,
    args: &[&str],
) -> RegisteredLauncherTurn {
    let turn = RegisteredLauncherTurn::committed_turn(label);
    let cwd = turn.record(|record| record.engram.observed_source_root().unwrap().root.clone());
    let mut recorder = turn.recorder();
    recorder.set_observation_provenance(claude_provenance(&turn));
    recorder
        .command_started_in(KEY, command, Some(command), Some(&cwd))
        .unwrap();
    let mut fixture =
        RegisteredLauncherTurn::after_start_with_program(turn, recorder, true, program, args);
    fixture.command = command.to_owned();
    fixture
}

/// The ended check of the fixture's call.
fn check_end(fixture: &RegisteredLauncherTurn) -> EngramTurnCheckEnd {
    fixture.turn.record(|record| {
        record
            .engram
            .active_turn_checks
            .iter()
            .find(|check| check.key == KEY)
            .and_then(|check| check.end.clone())
            .unwrap()
    })
}

/// The provenance a Claude frame of the fixture's current turn carries.
fn claude_provenance(turn: &EOCheckedTurn) -> EngramObservationProvenance {
    turn.record(|record| {
        EngramObservationProvenance::Claude(ClaudeObservationProvenance {
            token: record.runtime.runtime_token().unwrap(),
            origin: ClaudeWorkOrigin::Attempt {
                turn_generation: record.active_turn_generation,
            },
        })
    })
}

fn claude_token(turn: &EOCheckedTurn) -> RuntimeToken {
    turn.record(|record| record.runtime.runtime_token().unwrap())
}

/// A registered committed launcher run, still running, started by a Claude
/// Bash call whose observations carry Claude provenance. `full` registers a
/// full gate instead of a focused run.
fn claude_registered(label: &str, full: bool) -> RegisteredLauncherTurn {
    let command = if full {
        FULL_COMMAND
    } else {
        COMMITTED_COMMAND
    };
    claude_registered_as(label, full, command)
}

/// As `claude_registered`, launched by `command`.
fn claude_registered_as(label: &str, full: bool, command: &str) -> RegisteredLauncherTurn {
    let turn = RegisteredLauncherTurn::committed_turn(label);
    let cwd = turn.record(|record| record.engram.observed_source_root().unwrap().root.clone());
    let mut recorder = turn.recorder();
    recorder.set_observation_provenance(claude_provenance(&turn));
    recorder
        .command_started_in(KEY, command, Some(command), Some(&cwd))
        .unwrap();
    let mut fixture = RegisteredLauncherTurn::after_start_with_input(turn, recorder, true);
    if full {
        // The gate's own run, registered as the launcher writes a full gate.
        // The fixture's focused registration stays: another mode, so it is
        // never the gate's match.
        let mut request: Value =
            serde_json::from_slice(&fs::read(fixture.directory.join("request.json")).unwrap())
                .unwrap();
        request["full"] = json!(true);
        let run_id = format!("test-{}", Uuid::new_v4());
        let directory = fixture.directory.parent().unwrap().join(&run_id);
        fs::create_dir_all(&directory).unwrap();
        request["runId"] = json!(run_id);
        fs::write(
            directory.join("request.json"),
            serde_json::to_vec(&request).unwrap(),
        )
        .unwrap();
        fixture.run_id = run_id;
        fixture.directory = directory;
        fixture.command = command.to_owned();
        fixture.write_results("running");
        let ack = fixture
            .turn
            .state
            .register_test_run(
                &fixture.turn.session_id,
                RegisterTestRunRequest {
                    run_id: fixture.run_id.clone(),
                    run_directory: test_run_display_path(&fixture.directory),
                },
            )
            .unwrap();
        assert!(ack.registered);
        fixture.assert_unindexed();
    }
    fixture
}

/// The launch row of the fixture's call, as it now stands.
fn launch_row(fixture: &RegisteredLauncherTurn) -> TestRunCommandObservation {
    launch_row_for(fixture, KEY)
}

/// The launch row of the call `key`, as it now stands.
fn launch_row_for(fixture: &RegisteredLauncherTurn, key: &str) -> TestRunCommandObservation {
    let tool = sha256_hex(key.as_bytes());
    let inner = fixture.turn.state.inner.lock().unwrap();
    let rows: Vec<_> = inner
        .test_run_observations
        .values()
        .filter(|row| row.tool_identity == tool)
        .cloned()
        .collect();
    assert_eq!(rows.len(), 1);
    rows.into_iter().next().unwrap()
}

fn carried_run(fixture: &RegisteredLauncherTurn) -> Option<String> {
    launch_row(fixture).artifact.map(|artifact| artifact.run_id)
}

fn refusal_code(fixture: &RegisteredLauncherTurn) -> Option<TestRunNativeCompletionRefusalCode> {
    refusal_code_for(fixture, KEY)
}

fn refusal_code_for(
    fixture: &RegisteredLauncherTurn,
    key: &str,
) -> Option<TestRunNativeCompletionRefusalCode> {
    launch_row_for(fixture, key)
        .native_completion_refusal
        .map(|refusal| refusal.code)
}

/// Claude's Bash call `KEY` as its assistant frame starts it, admitted as the
/// runtime admits every frame.
fn admit_call(fixture: &RegisteredLauncherTurn, background: bool) {
    admit_call_for(fixture, KEY, background);
}

fn admit_call_for(fixture: &RegisteredLauncherTurn, key: &str, background: bool) {
    let EngramObservationProvenance::Claude(provenance) = claude_provenance(&fixture.turn) else {
        unreachable!()
    };
    let work = claude_frame_work(
        &json!({"type": "assistant", "message": {"content": [{
            "type": "tool_use", "id": key, "name": "Bash",
            "input": {"command": fixture.command, "run_in_background": background}
        }]}}),
        false,
    );
    fixture
        .turn
        .state
        .admit_claude_frame(&fixture.turn.session_id, &provenance, false, &work);
}

/// The tool result of a call that now runs in the background, as Claude's
/// Bash handler records it: the launch, not the run's end.
fn background_result(fixture: &mut RegisteredLauncherTurn, moved: bool) {
    background_result_for(fixture, KEY, moved);
}

fn background_result_for(fixture: &mut RegisteredLauncherTurn, key: &str, moved: bool) {
    fixture
        .recorder
        .command_completed_with_capture(
            key,
            &fixture.command,
            "Command running in background with ID: task-1",
            CommandStatus::Success,
            EngramHost::claude_command_exit(false, false, true, ""),
            false,
            false,
        )
        .unwrap();
    let mut result = json!({"type": "user", "message": {"content": [{
        "type": "tool_result", "tool_use_id": key, "content": "", "is_error": false
    }]}});
    if moved {
        result["tool_use_result"] = json!({"backgroundTaskId": "task-1"});
    }
    let work = claude_frame_work(&result, false);
    let token = claude_token(&fixture.turn);
    fixture
        .turn
        .state
        .settle_claude_frame(&fixture.turn.session_id, &token, &work);
}

/// Claude reports the background task's end.
fn task_end(fixture: &RegisteredLauncherTurn) {
    task_end_for(fixture, KEY);
}

fn task_end_for(fixture: &RegisteredLauncherTurn, key: &str) {
    let work = claude_frame_work(
        &json!({"type": "system", "subtype": "task_notification",
            "status": "completed", "tool_use_id": key}),
        false,
    );
    let token = claude_token(&fixture.turn);
    fixture
        .turn
        .state
        .settle_claude_frame(&fixture.turn.session_id, &token, &work);
}

/// The producer's next look at the run's held receipt, completed.
fn settle_held(fixture: &RegisteredLauncherTurn) -> EngramVerificationRecordSettlement {
    let registration = fixture
        .turn
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_registrations[&fixture.run_id]
        .clone();
    let EngramVerificationRecordPrepared::ReceiptHeld { frozen, receipt } =
        engram_verification_record_prepare(&registration)
    else {
        panic!("the held receipt is looked at again");
    };
    fixture
        .turn
        .state
        .complete_engram_verification_record(&registration, &frozen.body, &receipt)
}

/// Claude Code's cut foreground result: the visible head of a long output.
fn cut_result(fixture: &mut RegisteredLauncherTurn, head: &str) {
    let output = format!(
        "{head}\nwarning: unused variable: `x`\n{}",
        "x".repeat(2_000)
    );
    fixture
        .recorder
        .command_completed_with_capture(
            KEY,
            &fixture.command,
            &output,
            CommandStatus::Success,
            EngramCommandExit::Code(0),
            true,
            false,
        )
        .unwrap();
}

#[test]
fn a_cut_foreground_cargo_test_result_settles_from_its_visible_locator() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = claude_registered_cargo("claude-cut-foreground-cargo");
    let head = format!("RUN {}", test_run_display_path(&fixture.directory));
    cut_result(&mut fixture, &head);
    assert_eq!(
        carried_run(&fixture).as_deref(),
        Some(fixture.run_id.as_str())
    );
    let (_, settlement) = fixture.receive_until(true, None);
    assert_eq!(settlement, EngramVerificationRecordSettlement::Settled);
}

/// Direct focused launcher lines whose test arguments quote a `|`, or a `|`
/// and a `;`, with the argument bash passes on for each. The Bash start
/// recognizer reads each line as one simple command.
const QUOTED_OPERATOR_COMMANDS: [(&str, &str, &str); 2] = [
    (
        "pipe",
        "node scripts/test-launcher.mjs focused --input commit -- cargo test -- --skip 'alpha|beta'",
        "alpha|beta",
    ),
    (
        "pipe-and-semicolon",
        "node scripts/test-launcher.mjs focused --input commit -- cargo test -- --skip 'alpha|beta;gamma'",
        "alpha|beta;gamma",
    ),
];

#[test]
fn a_cut_foreground_run_whose_line_quotes_an_operator_settles_from_its_visible_locator() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for (label, command, skip) in QUOTED_OPERATOR_COMMANDS {
        let mut fixture = claude_registered_focused(
            &format!("claude-cut-quoted-{label}"),
            command,
            "cargo",
            &["test", "--", "--skip", skip],
        );
        let head = format!("RUN {}", test_run_display_path(&fixture.directory));
        cut_result(&mut fixture, &head);
        assert_eq!(
            carried_run(&fixture).as_deref(),
            Some(fixture.run_id.as_str()),
            "{command}"
        );
        let (_, settlement) = fixture.receive_until(true, None);
        assert_eq!(
            settlement,
            EngramVerificationRecordSettlement::Settled,
            "{command}"
        );
    }
}

#[test]
fn a_background_run_whose_line_quotes_an_operator_settles_at_its_end() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for (label, command, skip) in QUOTED_OPERATOR_COMMANDS {
        let mut fixture = claude_registered_focused(
            &format!("claude-background-quoted-{label}"),
            command,
            "cargo",
            &["test", "--", "--skip", skip],
        );
        admit_call(&fixture, true);
        background_result(&mut fixture, false);
        let (_, settlement) = fixture.receive_until(true, None);
        assert_eq!(
            settlement,
            EngramVerificationRecordSettlement::NoLaunchRow,
            "{command}"
        );
        task_end(&fixture);
        assert_eq!(
            carried_run(&fixture).as_deref(),
            Some(fixture.run_id.as_str()),
            "{command}"
        );
        assert_eq!(
            settle_held(&fixture),
            EngramVerificationRecordSettlement::Settled,
            "{command}"
        );
    }
}

#[test]
fn a_background_focused_run_longer_than_the_command_summary_settles() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    // More arguments than a run summary keeps, and more bytes: the whole
    // recorded command is what the line's words are compared with.
    let many: Vec<String> = ["test", "--"]
        .into_iter()
        .map(str::to_owned)
        .chain((0..16).flat_map(|index| ["--skip".to_owned(), format!("s{index}")]))
        .collect();
    let long: Vec<String> = ["test", "--", "--skip"]
        .into_iter()
        .map(str::to_owned)
        .chain(std::iter::once("x".repeat(600)))
        .collect();
    for (label, args) in [
        ("claude-many-arguments", many),
        ("claude-long-argument", long),
    ] {
        let command = format!(
            "node scripts/test-launcher.mjs focused --input commit -- cargo {}",
            args.join(" ")
        );
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut fixture = claude_registered_focused(label, &command, "cargo", &args);
        admit_call(&fixture, true);
        background_result(&mut fixture, false);
        let (_, settlement) = fixture.receive_until(true, None);
        assert_eq!(settlement, EngramVerificationRecordSettlement::NoLaunchRow);
        task_end(&fixture);
        assert_eq!(
            carried_run(&fixture).as_deref(),
            Some(fixture.run_id.as_str()),
            "{label}"
        );
        assert_eq!(
            settle_held(&fixture),
            EngramVerificationRecordSettlement::Settled,
            "{label}"
        );
    }
}

#[test]
fn a_compound_launcher_line_attaches_nothing_and_says_why() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    // The launcher's run is one of the line's commands; another runs after
    // it, so its end is not the run's alone.
    let mut fixture = claude_registered_focused(
        "claude-compound-launcher",
        "node scripts/test-launcher.mjs focused --input commit -- cargo test --bin termal locators && echo done",
        "cargo",
        &["test", "--bin", "termal", "locators"],
    );
    admit_call(&fixture, true);
    background_result(&mut fixture, false);
    fixture.write_results("passed");
    task_end(&fixture);
    assert_eq!(carried_run(&fixture), None);
    assert_eq!(
        launch_row(&fixture).native_completion_refusal,
        Some(TestRunNativeCompletionRefusal::new(
            TestRunNativeCompletionPhase::ArtifactAssociation,
            TestRunNativeCompletionRefusalCode::ArtifactRequestMismatch,
            &[(TestRunNativeCompletionFact::ObservationMatches, false)],
        ))
    );
}

#[test]
fn a_cut_locator_outside_the_run_root_is_refused_and_lends_no_record_to_the_check_end() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = claude_registered("claude-cut-foreign-locator", false);
    let foreign = fixture.turn.root.join("elsewhere").join(&fixture.run_id);
    fs::create_dir_all(&foreign).unwrap();
    fs::copy(
        fixture.directory.join("request.json"),
        foreign.join("request.json"),
    )
    .unwrap();
    // A record that, read from an uncut summary, says the run ran no stage.
    fs::write(
        foreign.join("results.json"),
        serde_json::to_vec(&json!({"runId": fixture.run_id, "state": "failed",
            "stages": [{"name": "focused", "state": "unrun"}]}))
        .unwrap(),
    )
    .unwrap();
    let head = format!(
        "FAIL {} exit=1\nresults: {}",
        fixture.run_id,
        test_run_display_path(&foreign.join("results.json"))
    );
    cut_result(&mut fixture, &head);
    assert_eq!(carried_run(&fixture), None);
    assert_eq!(
        refusal_code(&fixture),
        Some(TestRunNativeCompletionRefusalCode::ArtifactDirectoryMismatch)
    );
    assert!(
        !check_end(&fixture).launcher_ran_no_stage,
        "the end readers read no run record from a cut output"
    );
    let (_, settlement) = fixture.receive_until(true, None);
    assert_eq!(settlement, EngramVerificationRecordSettlement::NoLaunchRow);
}

#[test]
fn a_run_another_launch_row_carries_is_not_a_background_end_match() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = claude_registered("claude-carried-elsewhere", false);
    // Another call of the session already carries the only registered run.
    let cwd = fixture
        .turn
        .record(|record| record.engram.observed_source_root().unwrap().root.clone());
    let command = fixture.command.clone();
    fixture
        .recorder
        .command_started_in("other-call", &command, Some(&command), Some(&cwd))
        .unwrap();
    fixture
        .recorder
        .command_completed_with_capture(
            "other-call",
            &command,
            &format!("RUN {}", test_run_display_path(&fixture.directory)),
            CommandStatus::Success,
            EngramCommandExit::Code(0),
            false,
            false,
        )
        .unwrap();
    admit_call(&fixture, true);
    background_result(&mut fixture, false);
    fixture.write_results("passed");
    task_end(&fixture);
    assert_eq!(carried_run(&fixture), None);
    assert_eq!(
        refusal_code(&fixture),
        Some(TestRunNativeCompletionRefusalCode::ArtifactLocatorMissing)
    );
}

#[test]
fn a_background_end_whose_run_holds_another_request_attaches_nothing() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = claude_registered("claude-replaced-request", false);
    admit_call(&fixture, true);
    background_result(&mut fixture, false);
    fixture.write_results("passed");
    // The run directory no longer holds the registration's request.
    let mut request: Value =
        serde_json::from_slice(&fs::read(fixture.directory.join("request.json")).unwrap()).unwrap();
    request["creatorPid"] = json!(u64::from(std::process::id()) + 1);
    fs::write(
        fixture.directory.join("request.json"),
        serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    task_end(&fixture);
    assert_eq!(carried_run(&fixture), None);
    assert_eq!(
        refusal_code(&fixture),
        Some(TestRunNativeCompletionRefusalCode::ArtifactRequestMismatch)
    );
}

#[test]
fn a_cut_output_without_claude_provenance_stays_unread() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    // The ordinary recorder of another runtime: its cut output is not read.
    let mut fixture = RegisteredLauncherTurn::committed();
    let head = format!("RUN {}", test_run_display_path(&fixture.directory));
    cut_result(&mut fixture, &head);
    assert_eq!(carried_run(&fixture), None);
    assert_eq!(refusal_code(&fixture), None);
}

#[test]
fn a_launcher_run_in_the_background_settles_when_claude_reports_its_end() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = claude_registered("claude-run-in-background", false);
    admit_call(&fixture, true);
    background_result(&mut fixture, false);
    assert_eq!(
        carried_run(&fixture),
        None,
        "the launch result names no run"
    );
    // The run ends and its receipt arrives before Claude reports the end.
    let (_, settlement) = fixture.receive_until(true, None);
    assert_eq!(settlement, EngramVerificationRecordSettlement::NoLaunchRow);
    task_end(&fixture);
    assert_eq!(
        carried_run(&fixture).as_deref(),
        Some(fixture.run_id.as_str())
    );
    assert_eq!(
        settle_held(&fixture),
        EngramVerificationRecordSettlement::Settled
    );
}

#[test]
fn a_background_end_does_not_take_a_run_that_is_still_running() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    // The only registration in the window belongs to a run that has not
    // ended, so this launcher, which has ended, did not run it.
    let mut fixture = claude_registered("claude-still-running", false);
    admit_call(&fixture, true);
    background_result(&mut fixture, false);
    task_end(&fixture);
    assert_eq!(carried_run(&fixture), None);
    assert_eq!(
        refusal_code(&fixture),
        Some(TestRunNativeCompletionRefusalCode::ArtifactLocatorMissing)
    );
    // Its later end attaches nothing either: nothing looks again.
    let (_, settlement) = fixture.receive_until(true, None);
    assert_eq!(settlement, EngramVerificationRecordSettlement::NoLaunchRow);
}

#[test]
fn a_detached_launch_ending_in_the_background_attaches_nothing_and_says_why() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = claude_registered_as(
        "claude-detached-background",
        true,
        "node scripts/test-launcher.mjs full --input commit --detach --notify coordinator",
    );
    admit_call(&fixture, true);
    background_result(&mut fixture, false);
    fixture.write_results("passed");
    task_end(&fixture);
    assert_eq!(carried_run(&fixture), None);
    assert_eq!(
        refusal_code(&fixture),
        Some(TestRunNativeCompletionRefusalCode::TerminalNotConfirmed)
    );
}

#[test]
fn a_run_another_uncarried_launch_could_have_started_is_not_attached() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = claude_registered("claude-two-launch-rows", false);
    // A second background call of the same command, started within the
    // run's launch slack, that carries no run.
    let second = "second-call";
    let cwd = fixture
        .turn
        .record(|record| record.engram.observed_source_root().unwrap().root.clone());
    let command = fixture.command.clone();
    fixture
        .recorder
        .command_started_in(second, &command, Some(&command), Some(&cwd))
        .unwrap();
    admit_call(&fixture, true);
    admit_call_for(&fixture, second, true);
    background_result(&mut fixture, false);
    background_result_for(&mut fixture, second, false);
    fixture.write_results("passed");
    task_end(&fixture);
    assert_eq!(carried_run(&fixture), None);
    assert_eq!(launch_row_for(&fixture, second).artifact, None);
    assert_eq!(
        refusal_code(&fixture),
        Some(TestRunNativeCompletionRefusalCode::ArtifactLocatorAmbiguous)
    );
}

#[test]
fn a_test_run_directly_beside_the_launch_does_not_contest_its_run() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = claude_registered("claude-direct-test-beside", false);
    // A test runner run directly in the same worktree and window, carrying
    // no run: its row is a non-full row like the launcher's, but it started
    // no launcher, so it could not have started this run.
    let second = "direct-test";
    let direct = "cargo test --bin termal locators";
    let cwd = fixture
        .turn
        .record(|record| record.engram.observed_source_root().unwrap().root.clone());
    fixture
        .recorder
        .command_started_in(second, direct, Some(direct), Some(&cwd))
        .unwrap();
    let beside = launch_row_for(&fixture, second);
    assert_eq!(beside.artifact, None);
    assert_eq!(beside.summary.preset, launch_row(&fixture).summary.preset);
    assert_eq!(beside.launcher, Some(TestRunCommandLauncher::Direct));
    admit_call(&fixture, true);
    background_result(&mut fixture, false);
    fixture.write_results("passed");
    task_end(&fixture);
    assert_eq!(
        carried_run(&fixture).as_deref(),
        Some(fixture.run_id.as_str())
    );
    assert_eq!(refusal_code(&fixture), None);
}

#[test]
fn a_rival_launch_row_with_no_readable_start_contests_the_match() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = claude_registered("claude-unplaced-rival", false);
    let second = "second-call";
    let cwd = fixture
        .turn
        .record(|record| record.engram.observed_source_root().unwrap().root.clone());
    let command = fixture.command.clone();
    fixture
        .recorder
        .command_started_in(second, &command, Some(&command), Some(&cwd))
        .unwrap();
    // A persisted rival row whose start cannot be read: it could have
    // launched any run, so it contests this one.
    let rival = launch_row_for(&fixture, second).execution_id;
    fixture
        .turn
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_observations
        .update(&rival, |row| {
            row.summary.started_at = Some("not a time".to_owned())
        });
    admit_call(&fixture, true);
    background_result(&mut fixture, false);
    fixture.write_results("passed");
    task_end(&fixture);
    assert_eq!(carried_run(&fixture), None);
    assert_eq!(
        refusal_code(&fixture),
        Some(TestRunNativeCompletionRefusalCode::ArtifactLocatorAmbiguous)
    );
}

#[test]
fn a_background_focused_run_with_an_escaped_space_attaches_and_settles() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    // Bash passes `crates/my\ crate/Cargo.toml` as one word, and the launcher
    // records that one word; the row's command must read it the same way.
    let mut fixture = claude_registered_focused(
        "claude-escaped-space",
        r"node scripts/test-launcher.mjs focused --input commit -- cargo test --manifest-path crates/my\ crate/Cargo.toml",
        "cargo",
        &["test", "--manifest-path", "crates/my crate/Cargo.toml"],
    );
    admit_call(&fixture, true);
    background_result(&mut fixture, false);
    let (_, settlement) = fixture.receive_until(true, None);
    assert_eq!(settlement, EngramVerificationRecordSettlement::NoLaunchRow);
    task_end(&fixture);
    assert_eq!(
        carried_run(&fixture).as_deref(),
        Some(fixture.run_id.as_str())
    );
    assert_eq!(
        settle_held(&fixture),
        EngramVerificationRecordSettlement::Settled
    );
}

#[test]
fn a_background_focused_run_whose_test_command_names_detach_settles() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    // After `--` the launcher passes `--detach` on to the test command, so
    // the run is an ordinary one and its end attaches it.
    let mut fixture = claude_registered_focused(
        "claude-detach-after-separator",
        "node scripts/test-launcher.mjs focused --input commit -- cargo test -- --skip --detach",
        "cargo",
        &["test", "--", "--skip", "--detach"],
    );
    admit_call(&fixture, true);
    background_result(&mut fixture, false);
    let (_, settlement) = fixture.receive_until(true, None);
    assert_eq!(settlement, EngramVerificationRecordSettlement::NoLaunchRow);
    task_end(&fixture);
    assert_eq!(
        carried_run(&fixture).as_deref(),
        Some(fixture.run_id.as_str())
    );
    assert_eq!(
        settle_held(&fixture),
        EngramVerificationRecordSettlement::Settled
    );
}

#[test]
fn a_background_focused_run_whose_last_word_ends_in_an_escaped_space_settles() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    // Bash passes `locators\ ` as one word ending in a space.
    let mut fixture = claude_registered_focused(
        "claude-trailing-escaped-space",
        r"node scripts/test-launcher.mjs focused --input commit -- cargo test -- --skip locators\ ",
        "cargo",
        &["test", "--", "--skip", "locators "],
    );
    admit_call(&fixture, true);
    background_result(&mut fixture, false);
    let (_, settlement) = fixture.receive_until(true, None);
    assert_eq!(settlement, EngramVerificationRecordSettlement::NoLaunchRow);
    task_end(&fixture);
    assert_eq!(
        carried_run(&fixture).as_deref(),
        Some(fixture.run_id.as_str())
    );
    assert_eq!(
        settle_held(&fixture),
        EngramVerificationRecordSettlement::Settled
    );
}

#[test]
fn a_background_focused_run_whose_line_ends_in_a_line_continuation_settles() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    // Bash removes an unescaped backslash and the line break after it, and
    // whitespace after that only separates words; an escaped backslash stays
    // in the word, and the break only ends it. Before a CRLF the backslash
    // escapes the CR, which stays in the word.
    for (label, command, last) in [
        (
            "claude-trailing-continuation",
            "node scripts/test-launcher.mjs focused --input commit -- cargo test -- --skip locators\\\n",
            "locators",
        ),
        (
            "claude-trailing-continuation-then-space",
            "node scripts/test-launcher.mjs focused --input commit -- cargo test -- --skip locators\\\n ",
            "locators",
        ),
        (
            "claude-trailing-continuation-then-tab",
            "node scripts/test-launcher.mjs focused --input commit -- cargo test -- --skip locators\\\n\t",
            "locators",
        ),
        (
            "claude-quoted-operator-then-continuation",
            "node scripts/test-launcher.mjs focused --input commit -- cargo test -- --skip 'alpha;beta' \\\n ",
            "alpha;beta",
        ),
        (
            "claude-quoted-operator-joined-continuation",
            "node scripts/test-launcher.mjs focused --input commit -- cargo test -- --skip 'alpha|beta;gamma'\\\n",
            "alpha|beta;gamma",
        ),
        (
            "claude-quoted-operator-escaped-space-then-continuation",
            "node scripts/test-launcher.mjs focused --input commit -- cargo test -- --skip 'alpha;beta'\\ \\\n ",
            "alpha;beta ",
        ),
        (
            "claude-quoted-operator-escaped-space",
            "node scripts/test-launcher.mjs focused --input commit -- cargo test -- --skip 'alpha;beta'\\ ",
            "alpha;beta ",
        ),
        (
            "claude-trailing-escaped-backslash",
            "node scripts/test-launcher.mjs focused --input commit -- cargo test -- --skip locators\\\\\n",
            "locators\\",
        ),
        (
            "claude-trailing-escaped-carriage-return",
            "node scripts/test-launcher.mjs focused --input commit -- cargo test -- --skip locators\\\r\n",
            "locators\r",
        ),
    ] {
        let mut fixture =
            claude_registered_focused(label, command, "cargo", &["test", "--", "--skip", last]);
        admit_call(&fixture, true);
        background_result(&mut fixture, false);
        let (_, settlement) = fixture.receive_until(true, None);
        assert_eq!(settlement, EngramVerificationRecordSettlement::NoLaunchRow);
        task_end(&fixture);
        assert_eq!(
            carried_run(&fixture).as_deref(),
            Some(fixture.run_id.as_str()),
            "{command:?}"
        );
        assert_eq!(
            settle_held(&fixture),
            EngramVerificationRecordSettlement::Settled
        );
    }
}

#[test]
fn a_focused_run_launched_through_a_shell_wrapper_attaches_nothing_and_says_why() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    // A wrapper's script reaches the launcher through another shell's
    // quoting, which is not matched: only a direct launcher line is. The
    // launcher recorded exactly the args each line would pass on.
    for (label, command) in [
        (
            "claude-bash-c-wrapper",
            r#"bash -c "node scripts/test-launcher.mjs focused --input commit -- cargo test --manifest-path crates/sample/Cargo.toml""#,
        ),
        (
            "claude-pwsh-quoted-wrapper",
            r#"pwsh -Command "node scripts/test-launcher.mjs focused --input commit -- cargo test --manifest-path crates/sample/Cargo.toml""#,
        ),
        (
            "claude-pwsh-unquoted-wrapper",
            "pwsh -Command node scripts/test-launcher.mjs focused --input commit -- cargo test --manifest-path crates/sample/Cargo.toml",
        ),
        (
            "claude-cmd-wrapper",
            r#"cmd /c "node scripts/test-launcher.mjs focused --input commit -- cargo test --manifest-path crates/sample/Cargo.toml""#,
        ),
    ] {
        let mut fixture = claude_registered_focused(
            label,
            command,
            "cargo",
            &["test", "--manifest-path", "crates/sample/Cargo.toml"],
        );
        admit_call(&fixture, true);
        background_result(&mut fixture, false);
        fixture.write_results("passed");
        task_end(&fixture);
        assert_eq!(carried_run(&fixture), None, "{command}");
        assert_eq!(
            launch_row(&fixture).native_completion_refusal,
            Some(TestRunNativeCompletionRefusal::new(
                TestRunNativeCompletionPhase::ArtifactAssociation,
                TestRunNativeCompletionRefusalCode::ArtifactRequestMismatch,
                &[(TestRunNativeCompletionFact::ObservationMatches, false)],
            )),
            "{command}"
        );
    }
}

#[test]
fn a_registration_whose_args_cannot_be_read_does_not_match_a_command_without_arguments() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let command = "node scripts/test-launcher.mjs focused --input commit -- pytest";
    // The row's own run, registered with an empty args array, is still running
    // and already carried by another call of the session.
    let mut fixture = claude_registered_focused("claude-missing-args", command, "pytest", &[]);
    let cwd = fixture
        .turn
        .record(|record| record.engram.observed_source_root().unwrap().root.clone());
    fixture
        .recorder
        .command_started_in("other-call", command, Some(command), Some(&cwd))
        .unwrap();
    fixture
        .recorder
        .command_completed_with_capture(
            "other-call",
            command,
            &format!("RUN {}", test_run_display_path(&fixture.directory)),
            CommandStatus::Success,
            EngramCommandExit::Code(0),
            false,
            false,
        )
        .unwrap();
    assert_eq!(
        launch_row_for(&fixture, "other-call")
            .artifact
            .map(|artifact| artifact.run_id),
        Some(fixture.run_id.clone())
    );
    // So the only uncarried run in the window is an ended one that recorded
    // its stage with no args array.
    let mut request: Value =
        serde_json::from_slice(&fs::read(fixture.directory.join("request.json")).unwrap()).unwrap();
    let other_id = format!("test-{}", Uuid::new_v4());
    let other = fixture.directory.parent().unwrap().join(&other_id);
    fs::create_dir_all(&other).unwrap();
    request["runId"] = json!(other_id);
    request["stages"] = json!([{ "name": "focused", "command": "pytest" }]);
    fs::write(
        other.join("request.json"),
        serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    fs::write(
        other.join("results.json"),
        serde_json::to_vec(&json!({ "runId": other_id, "state": "passed",
            "started": fixture.started, "ended": test_run_timestamp(), "exitCode": 0,
            "stages": [{ "name": "focused", "state": "passed", "code": 0 }] }))
        .unwrap(),
    )
    .unwrap();
    assert!(
        fixture
            .turn
            .state
            .register_test_run(
                &fixture.turn.session_id,
                RegisterTestRunRequest {
                    run_id: other_id,
                    run_directory: test_run_display_path(&other),
                },
            )
            .unwrap()
            .registered
    );
    admit_call(&fixture, true);
    background_result(&mut fixture, false);
    task_end(&fixture);
    assert_eq!(carried_run(&fixture), None);
    assert_eq!(
        launch_row(&fixture).native_completion_refusal,
        Some(TestRunNativeCompletionRefusal::new(
            TestRunNativeCompletionPhase::ArtifactAssociation,
            TestRunNativeCompletionRefusalCode::ArtifactRequestMismatch,
            &[(TestRunNativeCompletionFact::ObservationMatches, false)],
        ))
    );
}

#[test]
fn a_background_end_does_not_take_a_focused_run_of_another_command() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    // The row's own launcher never registered. The only registration in its
    // window ran another focused command, launched by a route that made no
    // launch row, so it is not this row's run.
    let turn = RegisteredLauncherTurn::committed_turn("claude-other-focused-command");
    let cwd = turn.record(|record| record.engram.observed_source_root().unwrap().root.clone());
    let mut recorder = turn.recorder();
    recorder.set_observation_provenance(claude_provenance(&turn));
    recorder
        .command_started_in(KEY, COMMITTED_COMMAND, Some(COMMITTED_COMMAND), Some(&cwd))
        .unwrap();
    let mut fixture = RegisteredLauncherTurn::after_start_with_stage(
        turn,
        recorder,
        true,
        &["--test", "scripts/other.test.mjs"],
    );
    admit_call(&fixture, true);
    background_result(&mut fixture, false);
    fixture.write_results("passed");
    task_end(&fixture);
    assert_eq!(carried_run(&fixture), None);
    assert_eq!(
        launch_row(&fixture).native_completion_refusal,
        Some(TestRunNativeCompletionRefusal::new(
            TestRunNativeCompletionPhase::ArtifactAssociation,
            TestRunNativeCompletionRefusalCode::ArtifactRequestMismatch,
            &[(TestRunNativeCompletionFact::ObservationMatches, false)],
        ))
    );
}

#[test]
fn an_unindexed_full_gate_moved_to_the_background_settles_its_held_receipt_at_its_end() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    // A registered full gate, never indexed and with no notification target,
    // moved to the background by the tool's time limit. Its receipt arrives
    // before Claude reports the task's end.
    let mut fixture = claude_registered("claude-moved-full-gate", true);
    assert!(
        fixture
            .turn
            .state
            .inner
            .lock()
            .unwrap()
            .test_run_registrations[&fixture.run_id]
            .preset
            == TestRunPreset::Full
    );
    admit_call(&fixture, false);
    background_result(&mut fixture, true);
    let (_, settlement) = fixture.receive_until(true, None);
    assert_eq!(settlement, EngramVerificationRecordSettlement::NoLaunchRow);
    task_end(&fixture);
    assert_eq!(
        carried_run(&fixture).as_deref(),
        Some(fixture.run_id.as_str())
    );
    // The producer's next look at the held receipt now settles it.
    assert_eq!(
        settle_held(&fixture),
        EngramVerificationRecordSettlement::Settled
    );
}

#[test]
fn two_matching_registrations_attach_nothing_and_say_why() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = claude_registered("claude-ambiguous-background", false);
    fixture.write_results("passed");
    // A second ended run of the same session and worktree in the same window.
    let request: Value =
        serde_json::from_slice(&fs::read(fixture.directory.join("request.json")).unwrap()).unwrap();
    let other_id = format!("test-{}", Uuid::new_v4());
    let other = fixture.directory.parent().unwrap().join(&other_id);
    fs::create_dir_all(&other).unwrap();
    let mut other_request = request;
    other_request["runId"] = json!(other_id);
    fs::write(
        other.join("request.json"),
        serde_json::to_vec(&other_request).unwrap(),
    )
    .unwrap();
    let mut other_results: Value =
        serde_json::from_slice(&fs::read(fixture.directory.join("results.json")).unwrap()).unwrap();
    other_results["runId"] = json!(other_id);
    fs::write(
        other.join("results.json"),
        serde_json::to_vec(&other_results).unwrap(),
    )
    .unwrap();
    assert!(
        fixture
            .turn
            .state
            .register_test_run(
                &fixture.turn.session_id,
                RegisterTestRunRequest {
                    run_id: other_id,
                    run_directory: test_run_display_path(&other),
                },
            )
            .unwrap()
            .registered
    );
    admit_call(&fixture, true);
    background_result(&mut fixture, false);
    task_end(&fixture);
    assert_eq!(carried_run(&fixture), None);
    assert_eq!(
        refusal_code(&fixture),
        Some(TestRunNativeCompletionRefusalCode::ArtifactLocatorAmbiguous)
    );
    let (_, settlement) = fixture.receive_until(true, None);
    assert_eq!(settlement, EngramVerificationRecordSettlement::NoLaunchRow);
}

/// Registers, beside the fixture's own run, another ended run of the same
/// session, worktree and command whose request starts at `started`.
fn register_second_ended_run(fixture: &RegisteredLauncherTurn, started: &str) {
    let mut request: Value =
        serde_json::from_slice(&fs::read(fixture.directory.join("request.json")).unwrap()).unwrap();
    let other_id = format!("test-{}", Uuid::new_v4());
    let other = fixture.directory.parent().unwrap().join(&other_id);
    fs::create_dir_all(&other).unwrap();
    request["runId"] = json!(other_id);
    request["started"] = json!(started);
    fs::write(
        other.join("request.json"),
        serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    fs::write(
        other.join("results.json"),
        serde_json::to_vec(&json!({ "runId": other_id, "state": "passed",
            "started": started, "ended": test_run_timestamp(), "exitCode": 0,
            "stages": [{ "name": "focused", "state": "passed", "code": 0 }] }))
        .unwrap(),
    )
    .unwrap();
    assert!(
        fixture
            .turn
            .state
            .register_test_run(
                &fixture.turn.session_id,
                RegisterTestRunRequest {
                    run_id: other_id,
                    run_directory: test_run_display_path(&other),
                },
            )
            .unwrap()
            .registered
    );
}

#[test]
fn a_second_registration_whose_run_has_not_ended_still_makes_the_match_ambiguous() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    // The row's own run never ended: its launcher may have died before
    // writing results. Another ended run of the same command in the window
    // is then no unique match.
    let mut fixture = claude_registered("claude-one-running-one-ended", false);
    let started = fixture.started.clone();
    register_second_ended_run(&fixture, &started);
    admit_call(&fixture, true);
    background_result(&mut fixture, false);
    task_end(&fixture);
    assert_eq!(carried_run(&fixture), None);
    assert_eq!(
        refusal_code(&fixture),
        Some(TestRunNativeCompletionRefusalCode::ArtifactLocatorAmbiguous)
    );
}

#[test]
fn a_registration_whose_start_cannot_be_read_makes_the_match_ambiguous() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    // A registration whose start cannot be read could have started within
    // the window, so the row's own ended run is no unique match.
    let mut fixture = claude_registered("claude-unreadable-start", false);
    fixture.write_results("passed");
    register_second_ended_run(&fixture, "not a time");
    admit_call(&fixture, true);
    background_result(&mut fixture, false);
    task_end(&fixture);
    assert_eq!(carried_run(&fixture), None);
    assert_eq!(
        refusal_code(&fixture),
        Some(TestRunNativeCompletionRefusalCode::ArtifactLocatorAmbiguous)
    );
}

#[test]
fn a_full_gate_launched_through_a_shell_wrapper_attaches_nothing_and_says_why() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    // Only a direct launcher line is matched to a registration, in every
    // launcher mode.
    for (label, command) in [
        (
            "claude-full-bash-c-wrapper",
            r#"bash -c "node scripts/test-launcher.mjs full --input commit""#,
        ),
        (
            "claude-full-pwsh-wrapper",
            r#"pwsh -Command "node scripts/test-launcher.mjs full --input commit""#,
        ),
    ] {
        let mut fixture = claude_registered_as(label, true, command);
        admit_call(&fixture, true);
        background_result(&mut fixture, false);
        fixture.write_results("passed");
        task_end(&fixture);
        assert_eq!(carried_run(&fixture), None, "{command}");
        assert_eq!(
            launch_row(&fixture).native_completion_refusal,
            Some(TestRunNativeCompletionRefusal::new(
                TestRunNativeCompletionPhase::ArtifactAssociation,
                TestRunNativeCompletionRefusalCode::ArtifactRequestMismatch,
                &[(TestRunNativeCompletionFact::ObservationMatches, false)],
            )),
            "{command}"
        );
    }
}

#[test]
fn a_background_end_takes_only_a_run_of_the_mode_its_command_names() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    // A live launcher whose only registration in the window is a focused run.
    let turn = RegisteredLauncherTurn::committed_turn("claude-live-mode");
    let cwd = turn.record(|record| record.engram.observed_source_root().unwrap().root.clone());
    let live = "node scripts/test-launcher.mjs live";
    let mut recorder = turn.recorder();
    recorder.set_observation_provenance(claude_provenance(&turn));
    recorder
        .command_started_in(KEY, live, Some(live), Some(&cwd))
        .unwrap();
    let mut fixture = RegisteredLauncherTurn::after_start_with_input(turn, recorder, true);
    fixture.command = live.to_owned();
    admit_call(&fixture, true);
    background_result(&mut fixture, false);
    fixture.write_results("passed");
    task_end(&fixture);
    assert_eq!(carried_run(&fixture), None);
    assert_eq!(
        refusal_code(&fixture),
        Some(TestRunNativeCompletionRefusalCode::ArtifactLocatorMissing)
    );
}

#[test]
fn a_run_that_ended_before_a_later_launch_is_not_that_launchs_match() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    // The provider-boundary clock pins every start to past instants, with no
    // sleep: the run's start, its end one second later, and the later call's
    // start two seconds later all precede the task end, so only the run's
    // end before that call's start can refuse it.
    struct StartClock(Option<(String, Option<String>)>);
    impl Drop for StartClock {
        fn drop(&mut self) {
            TEST_ENGRAM_COMMAND_START_TIMING.with(|slot| slot.replace(self.0.take()));
        }
    }
    let past = (chrono::Utc::now() - chrono::TimeDelta::seconds(30))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let _clock = StartClock(
        TEST_ENGRAM_COMMAND_START_TIMING.with(|slot| slot.replace(Some((past.clone(), None)))),
    );
    let mut fixture = claude_registered("claude-earlier-ended-run", false);
    assert_eq!(fixture.started, past);
    assert_eq!(launch_row(&fixture).summary.started_at, Some(past.clone()));
    // The registered run already ended, unattached, before the later call
    // started, though it started within that call's launch slack.
    let mut results: Value =
        serde_json::from_slice(&fs::read(fixture.directory.join("results.json")).unwrap()).unwrap();
    let at = |seconds: i64| {
        (EngramHost::check_timestamp(&fixture.started).unwrap()
            + chrono::TimeDelta::seconds(seconds))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    };
    let ended = at(1);
    results["state"] = json!("passed");
    results["ended"] = json!(ended);
    results["exitCode"] = json!(0);
    fs::write(
        fixture.directory.join("results.json"),
        serde_json::to_vec(&results).unwrap(),
    )
    .unwrap();
    // A later background launcher of the same mode, which never registers.
    let later = "later-call";
    let cwd = fixture
        .turn
        .record(|record| record.engram.observed_source_root().unwrap().root.clone());
    let command = fixture.command.clone();
    // The later call starts two seconds after the run: inside the slack,
    // after its end.
    TEST_ENGRAM_COMMAND_START_TIMING.with(|slot| slot.replace(Some((at(2), None))));
    fixture
        .recorder
        .command_started_in(later, &command, Some(&command), Some(&cwd))
        .unwrap();
    assert_eq!(
        launch_row_for(&fixture, later).summary.started_at,
        Some(at(2))
    );
    admit_call_for(&fixture, later, true);
    background_result_for(&mut fixture, later, false);
    task_end_for(&fixture, later);
    assert_eq!(launch_row_for(&fixture, later).artifact, None);
    assert_eq!(
        refusal_code_for(&fixture, later),
        Some(TestRunNativeCompletionRefusalCode::ArtifactLocatorMissing)
    );
}
