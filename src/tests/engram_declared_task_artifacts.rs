// Original Claude task END without a native reservation. Uses production
// provider parsing and artifact readers with disposable Flutter JSON fixtures;
// no Flutter process or consumer verification is claimed by these tests.
use super::*;

#[path = "engram_declared_native_task_artifacts.rs"]
mod native_route;

const COMMAND: &str = "flutter test --file-reporter json:.tmp/tests.json";
const DECLARATION: &str = "[[test]]\ncommand = \"flutter test --file-reporter json:.tmp/tests.json\"\ncwd = \".\"\nartifact = \".tmp/tests.json\"\n";
const PASS: &[u8] = include_bytes!("fixtures/declared-dart/flutter-pass.jsonl");
const FAIL: &[u8] = include_bytes!("fixtures/declared-dart/flutter-fail.jsonl");

fn ordinary(membership: &str) -> NativeFixture {
    let fixture = NativeFixture::new("ordinary-declared-task-end", true);
    {
        // The same ordinary observation owner a provider handoff starts. It
        // carries command facts, with no grant or verification reservation.
        let mut inner = fixture.fixture.state.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.session).unwrap();
        inner.sessions[index].engram.unadmitted_active_turn = Some(
            EngramRuntimeObservedTurn::started("ordinary-task-turn".into(), 1),
        );
    }
    fs::write(
        fixture.fixture.root.join(ENGRAM_DECLARATION_FILE),
        DECLARATION,
    )
    .unwrap();
    if membership == "committed" {
        run_git_test_command(
            &fixture.fixture.root,
            &["add", "--", ENGRAM_DECLARATION_FILE],
        );
    } else if membership == "untracked" {
        run_git_test_command(
            &fixture.fixture.root,
            &["rm", "--cached", "--", ENGRAM_DECLARATION_FILE],
        );
    } else {
        assert_eq!(membership, "dirty-tracked");
        // NativeFixture committed this path already. This is an ordinary
        // dirty edit, distinct from a declaration absent from HEAD.
        return fixture;
    }
    run_git_test_command(
        &fixture.fixture.root,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "--no-gpg-sign",
            "-m",
            "ordinary declaration membership control",
        ],
    );
    fixture.source_owner.refresh_commit();
    if membership == "untracked" {
        let snapshot = fixture
            .source_owner
            .snapshot(std::time::Instant::now() + REVIEW_FREEZE_TIMEOUT, None)
            .map_err(|error| format!("{error:#}"));
        assert!(
            snapshot
                .as_ref()
                .unwrap_err()
                .contains("declaration_not_committed")
        );
        fixture.install_snapshot(snapshot);
        fixture.fixture.state.prepare_declared_run_before_handoff(
            &fixture.session,
            &fixture.token,
            1,
        );
        let inner = fixture.fixture.state.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.session).unwrap()];
        assert_eq!(
            record
                .engram
                .declared_run_turn
                .as_ref()
                .unwrap()
                .opening_refusal_code,
            Some("declaration_not_committed")
        );
    }
    fixture
}

fn start(fixture: &NativeFixture, recorder: &mut SessionRecorder, mode: &str) {
    start_route(fixture, recorder, mode, false);
}

fn start_route(fixture: &NativeFixture, recorder: &mut SessionRecorder, mode: &str, native: bool) {
    let mut frame = fixture.frame();
    let path = identity_git_path(&fs::canonicalize(&fixture.fixture.root).unwrap())
        .unwrap()
        .replace('\\', "/");
    frame["input"]["command"] = json!(format!("pushd \"{path}\" && {COMMAND}"));
    frame["input"]["run_in_background"] = json!(mode == "background");
    register_claude_tool_use(&frame, &mut fixture.claude_state.lock().unwrap(), recorder).unwrap();
    if native {
        fixture.wait();
    }
    let row = observation(fixture);
    assert!(
        row.declared_binding.is_some(),
        "actual recognized start retains its binding"
    );
    assert!(
        row.declared_native.is_some() == native,
        "only an admitted native start may retain a reservation"
    );
    assert_eq!(fixture.execution().is_some(), native);
}

fn artifact(fixture: &NativeFixture, bytes: &[u8]) {
    fs::create_dir_all(fixture.fixture.root.join(".tmp")).unwrap();
    fs::write(fixture.fixture.root.join(".tmp/tests.json"), bytes).unwrap();
}

fn result(fixture: &NativeFixture, recorder: &mut SessionRecorder, mode: &str) {
    let mut metadata = json!({"stdout":"provider preview contains no test verdict"});
    if mode == "moved" {
        metadata["backgroundTaskId"] = json!("original-task");
    }
    handle_claude_tool_result(
        &json!({"tool_use_result":metadata,"message":{"content":[{
            "type":"tool_result", "tool_use_id":"native-tool", "is_error":false,"content":""
        }]}}),
        &mut fixture.claude_state.lock().unwrap(),
        recorder,
    )
    .unwrap();
}

fn task_end(fixture: &NativeFixture, status: &str, token: &RuntimeToken) {
    let work = claude_frame_work(
        &json!({"type":"system","subtype":"task_notification",
        "status":status,"tool_use_id":"native-tool"}),
        false,
    );
    fixture
        .fixture
        .state
        .settle_claude_frame(&fixture.session, token, &work);
}

fn observation(fixture: &NativeFixture) -> TestRunCommandObservation {
    let inner = fixture.fixture.state.inner.lock().unwrap();
    let rows = inner
        .test_run_observations
        .values()
        .filter(|row| row.summary.owner_session_id.as_deref() == Some(fixture.session.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1);
    rows[0].clone()
}

fn no_native_credit(fixture: &NativeFixture) {
    let inner = fixture.fixture.state.inner.lock().unwrap();
    assert!(inner.test_run_registrations.is_empty());
    assert!(inner.test_runs.declared_executions.is_empty());
    let row = inner.test_run_observations.values().next().unwrap();
    assert!(row.evidence.is_empty());
    assert!(
        row.artifact.is_none(),
        "ordinary artifact verdict is not launcher registration"
    );
    assert!(row.declared_native.is_none());
}

#[test]
fn ordinary_task_end_names_the_missing_exit_with_any_declaration_membership() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for membership in ["committed", "dirty-tracked", "untracked"] {
        let fixture = ordinary(membership);
        let mut recorder = fixture.recorder();
        start(&fixture, &mut recorder, "background");
        // A complete artifact at launch cannot supply the original END.
        artifact(&fixture, PASS);
        result(&fixture, &mut recorder, "background");
        let running = observation(&fixture);
        assert_eq!(running.summary.state, TestRunState::Running, "{membership}");
        assert!(running.summary.ended_at.is_none());
        fixture.later_prompt();
        task_end(&fixture, "completed", &fixture.token);
        let terminal = observation(&fixture);
        assert_eq!(
            terminal.summary.state,
            TestRunState::Unknown,
            "{membership}: {:?}",
            terminal.summary.error
        );
        assert!(terminal.summary.ended_at.is_some());
        assert_eq!(
            terminal.summary.exit_code, None,
            "task notice supplied no OS exit"
        );
        assert!(
            terminal
                .summary
                .error
                .as_deref()
                .unwrap()
                .contains("without a usable exit status")
        );
        no_native_credit(&fixture);
        artifact(&fixture, FAIL);
        task_end(&fixture, "completed", &fixture.token);
        assert_eq!(
            observation(&fixture),
            terminal,
            "duplicate END cannot rejudge a replaced artifact"
        );
    }
}

#[test]
fn foreground_moved_by_provider_waits_for_its_task_and_keeps_artifact_failure() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = ordinary("untracked");
    let mut recorder = fixture.recorder();
    start(&fixture, &mut recorder, "moved");
    artifact(&fixture, FAIL);
    result(&fixture, &mut recorder, "moved");
    assert!(observation(&fixture).summary.ended_at.is_none());
    task_end(&fixture, "completed", &fixture.token);
    let terminal = observation(&fixture);
    assert_eq!(
        terminal.summary.state,
        TestRunState::Failed,
        "{:?}",
        terminal.summary.error
    );
    assert!(terminal.summary.error.is_some());
    assert_eq!(terminal.summary.exit_code, None);
    no_native_credit(&fixture);
}

#[test]
fn original_task_end_reports_the_missing_or_invalid_artifact_reason() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for bytes in [None, Some(b"incomplete or invalid artifact".as_slice())] {
        let fixture = ordinary("committed");
        let mut recorder = fixture.recorder();
        start(&fixture, &mut recorder, "background");
        if let Some(bytes) = bytes {
            artifact(&fixture, bytes);
        }
        result(&fixture, &mut recorder, "background");
        let binding = observation(&fixture).declared_binding.unwrap();
        let EngramTrxVerdict::Unknown(expected) = engram_declared_artifact_result(&binding).verdict
        else {
            panic!("fixture must have an actual artifact refusal");
        };
        task_end(&fixture, "completed", &fixture.token);
        let terminal = observation(&fixture);
        assert_eq!(terminal.summary.state, TestRunState::Unknown);
        assert_eq!(terminal.summary.error, Some(expected));
        assert!(terminal.summary.ended_at.is_some());
        no_native_credit(&fixture);
    }
}

#[test]
fn genuine_foreground_artifact_verdict_is_unchanged_without_native_credit() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for (bytes, expected) in [(PASS, TestRunState::Passed), (FAIL, TestRunState::Failed)] {
        let fixture = ordinary("untracked");
        let mut recorder = fixture.recorder();
        start(&fixture, &mut recorder, "foreground");
        artifact(&fixture, bytes);
        result(&fixture, &mut recorder, "foreground");
        let terminal = observation(&fixture);
        assert_eq!(terminal.summary.state, expected);
        assert!(terminal.summary.ended_at.is_some());
        assert_eq!(
            terminal.summary.exit_code, None,
            "ReportedSuccess is not a numeric OS exit"
        );
        no_native_credit(&fixture);
    }
}

#[test]
fn ordinary_foreground_numeric_exit_keeps_its_existing_precedence() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for (bytes, exit, expected) in [
        (PASS, 0, TestRunState::Passed),
        (PASS, 7, TestRunState::Failed),
        (FAIL, 0, TestRunState::Failed),
    ] {
        let fixture = ordinary("committed");
        let mut recorder = fixture.recorder();
        start(&fixture, &mut recorder, "foreground");
        artifact(&fixture, bytes);
        recorder
            .command_completed_with_exit(
                "native-tool",
                COMMAND,
                "actual exit endpoint",
                if exit == 0 {
                    CommandStatus::Success
                } else {
                    CommandStatus::Error
                },
                EngramCommandExit::Code(exit),
            )
            .unwrap();
        let row = observation(&fixture);
        assert_eq!(row.summary.state, expected);
        assert_eq!(row.summary.exit_code, Some(exit));
        no_native_credit(&fixture);
    }
}

#[test]
fn foreground_persisted_output_without_an_exit_uses_the_declared_artifact() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for structured_metadata in [false, true] {
        let fixture = ordinary("untracked");
        let mut recorder = fixture.recorder();
        start(&fixture, &mut recorder, "foreground");
        artifact(&fixture, PASS);
        let spill = fixture.fixture.root.join(".tmp/provider-console.txt");
        let original = b"provider console is not an artifact and must not be read or changed";
        fs::write(&spill, original).unwrap();
        let preview = format!(
            "Output too large (743.5KB). Full output saved to: {}\nPreview contains no verdict or exit code",
            spill.display()
        );
        let mut frame = json!({"message":{"content":[{
            "type":"tool_result","tool_use_id":"native-tool","is_error":false,"content":preview
        }]}});
        if structured_metadata {
            frame["tool_use_result"] = json!({"stdout":"bounded preview", "persistedOutputPath":spill,
                "persistedOutputSize":743_500});
        }
        handle_claude_tool_result(
            &frame,
            &mut fixture.claude_state.lock().unwrap(),
            &mut recorder,
        )
        .unwrap();
        let terminal = observation(&fixture);
        assert_eq!(terminal.summary.state, TestRunState::Passed);
        assert!(terminal.summary.ended_at.is_some());
        assert_eq!(terminal.summary.exit_code, None);
        assert_eq!(fs::read(&spill).unwrap(), original);
        no_native_credit(&fixture);
    }
}

#[test]
fn ordinary_persisted_output_without_a_success_report_names_the_missing_exit() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for structured_metadata in [false, true] {
        let fixture = ordinary("committed");
        let mut recorder = fixture.recorder();
        start(&fixture, &mut recorder, "foreground");
        artifact(&fixture, PASS);
        let mut frame = json!({"message":{"content":[{
            "type":"tool_result", "tool_use_id":"native-tool", "is_error":true,
            "content":"<persisted-output>Output too large. Full output saved to: .tmp/provider-console.txt</persisted-output>"
        }]}});
        if structured_metadata {
            frame["tool_use_result"] = json!({"stdout":"bounded preview",
                "persistedOutputPath":".tmp/provider-console.txt", "persistedOutputSize":743_500});
        }
        handle_claude_tool_result(
            &frame,
            &mut fixture.claude_state.lock().unwrap(),
            &mut recorder,
        )
        .unwrap();
        let terminal = observation(&fixture);
        assert_eq!(terminal.summary.state, TestRunState::Unknown);
        assert_eq!(terminal.summary.exit_code, None);
        assert!(terminal.summary.ended_at.is_some());
        assert_eq!(
            terminal.summary.error.as_deref(),
            Some("the command ended without an exit status")
        );
        no_native_credit(&fixture);
    }
}

#[test]
fn replaced_runtime_cannot_close_the_original_task_and_interruption_is_not_pass() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = ordinary("dirty-tracked");
    let mut recorder = fixture.recorder();
    start(&fixture, &mut recorder, "background");
    artifact(&fixture, PASS);
    result(&fixture, &mut recorder, "background");
    task_end(
        &fixture,
        "completed",
        &RuntimeToken::Claude("a-replaced-runtime".into()),
    );
    assert!(observation(&fixture).summary.ended_at.is_none());
    task_end(&fixture, "killed", &fixture.token);
    let terminal = observation(&fixture);
    assert_eq!(terminal.summary.state, TestRunState::Unknown);
    assert!(terminal.summary.interrupted);
    assert!(terminal.summary.error.unwrap().contains("interrupted"));
    no_native_credit(&fixture);
}

#[test]
fn a_restored_row_without_its_original_binding_cannot_rebind_at_end() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = ordinary("committed");
    let mut recorder = fixture.recorder();
    start(&fixture, &mut recorder, "background");
    result(&fixture, &mut recorder, "background");
    let original = observation(&fixture);
    let mut legacy = serde_json::to_value(&original).unwrap();
    legacy.as_object_mut().unwrap().remove("declaredBinding");
    let restored: TestRunCommandObservation = serde_json::from_value(legacy).unwrap();
    assert!(restored.declared_binding.is_none());
    fixture
        .fixture
        .state
        .inner
        .lock()
        .unwrap()
        .test_run_observations
        .insert(original.execution_id, restored);
    artifact(&fixture, PASS);
    task_end(&fixture, "completed", &fixture.token);
    let terminal = observation(&fixture);
    assert_eq!(terminal.summary.state, TestRunState::Unknown);
    assert!(terminal.summary.ended_at.is_some());
    no_native_credit(&fixture);
}
