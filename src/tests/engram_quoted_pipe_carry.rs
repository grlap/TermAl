// Qualified command identity at the foreground-to-carried launcher boundary.
// Synthetic reader provenance and full-run records drive the real recorder
// and artifact reader; these tests execute no launcher or shell command.
// A carried artifact never becomes automatic checkpoint/native verification.
use super::*;

fn quoted_turn(label: &str) -> (EOCheckedTurn, PathBuf) {
    let turn = EOCheckedTurn::start_named(label);
    let worktree = turn.root.join(".worktrees").join("wt");
    (turn, worktree)
}

fn assert_launch_fact(turn: &EOCheckedTurn, observation: &Value) {
    let checks = observation["occurrence"]["observed_checks"]
        .as_array()
        .unwrap();
    assert_eq!(checks.len(), 1, "{observation:#}");
    assert_eq!(checks[0]["check_kind"], "test");
    assert_eq!(
        checks[0]["host_check_id"],
        unadmitted_host_check_id(
            observation["occurrence"]["host_turn_ref"].as_str().unwrap(),
            "gate"
        )
    );
    assert_eq!(checks[0]["observed_result"], "indeterminate");
    assert!(
        checks[0].get("finished_at").is_none(),
        "a launch ACK is not a test end"
    );
    assert!(checks[0].get("source_basis").is_none());
    assert_ordinary_observation_has_no_native_credit(turn, observation);
}

fn assert_quoted_launch_dropped(turn: &EOCheckedTurn, why: &str) {
    turn.record(|record| {
        assert!(record.engram.carried_checks.is_empty());
        assert!(record.engram.active_turn_checks.is_empty());
        let line = record.engram.pending_source_root_line.as_deref().unwrap();
        assert!(
            line.contains(ENGRAM_CHECK_CREDIT_LINE_PREFIX) && line.contains(why),
            "{line}"
        );
    });
}

fn assert_matched_artifact_is_not_verification(turn: &EOCheckedTurn, run: &FsPath) {
    let digest = engram_terminal_record_digest(run).expect("a real terminal fixture record");
    let verdict = engram_read_carried_run(run, &digest).expect("the exact artifact validates");
    assert!(verdict.passed);
    assert_eq!(verdict.exit_code, 0);
    assert_eq!(
        serde_json::to_value(verdict.source_id).unwrap(),
        launcher_source_id_fixture()
    );
    turn.state.poll_engram_carried_runs();
    assert_retired_run(turn, run);
    assert_launch_fact(turn, &turn.finish());
    assert_retired_run(turn, run);
}

fn launch_bash_gate(turn: &EOCheckedTurn, worktree: &FsPath, started: &str, finished: &str) {
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
    let cwd = fs::canonicalize(worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut recorder = turn.recorder();
    recorder.set_observation_provenance(provenance);
    recorder
        .command_started_in("gate", started, Some(started), Some(&cwd))
        .unwrap();
    turn.wait_for_snapshots();
    turn.record(|record| {
        assert_eq!(
            record.engram.active_turn_checks.len(),
            1,
            "the matched reader must start the full-gate observation"
        );
        assert!(record.engram.active_turn_checks[0].command.simple);
    });
    recorder
        .command_completed_with_exit(
            "gate",
            finished,
            "STARTED",
            CommandStatus::Success,
            EngramCommandExit::Code(0),
        )
        .unwrap();
    turn.wait_for_snapshots();
}

#[test]
fn qualified_detached_full_cannot_borrow_generic_terminal_stages() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = quoted_turn("qualified-full-carry");
    let command = format!("{DETACHED_GATE} --notify 'a|b'");
    let check = engram_check_command_in(&command, EngramShellDialect::Bash).unwrap();
    assert!(
        check.literal_pipe && check.simple,
        "the witness must reach qualification"
    );
    launch_bash_gate(&turn, &worktree, &command, &command);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    assert_launch_fact(&turn, &turn.finish());
    assert!(engram_terminal_record_digest(&run).is_some());
    assert_quoted_launch_dropped(&turn, "qualified pipe");
    assert_eq!(
        turn.record(|record| record.engram.carried_unmatched_launches.len()),
        1,
        "a refused invocation may have launched a run; retain its ambiguity window"
    );
}

#[test]
fn detached_full_completion_must_match_the_started_argv() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = quoted_turn("full-carry-association");
    let started = format!("{DETACHED_GATE} --notify session-original");
    let finished = format!("{DETACHED_GATE} --notify session-other");
    launch_bash_gate(&turn, &worktree, &started, &finished);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    assert_launch_fact(&turn, &turn.finish());
    assert!(engram_terminal_record_digest(&run).is_some());
    assert_quoted_launch_dropped(&turn, "does not match");
    assert_eq!(
        turn.record(|record| record.engram.carried_unmatched_launches.len()),
        1,
        "the unmatched completion must not free its run for a later launch"
    );
}

#[test]
fn qualified_full_is_never_carried_by_any_launch_exit() {
    let command = format!("{DETACHED_GATE} --notify 'a|b'");
    let check = engram_check_command_in(&command, EngramShellDialect::Bash).unwrap();
    assert!(check.simple && check.literal_pipe);
    for exit in [
        EngramCommandExit::Code(0),
        EngramCommandExit::ReportedSuccess,
        EngramCommandExit::NotFinished,
    ] {
        assert_eq!(
            engram_launch_disposition(&check, exit),
            EngramLaunchDisposition::Drop(ENGRAM_LAUNCH_DROP_QUALIFIED_PIPE)
        );
    }
}

#[test]
fn matched_pipe_free_bash_full_keeps_artifact_custody_without_verification() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = quoted_turn("plain-bash-full-carry");
    let command = format!("{DETACHED_GATE} --notify session-original");
    launch_bash_gate(&turn, &worktree, &command, &command);
    assert_eq!(turn.record(|record| record.engram.carried_checks.len()), 1);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    assert_matched_artifact_is_not_verification(&turn, &run);
}

// Applies actual ACP terminal updates, keeping display titles distinct from
// rawInput.command. Synthetic full-run records exercise artifact custody.
fn acp_full_completion(label: &str, title: Option<&str>, ran: Option<&str>, matched: bool) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = quoted_turn(label);
    let cwd = fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let started = format!("{DETACHED_GATE} --notify session-original");
    let (input_tx, _input_rx) = std::sync::mpsc::channel();
    let mut state = AcpTurnState::default();
    let mut recorder = turn.recorder();
    let begin = json!({"sessionUpdate":"tool_call_update", "toolCallId":"gate",
        "status":"in_progress", "rawInput":{"command":started, "cwd":cwd}});
    handle_acp_session_update(
        &begin,
        &turn.state,
        &turn.session_id,
        &input_tx,
        &mut state,
        &mut recorder,
        AcpAgent::Kimi,
    )
    .unwrap();
    turn.wait_for_snapshots();
    turn.record(|record| {
        assert_eq!(
            record.engram.active_turn_checks.len(),
            1,
            "the actual ACP input must start the full check before completion"
        );
        assert_eq!(
            record.engram.active_turn_checks[0].command.normalized,
            started
        );
    });
    let mut end = json!({"sessionUpdate":"tool_call_update", "toolCallId":"gate",
        "status":"completed", "rawOutput":"STARTED"});
    if let Some(title) = title {
        end["title"] = json!(title);
    }
    if let Some(ran) = ran {
        end["rawInput"] = json!({"command":ran, "cwd":cwd});
    }
    handle_acp_session_update(
        &end,
        &turn.state,
        &turn.session_id,
        &input_tx,
        &mut state,
        &mut recorder,
        AcpAgent::Kimi,
    )
    .unwrap();
    turn.wait_for_snapshots();
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    if matched {
        assert_matched_artifact_is_not_verification(&turn, &run);
    } else {
        assert_launch_fact(&turn, &turn.finish());
        assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
        assert!(engram_terminal_record_digest(&run).is_some());
    }
}

#[test]
fn acp_titleless_full_keeps_its_artifact_custody() {
    acp_full_completion("acp-titleless-full", None, None, true);
}

#[test]
fn acp_title_only_full_keeps_its_artifact_custody() {
    // A title can itself look like a recognised runner without becoming a
    // report that this different command ran.
    acp_full_completion("acp-title-full", Some("cargo test"), None, true);
}

#[test]
fn acp_explicit_matching_full_keeps_its_artifact_custody() {
    acp_full_completion(
        "acp-matched-full",
        None,
        Some(&format!("{DETACHED_GATE} --notify session-original")),
        true,
    );
}

#[test]
fn acp_explicit_changed_full_has_no_credit() {
    acp_full_completion(
        "acp-changed-full",
        None,
        Some(&format!("{DETACHED_GATE} --notify session-other")),
        false,
    );
    // Unrecognised actual text is still a report of a different command,
    // not evidence that the completion supplied no command.
    acp_full_completion("acp-unknown-command", None, Some("echo finished"), false);
}

#[test]
fn an_unrecognized_explicit_end_does_not_borrow_the_started_full() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = quoted_turn("unrecognized-full-end");
    launch_bash_gate(&turn, &worktree, DETACHED_GATE, "echo finished");
    finish_run(&worktree, "passed", &"f".repeat(64));
    assert_launch_fact(&turn, &turn.finish());
    assert_quoted_launch_dropped(&turn, "does not match");
}

#[test]
fn a_qualified_full_is_dropped_even_with_an_acp_titleless_completion() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = quoted_turn("qualified-titleless-full");
    let cwd = fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let command = format!("{DETACHED_GATE} --notify 'a|b'");
    let provenance = turn.record(|record| {
        EngramObservationProvenance::Claude(ClaudeObservationProvenance {
            token: record.runtime.runtime_token().unwrap(),
            origin: ClaudeWorkOrigin::Attempt {
                turn_generation: record.active_turn_generation,
            },
        })
    });
    let mut recorder = turn.recorder();
    recorder.set_observation_provenance(provenance);
    recorder
        .command_started_in("gate", &command, Some(&command), Some(&cwd))
        .unwrap();
    turn.wait_for_snapshots();
    assert!(turn.record(|record| record.engram.active_turn_checks[0].command.literal_pipe));
    // This combines a retained known-Bash start with the actual ACP absent-
    // command completion shape as a boundary control. ACP itself never
    // supplies the known-Bash provenance needed to qualify that start.
    recorder.set_observation_provenance(EngramObservationProvenance::Ambient);
    let (input_tx, _input_rx) = std::sync::mpsc::channel();
    handle_acp_session_update(
        &json!({"sessionUpdate":"tool_call_update",
        "toolCallId":"gate", "status":"completed", "rawOutput":"STARTED"}),
        &turn.state,
        &turn.session_id,
        &input_tx,
        &mut AcpTurnState::default(),
        &mut recorder,
        AcpAgent::Kimi,
    )
    .unwrap();
    finish_run(&worktree, "passed", &"f".repeat(64));
    assert_launch_fact(&turn, &turn.finish());
    assert_quoted_launch_dropped(&turn, "qualified pipe");
}
