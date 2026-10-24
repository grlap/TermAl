// Qualified command identity at the foreground-to-carried full-gate boundary.
// Synthetic reader provenance and full-run records drive the real recorder
// and settlement; these tests execute no launcher or shell command.
use super::*;

fn launch_bash_gate(turn: &CheckedTurn, worktree: &FsPath, started: &str, finished: &str) {
    let provenance = turn.record(|record| {
        EngramObservationProvenance::Claude(ClaudeObservationProvenance {
            token: record.runtime.runtime_token().expect("matching fixture runtime"),
            origin: ClaudeWorkOrigin::Attempt {
                turn_generation: record.active_turn_generation,
            },
        })
    });
    let cwd = fs::canonicalize(worktree).unwrap().to_string_lossy().into_owned();
    let mut recorder = turn.recorder();
    recorder.set_observation_provenance(provenance);
    recorder.command_started_in("gate", started, Some(started), Some(&cwd)).unwrap();
    turn.wait_for_snapshots();
    turn.record(|record| {
        assert_eq!(record.engram.active_turn_checks.len(), 1,
            "the matched reader must start the full-gate observation");
        assert!(record.engram.active_turn_checks[0].command.simple);
    });
    recorder.command_completed_with_exit("gate", finished, "STARTED",
        CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
    turn.wait_for_snapshots();
}

#[test]
fn qualified_detached_full_cannot_borrow_generic_terminal_stages() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = named_turn("qualified-full-carry");
    let command = format!("{DETACHED_GATE} --notify 'a|b'");
    let check = engram_check_command_in(&command, EngramShellDialect::Bash).unwrap();
    assert!(check.literal_pipe && check.simple, "the witness must reach qualification");
    launch_bash_gate(&turn, &worktree, &command, &command);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    let checkpoint = turn.finish();
    assert!(checkpoint.get("verification_evidence").is_none(),
        "qualified argv cannot borrow the generic full stages at {run:?}: {checkpoint:#}");
    assert_dropped_at_launch(&turn, "qualified pipe");
    assert_eq!(turn.record(|record| record.engram.carried_unmatched_launches.len()), 1,
        "a refused invocation may have launched a run; retain its ambiguity window");
}

#[test]
fn detached_full_completion_must_match_the_started_argv() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = named_turn("full-carry-association");
    let started = format!("{DETACHED_GATE} --notify session-original");
    let finished = format!("{DETACHED_GATE} --notify session-other");
    launch_bash_gate(&turn, &worktree, &started, &finished);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    let checkpoint = turn.finish();
    assert!(checkpoint.get("verification_evidence").is_none(),
        "a changed completion cannot transfer its old start to {run:?}: {checkpoint:#}");
    assert_dropped_at_launch(&turn, "does not match");
    assert_eq!(turn.record(|record| record.engram.carried_unmatched_launches.len()), 1,
        "the unmatched completion must not free its run for a later launch");
}

#[test]
fn qualified_full_is_never_carried_by_any_launch_exit() {
    let command = format!("{DETACHED_GATE} --notify 'a|b'");
    let check = engram_check_command_in(&command, EngramShellDialect::Bash).unwrap();
    assert!(check.simple && check.literal_pipe);
    for exit in [EngramCommandExit::Code(0), EngramCommandExit::ReportedSuccess,
        EngramCommandExit::NotFinished] {
        assert_eq!(engram_launch_disposition(&check, exit),
            EngramLaunchDisposition::Drop(ENGRAM_LAUNCH_DROP_QUALIFIED_PIPE));
    }
}

#[test]
fn matched_pipe_free_bash_full_keeps_its_carried_credit() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = named_turn("plain-bash-full-carry");
    let command = format!("{DETACHED_GATE} --notify session-original");
    launch_bash_gate(&turn, &worktree, &command, &command);
    assert_eq!(turn.record(|record| record.engram.carried_checks.len()), 1);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();
    assert_credited(&turn, &checkpoint, &run, "succeeded");
}

// Applies actual ACP terminal updates, keeping display titles distinct from
// rawInput.command. Synthetic full-run records exercise later settlement.
fn acp_full_completion(label: &str, title: Option<&str>, ran: Option<&str>, credited: bool) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = named_turn(label);
    let cwd = fs::canonicalize(&worktree).unwrap().to_string_lossy().into_owned();
    let started = format!("{DETACHED_GATE} --notify session-original");
    let (input_tx, _input_rx) = std::sync::mpsc::channel();
    let mut state = AcpTurnState::default();
    let mut recorder = turn.recorder();
    let begin = json!({"sessionUpdate":"tool_call_update", "toolCallId":"gate",
        "status":"in_progress", "rawInput":{"command":started, "cwd":cwd}});
    handle_acp_session_update(&begin, &turn.state, &turn.session_id, &input_tx,
        &mut state, &mut recorder, AcpAgent::Kimi).unwrap();
    turn.wait_for_snapshots();
    turn.record(|record| {
        assert_eq!(record.engram.active_turn_checks.len(), 1,
            "the actual ACP input must start the full check before completion");
        assert_eq!(record.engram.active_turn_checks[0].command.normalized, started);
    });
    let mut end = json!({"sessionUpdate":"tool_call_update", "toolCallId":"gate",
        "status":"completed", "rawOutput":"STARTED"});
    if let Some(title) = title {
        end["title"] = json!(title);
    }
    if let Some(ran) = ran {
        end["rawInput"] = json!({"command":ran, "cwd":cwd});
    }
    handle_acp_session_update(&end, &turn.state, &turn.session_id, &input_tx,
        &mut state, &mut recorder, AcpAgent::Kimi).unwrap();
    turn.wait_for_snapshots();
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();
    if credited {
        assert_credited(&turn, &checkpoint, &run, "succeeded");
    } else {
        assert!(checkpoint.get("verification_evidence").is_none(),
            "an explicitly substituted command cannot borrow the full run: {checkpoint:#}");
        assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
    }
}

#[test]
fn acp_titleless_full_keeps_its_carried_credit() {
    acp_full_completion("acp-titleless-full", None, None, true);
}

#[test]
fn acp_title_only_full_keeps_its_carried_credit() {
    // A title can itself look like a recognised runner without becoming a
    // report that this different command ran.
    acp_full_completion("acp-title-full", Some("cargo test"), None, true);
}

#[test]
fn acp_explicit_matching_full_keeps_its_carried_credit() {
    acp_full_completion("acp-matched-full", None,
        Some(&format!("{DETACHED_GATE} --notify session-original")), true);
}

#[test]
fn acp_explicit_changed_full_has_no_credit() {
    acp_full_completion("acp-changed-full", None,
        Some(&format!("{DETACHED_GATE} --notify session-other")), false);
    // Unrecognised actual text is still a report of a different command,
    // not evidence that the completion supplied no command.
    acp_full_completion("acp-unknown-command", None, Some("echo finished"), false);
}

#[test]
fn an_unrecognized_explicit_end_does_not_borrow_the_started_full() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = named_turn("unrecognized-full-end");
    launch_bash_gate(&turn, &worktree, DETACHED_GATE, "echo finished");
    finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();
    assert!(checkpoint.get("verification_evidence").is_none(),
        "parsed=None is not proof that the actual command was absent: {checkpoint:#}");
    assert_dropped_at_launch(&turn, "does not match");
}

#[test]
fn a_qualified_full_is_dropped_even_with_an_acp_titleless_completion() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = named_turn("qualified-titleless-full");
    let cwd = fs::canonicalize(&worktree).unwrap().to_string_lossy().into_owned();
    let command = format!("{DETACHED_GATE} --notify 'a|b'");
    let provenance = turn.record(|record| {
        EngramObservationProvenance::Claude(ClaudeObservationProvenance {
            token: record.runtime.runtime_token().unwrap(),
            origin: ClaudeWorkOrigin::Attempt { turn_generation: record.active_turn_generation },
        })
    });
    let mut recorder = turn.recorder();
    recorder.set_observation_provenance(provenance);
    recorder.command_started_in("gate", &command, Some(&command), Some(&cwd)).unwrap();
    turn.wait_for_snapshots();
    assert!(turn.record(|record| record.engram.active_turn_checks[0].command.literal_pipe));
    // This combines a retained known-Bash start with the actual ACP absent-
    // command completion shape as a boundary control. ACP itself never
    // supplies the known-Bash provenance needed to qualify that start.
    recorder.set_observation_provenance(EngramObservationProvenance::Ambient);
    let (input_tx, _input_rx) = std::sync::mpsc::channel();
    handle_acp_session_update(&json!({"sessionUpdate":"tool_call_update",
        "toolCallId":"gate", "status":"completed", "rawOutput":"STARTED"}),
        &turn.state, &turn.session_id, &input_tx, &mut AcpTurnState::default(),
        &mut recorder, AcpAgent::Kimi).unwrap();
    finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();
    assert!(checkpoint.get("verification_evidence").is_none(), "{checkpoint:#}");
    assert_dropped_at_launch(&turn, "qualified pipe");
}
