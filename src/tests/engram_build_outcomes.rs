// Owns Build terminal correlation and rejection diagnostics through the real
// recorder/ordinary-observation path. Synthetic launcher records are not live evidence.
// Does not own Build grammar, live-store admission, or carried-gate settlement.
// Child of engram_build_checks.rs; reuses its complete owned fixture lifecycle.
use super::*;

fn summary(turn: &EOCheckedTurn) -> String {
    diagnostic_summary(turn)
}

fn rejected_terminal(turn: &EOCheckedTurn, request: &Value, revision: &str, code: i64, reason: &str) {
    one_build(request, revision, "indeterminate");
    let text = summary(turn);
    assert!(text.contains(&format!("observed native exit {code}")),
        "the runtime terminal remains diagnostic, independent of credit: {text}");
    assert!(text.contains(reason), "must name rejection {reason}: {text}");
    assert!(!text.contains("without an exit status"), "{text}");
    assert!(text.len() <= ENGRAM_CHECK_SUMMARY_MAX_BYTES, "{text}");
    assert!(!diagnostic_refs(turn).contains(&format!("exit:{code}")),
        "a rejected observed exit supplies no eligible native reference");
}

fn typed_item(
    turn: &EOCheckedTurn,
    key: &str,
    command: &str,
    status: &str,
    code: Option<i64>,
) -> Value {
    let mut item = json!({
        "type":"commandExecution", "id":key, "command":command,
        "cwd":turn.root, "status":status, "aggregatedOutput":""
    });
    if let Some(code) = code {
        item["exitCode"] = json!(code);
    }
    item
}

fn complete_typed(turn: &EOCheckedTurn, recorder: &mut SessionRecorder, item: &Value) {
    handle_codex_app_server_item_completed(
        item,
        &turn.state,
        &turn.session_id,
        &mut CodexTurnState::default(),
        recorder,
    )
    .unwrap();
}

#[test]
fn engram_build_outcome_typed_native_pass_and_failure_reach_the_request() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for code in [0, 101] {
        let turn = EOCheckedTurn::start(&format!("build-outcome-typed-{code}"), true);
        let revision = turn.revision();
        let mut recorder = turn.recorder();
        handle_codex_app_server_item_started(
            &typed_item(&turn, "owned", "cargo build", "inProgress", None),
            &mut recorder,
        )
        .unwrap();
        turn.wait_for_snapshots();
        complete_typed(
            &turn,
            &mut recorder,
            &typed_item(&turn, "owned", "cargo build", "completed", Some(code)),
        );
        turn.wait_for_snapshots();
        one_build(
            &finish_observation(&turn),
            &revision,
            if code == 0 { "passed" } else { "failed" },
        );
    }
}

#[test]
fn engram_build_outcome_typed_yield_and_foreign_key_do_not_end_the_owned_attempt() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = EOCheckedTurn::start("build-outcome-yield", true);
    let revision = turn.revision();
    let mut recorder = turn.recorder();
    handle_codex_app_server_item_started(
        &typed_item(&turn, "owned", "cargo build", "inProgress", None),
        &mut recorder,
    )
    .unwrap();
    turn.wait_for_snapshots();
    complete_typed(
        &turn,
        &mut recorder,
        &typed_item(&turn, "owned", "cargo build", "inProgress", None),
    );
    complete_typed(
        &turn,
        &mut recorder,
        &typed_item(&turn, "foreign", "cargo build", "completed", Some(0)),
    );
    assert!(
        turn.record(|record| record.engram.active_turn_checks[0].end.is_none()),
        "yield or another command key cannot become this attempt's terminal"
    );
    turn.record(|record| {
        let trace = record.engram.unadmitted_active_turn.as_ref().unwrap();
        let owned = trace.commands.iter().find(|entry| entry.tool_key == "owned").unwrap();
        assert!(owned.check.finished_at.is_none(), "yield cannot terminalize the owned EO fact");
    });
    complete_typed(
        &turn,
        &mut recorder,
        &typed_item(&turn, "owned", "cargo build", "completed", Some(101)),
    );
    turn.wait_for_snapshots();
    // The foreign event cannot lend its pass or consume the owned attempt.
    // The later owned terminal is the command's failed native result.
    let request = finish_observation(&turn);
    let turn_ref = request["occurrence"]["host_turn_ref"].as_str().unwrap();
    let id = unadmitted_host_check_id(turn_ref, "owned");
    let owned = request["occurrence"]["observed_checks"].as_array().unwrap().iter()
        .filter(|check| check["host_check_id"] == id).collect::<Vec<_>>();
    assert_eq!(owned.len(), 1, "one exact owned occurrence: {request:#}");
    assert_eq!(owned[0]["observed_result"], "failed");
    assert_eq!(request["occurrence"]["source_change"]["sighting"]["source_basis"]["source_revision"], revision);
    assert!(summary(&turn).contains("exited 101"));
    assert!(
        diagnostic_refs(&turn).contains(&"exit:101".to_owned())
    );
}

#[test]
fn engram_build_outcome_focused_valid_terminal_is_not_inferred_from_test_counts() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for code in [0, 101] {
        let fixture = FocusedBuildFixture::new(&format!("build-outcome-focused-{code}"), code);
        let revision = fixture.turn.revision();
        one_build(
            &fixture.finish(EngramCommandExit::Code(code)),
            &revision,
            if code == 0 { "passed" } else { "failed" },
        );
    }
}

fn focused_rejection(reason: &str, mutate: fn(&mut FocusedBuildFixture)) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let mut fixture = FocusedBuildFixture::new(&format!("build-outcome-reject-{reason}"), 0);
    fixture.save();
    assert_eq!(
        engram_focused_build_exit(
            &fixture.check,
            &fixture.turn.session_id,
            &fixture.output,
            EngramCommandExit::Code(0)
        ),
        Some(0),
        "positive control {reason}"
    );
    mutate(&mut fixture);
    fixture.save();
    assert_eq!(
        engram_focused_build_exit(
            &fixture.check,
            &fixture.turn.session_id,
            &fixture.output,
            EngramCommandExit::Code(0)
        ),
        None,
        "rejection control {reason}"
    );
    let revision = fixture.turn.revision();
    rejected_terminal(
        &fixture.turn,
        &fixture.finish(EngramCommandExit::Code(0)),
        &revision,
        0,
        reason,
    );
}

#[test]
fn engram_build_outcome_owner_rejection_retains_native_exit() {
    focused_rejection("owner", |f| f.request["owner"] = json!("foreign-owner"));
}

#[test]
fn engram_build_outcome_root_rejection_retains_native_exit() {
    focused_rejection("root", |f| {
        f.request["root"] = json!(f.turn.root.join("other"))
    });
}

#[test]
fn engram_build_outcome_argv_rejection_retains_native_exit() {
    focused_rejection("argv", |f| f.request["stages"][0]["args"] = json!(["test"]));
}

#[test]
fn engram_build_outcome_terminal_rejection_retains_native_exit() {
    focused_rejection("terminal", |f| f.results["stages"][0]["code"] = Value::Null);
}

#[test]
fn engram_build_outcome_interval_rejection_retains_native_exit() {
    focused_rejection("interval", |f| set_interval(f, "2000-01-01T00:00:00.000Z"));
}

#[test]
fn engram_build_outcome_input_rejection_retains_native_exit() {
    focused_rejection("input", |f| f.input["fingerprint"] = json!("a".repeat(64)));
}

#[test]
fn engram_build_outcome_path_rejection_retains_native_exit() {
    focused_rejection("path", |f| {
        f.output = f.output.replace("results.json", "foreign.json")
    });
}

fn set_interval(fixture: &mut FocusedBuildFixture, time: &str) {
    fixture.request["started"] = json!(time);
    for key in ["started", "ended"] {
        fixture.results[key] = json!(time);
        fixture.results["stages"][0][key] = json!(time);
    }
}

struct StartTimingScope {
    previous: Option<(String, Option<String>)>,
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl StartTimingScope {
    fn delayed() -> Self {
        let previous = TEST_ENGRAM_COMMAND_START_TIMING.with(|slot| {
            slot.replace(Some((
                "2000-01-01T00:00:00.000Z".to_owned(),
                Some("2000-01-01T00:00:00.002Z".to_owned()),
            )))
        });
        Self {
            previous,
            _thread_bound: std::marker::PhantomData,
        }
    }
}

impl Drop for StartTimingScope {
    fn drop(&mut self) {
        TEST_ENGRAM_COMMAND_START_TIMING.with(|slot| {
            slot.replace(self.previous.take());
        });
    }
}

#[test]
fn engram_build_outcome_delayed_bookkeeping_keeps_the_observed_start_boundary() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let _clock = StartTimingScope::delayed();
    let mut fixture = FocusedBuildFixture::new("build-outcome-delayed-start", 0);
    set_interval(&mut fixture, "2000-01-01T00:00:00.001Z");
    fixture.save();
    let mut entry_boundary = fixture.check.clone();
    entry_boundary.started_at = "2000-01-01T00:00:00.000Z".to_owned();
    assert_eq!(
        engram_focused_build_exit(
            &entry_boundary,
            &fixture.turn.session_id,
            &fixture.output,
            EngramCommandExit::Code(0)
        ),
        Some(0),
        "genuine validator positive control differs only in host start timestamp"
    );
    assert_eq!(
        engram_focused_build_exit(
            &fixture.check,
            &fixture.turn.session_id,
            &fixture.output,
            EngramCommandExit::Code(0)
        ),
        Some(0),
        "target resolution must not move the correlated command's start after its request"
    );
    let revision = fixture.turn.revision();
    one_build(
        &fixture.finish(EngramCommandExit::Code(0)),
        &revision,
        "passed",
    );
}

#[test]
fn engram_build_outcome_pre_start_replay_still_cannot_supply_a_terminal() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let _clock = StartTimingScope::delayed();
    let mut fixture = FocusedBuildFixture::new("build-outcome-pre-start-replay", 0);
    set_interval(&mut fixture, "1999-12-31T23:59:59.999Z");
    fixture.save();
    let mut entry_boundary = fixture.check.clone();
    entry_boundary.started_at = "2000-01-01T00:00:00.000Z".to_owned();
    assert_eq!(
        engram_focused_build_exit(
            &entry_boundary,
            &fixture.turn.session_id,
            &fixture.output,
            EngramCommandExit::Code(0)
        ),
        None,
        "even the earlier correlated entry boundary must reject pre-start replay"
    );
    let revision = fixture.turn.revision();
    one_build(
        &fixture.finish(EngramCommandExit::Code(0)),
        &revision,
        "indeterminate",
    );
}

#[test]
fn engram_build_outcome_missing_ingress_is_not_a_present_rejected_terminal() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = FocusedBuildFixture::new("build-outcome-no-ingress-exit", 0);
    let revision = fixture.turn.revision();
    let request = fixture.finish(EngramCommandExit::Unknown);
    one_build(&request, &revision, "indeterminate");
    let text = summary(&fixture.turn);
    assert!(text.contains("no native terminal exit observed"), "{text}");
    assert!(
        !text.contains("observed native exit 0"),
        "artifact exit is not runtime ingress: {text}"
    );
}

#[test]
fn engram_build_outcome_mismatched_command_has_association_reason_and_no_pass() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let fixture = FocusedBuildFixture::new("build-outcome-association", 0);
    let revision = fixture.turn.revision();
    fixture.save();
    fixture
        .turn
        .recorder()
        .command_completed_with_exit(
            "build",
            "cargo build --release",
            &fixture.output,
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    fixture.turn.wait_for_snapshots();
    rejected_terminal(&fixture.turn, &finish_observation(&fixture.turn), &revision, 0, "association");
}

#[test]
fn engram_build_outcome_rejection_facts_survive_long_unicode_command_summary_bounds() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let turn = EOCheckedTurn::start("build-outcome-bounded", true);
    let revision = turn.revision();
    let command = format!(
        "cargo build --features {}",
        "é".repeat(ENGRAM_CHECK_SUMMARY_MAX_BYTES)
    );
    let mut recorder = turn.recorder();
    recorder.command_started("build", &command).unwrap();
    turn.wait_for_snapshots();
    recorder
        .command_completed_with_exit(
            "build",
            "cargo build --release",
            "",
            CommandStatus::Error,
            EngramCommandExit::Code(101),
        )
        .unwrap();
    turn.wait_for_snapshots();
    let request = finish_observation(&turn);
    rejected_terminal(&turn, &request, &revision, 101, "association");
    assert!(
        diagnostic_refs(&turn).is_empty(),
        "overlong command and rejected exit leave no eligible references"
    );
    assert!(summary(&turn).contains("command: cargo build --features é"));
    assert!(summary(&turn).len() > ENGRAM_CHECK_SUMMARY_MAX_BYTES - 4);
}

#[test]
fn engram_build_outcome_plain_missing_native_is_not_generic_success() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    for exit in [
        EngramCommandExit::Unknown,
        EngramCommandExit::ReportedSuccess,
    ] {
        let turn = EOCheckedTurn::start("build-outcome-plain-no-native", true);
        let revision = turn.revision();
        complete_command(&turn, "build", "cargo build", exit, "Finished successfully");
        let request = finish_observation(&turn);
        one_build(&request, &revision, "indeterminate");
        assert!(summary(&turn).contains("no native terminal exit observed"));
        assert!(
            !diagnostic_refs(&turn)
                .iter()
                .any(|reference| reference.starts_with("exit:"))
        );
    }
}
