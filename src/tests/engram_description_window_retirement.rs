// A changed description or repeated start can retire a launch check before
// its completion. Actual ACP updates and synthetic run records exercise
// later correlation and settlement without executing a launcher or shell.
use super::*;

struct StartClock(Option<(String, Option<String>)>);

impl StartClock {
    fn at(time: &str) -> Self {
        Self(TEST_ENGRAM_COMMAND_START_TIMING.with(|slot| {
            slot.replace(Some((time.to_owned(), None)))
        }))
    }
}

impl Drop for StartClock {
    fn drop(&mut self) {
        TEST_ENGRAM_COMMAND_START_TIMING.with(|slot| slot.replace(self.0.take()));
    }
}

fn take_retirement_notices(turn: &CheckedTurn) -> Vec<String> {
    let pending = turn.record(|record| {
        record.engram.pending_source_root_line.clone().unwrap_or_default()
    });
    turn.record_mut(|record| record.engram.pending_source_root_line = None);
    pending.lines().filter(|line| line.contains("command or place changed"))
        .map(str::to_owned).collect()
}

fn assert_retirement_notice(turn: &CheckedTurn, fingerprint: &str, original: &str, later: &str) {
    let notices = take_retirement_notices(turn);
    assert_eq!(notices.len(), 1, "the retired launch needs one notice: {notices:?}");
    let line = &notices[0];
    assert!(line.starts_with(ENGRAM_CHECK_CREDIT_LINE_PREFIX), "{line}");
    assert!(line.contains(fingerprint) && line.contains(original), "{line}");
    assert!(!line.contains(later), "the replacement's timestamp is not the launch's: {line}");
    assert!(line.contains("may already have started a run"), "{line}");
    assert!(line.contains("two minutes or more after the last launch"), "{line}");
}

fn acp_update(
    turn: &CheckedTurn,
    recorder: &mut SessionRecorder,
    state: &mut AcpTurnState,
    key: &str,
    status: &str,
    command: &str,
    cwd: &str,
) {
    let (input_tx, _input_rx) = std::sync::mpsc::channel();
    test_acp_session_update(&json!({
        "sessionUpdate":"tool_call_update", "toolCallId":key, "status":status,
        "rawInput":{"command":command, "cwd":cwd}, "rawOutput":"STARTED",
    }), &turn.state, &turn.session_id, &input_tx, state, recorder, AcpAgent::Kimi).unwrap();
}

fn acp_create(turn: &CheckedTurn, recorder: &mut SessionRecorder, state: &mut AcpTurnState, key: &str) {
    let (input_tx, _input_rx) = std::sync::mpsc::channel();
    test_acp_session_update(&json!({"sessionUpdate":"tool_call", "toolCallId":key,
        "status":"pending", "title":"Bash"}), &turn.state, &turn.session_id,
        &input_tx, state, recorder, AcpAgent::Kimi).unwrap();
}

fn running_run(runs: &FsPath, root: &FsPath, started: &str) -> PathBuf {
    let run_id = "test-retired-launch";
    let directory = runs.join(run_id);
    fs::create_dir_all(&directory).unwrap();
    let stages = ["cargo-check", "typescript", "fingerprint-tests", "rust-tests", "vitest"];
    fs::write(directory.join("request.json"), serde_json::to_vec(&json!({
        "runId":run_id, "root":root.to_string_lossy(), "started":started,
        "full":true, "detached":false, "expectedFingerprint":"f".repeat(64),
        "stages":stages.iter().map(|name| json!({"name":name})).collect::<Vec<_>>(),
    })).unwrap()).unwrap();
    // The launcher's initial result has no terminal timestamp or exit code.
    fs::write(directory.join("results.json"), serde_json::to_vec(&json!({
        "runId":run_id, "state":"running", "started":started, "preflight":[],
        "stages":stages.iter().map(|name| json!({"name":name,"state":"unrun"}))
            .collect::<Vec<_>>(),
    })).unwrap()).unwrap();
    assert!(engram_terminal_record_digest(&directory).is_none());
    directory
}

fn retired_launch_cannot_lend_run(repeated_start: bool, changed_place: bool) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = named_turn(&format!("retired-description-{repeated_start}-{changed_place}"));
    // Create the alternate location before any check's source interval opens.
    let elsewhere = sibling_worktree(&turn, "elsewhere");
    let cwd = fs::canonicalize(&worktree).unwrap();
    let other_cwd = fs::canonicalize(&elsewhere).unwrap();
    let command = format!("{DETACHED_GATE} --notify session-original");
    let mut recorder = turn.recorder();
    let mut acp = AcpTurnState::default();
    acp_create(&turn, &mut recorder, &mut acp, "old");
    let first = chrono::Utc::now();
    let original_at = first.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let replacement_at = (first + chrono::Duration::seconds(1))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    {
        let _clock = StartClock::at(&original_at);
        acp_update(&turn, &mut recorder, &mut acp, "old", "in_progress", &command,
            &cwd.to_string_lossy());
    }
    turn.wait_for_snapshots();
    let original = turn.record(|record| {
        assert_eq!(record.engram.active_turn_checks.len(), 1);
        record.engram.active_turn_checks[0].clone()
    });
    assert_eq!(original.started_at, original_at);
    let fingerprint = engram_check_fingerprint(&original.command);
    assert!(take_retirement_notices(&turn).is_empty());
    assert_eq!(engram_path_key(&original.target.root), engram_path_key(&cwd));
    let replacement = if changed_place { command.clone() }
        else { format!("{DETACHED_GATE} --notify session-changed") };
    let replacement_cwd = if changed_place { &other_cwd } else { &cwd };
    if repeated_start {
        let _clock = StartClock::at(&replacement_at);
        acp_update(&turn, &mut recorder, &mut acp, "old", "in_progress", &replacement,
            &replacement_cwd.to_string_lossy());
        turn.record(|record| assert!(record.engram.active_turn_checks.is_empty(),
            "the repeated start must retire the old check before completion"));
        assert_retirement_notice(&turn, &fingerprint, &original_at, &replacement_at);
    }
    // Production ACP describes the actual command/place before completing it.
    acp_update(&turn, &mut recorder, &mut acp, "old", "completed", &replacement,
        &replacement_cwd.to_string_lossy());
    turn.wait_for_snapshots();
    if repeated_start {
        assert!(take_retirement_notices(&turn).is_empty(),
            "the terminal update must not repeat the earlier retirement notice");
    } else {
        assert_retirement_notice(&turn, &fingerprint, &original_at, &replacement_at);
    }
    let expected = (engram_path_key(&original.target.root), original.started_at.clone());
    let retained = turn.record(|record| {
        assert!(record.engram.active_turn_checks.is_empty());
        assert!(record.engram.carried_checks.is_empty());
        assert!(!record.engram.running_command_keys.contains_key(&original.key));
        record.engram.carried_unmatched_launches.contains(&expected)
    });

    recorder.command_started_in("gate", DETACHED_GATE, Some(DETACHED_GATE),
        Some(&cwd.to_string_lossy())).unwrap();
    turn.wait_for_snapshots();
    // Pin the later launch to the original stamp. The retired run starts
    // exactly one second inside the search slack, independent of scheduling.
    turn.record_mut(|record| {
        let later = record.engram.active_turn_checks.iter_mut()
            .find(|check| check.key == "gate").expect("later launch starts its own check");
        later.started_at = original.started_at.clone();
    });
    let earlier_at = (engram_parse_time(&original.started_at).unwrap()
        - chrono::Duration::seconds(1)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let runs = review_runs(&worktree);
    let late_publication = changed_place;
    if !late_publication { running_run(&runs, &cwd, &earlier_at); }
    recorder.command_completed_with_exit("gate", DETACHED_GATE, "STARTED",
        CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
    turn.wait_for_snapshots();
    if late_publication { running_run(&runs, &cwd, &earlier_at); }
    turn.state.poll_engram_carried_runs();
    turn.record(|record| {
        assert_eq!(record.engram.carried_checks.len(), 1);
        let later = &record.engram.carried_checks[0];
        assert!(later.fence.is_none(), "an unrelated fence must not satisfy this witness");
        assert!(later.terminal_digest.is_none() && !later.terminal_conflict);
        if !later.ambiguous {
            assert_eq!(later.run_directory.as_ref(), Some(&runs.join("test-retired-launch")),
                "without a window the fixture must reach borrowing of the old run");
        }
    });
    write_run(&runs, "test-retired-launch", &cwd, &earlier_at, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();
    assert!(checkpoint.get("verification_evidence").is_none(),
        "a later launch borrowed the retired check's run: {checkpoint:#}");
    assert_refused(&turn, &checkpoint, "another background gate launched");
    assert!(retained, "retirement must keep the original root/start, not the replacement's");
}

#[test]
fn acp_terminal_changed_command_cannot_lend_its_running_run() {
    retired_launch_cannot_lend_run(false, false);
}

#[test]
fn acp_terminal_changed_place_cannot_lend_a_late_published_run() {
    retired_launch_cannot_lend_run(false, true);
}

#[test]
fn repeated_start_changed_command_cannot_lend_its_running_run() {
    retired_launch_cannot_lend_run(true, false);
}

#[test]
fn repeated_start_changed_place_cannot_lend_a_late_published_run() {
    retired_launch_cannot_lend_run(true, true);
}

fn unchanged_launch_still_credits(repeated_start: bool) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = named_turn(&format!("matched-description-{repeated_start}"));
    let cwd = fs::canonicalize(&worktree).unwrap().to_string_lossy().into_owned();
    let mut recorder = turn.recorder();
    let mut acp = AcpTurnState::default();
    acp_create(&turn, &mut recorder, &mut acp, "gate");
    acp_update(&turn, &mut recorder, &mut acp, "gate", "in_progress", DETACHED_GATE, &cwd);
    turn.wait_for_snapshots();
    let identity = turn.record(|record| {
        let check = &record.engram.active_turn_checks[0];
        (check.sequence, check.started_at.clone())
    });
    if repeated_start {
        acp_update(&turn, &mut recorder, &mut acp, "gate", "in_progress", DETACHED_GATE, &cwd);
    }
    acp_update(&turn, &mut recorder, &mut acp, "gate", "completed", DETACHED_GATE, &cwd);
    turn.wait_for_snapshots();
    assert!(take_retirement_notices(&turn).is_empty());
    turn.record(|record| {
        assert!(record.engram.carried_unmatched_launches.is_empty());
        assert_eq!(record.engram.carried_checks.len(), 1);
        let carried = &record.engram.carried_checks[0];
        assert_eq!((carried.check.sequence, carried.check.started_at.clone()), identity);
        assert!(!carried.ambiguous);
    });
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();
    assert_credited(&turn, &checkpoint, &run, "succeeded");
}

#[test]
fn acp_matching_terminal_description_keeps_its_own_run_credit() {
    unchanged_launch_still_credits(false);
}

#[test]
fn matching_repeated_start_keeps_the_original_start_and_credit() {
    unchanged_launch_still_credits(true);
}

#[test]
fn retiring_an_ordinary_test_does_not_create_a_full_launch_window() {
    ordinary_retirement_stays_silent(false);
}

#[test]
fn repeating_an_ordinary_test_with_a_changed_command_creates_no_full_launch_notice() {
    ordinary_retirement_stays_silent(true);
}

fn ordinary_retirement_stays_silent(repeated_start: bool) {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (turn, worktree) = named_turn(&format!("retired-ordinary-test-{repeated_start}"));
    let cwd = fs::canonicalize(&worktree).unwrap().to_string_lossy().into_owned();
    let mut recorder = turn.recorder();
    recorder.command_started_in("ordinary", "cargo test", Some("cargo test"), Some(&cwd)).unwrap();
    turn.wait_for_snapshots();
    assert_eq!(turn.record(|record| record.engram.active_turn_checks.len()), 1);
    assert!(take_retirement_notices(&turn).is_empty());
    if repeated_start {
        recorder.command_started_in("ordinary", "echo finished", Some("echo finished"),
            Some(&cwd)).unwrap();
    } else {
        recorder.command_described("ordinary", Some("echo finished"), Some(&cwd)).unwrap();
    }
    recorder.command_completed_with_exit("ordinary", "echo finished", "",
        CommandStatus::Success, EngramCommandExit::Code(0)).unwrap();
    turn.record(|record| {
        assert!(record.engram.active_turn_checks.is_empty());
        assert!(record.engram.carried_checks.is_empty());
        assert!(record.engram.carried_unmatched_launches.is_empty());
    });
    assert!(take_retirement_notices(&turn).is_empty());
    assert!(turn.finish().get("verification_evidence").is_none());
}
