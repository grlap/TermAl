// Late ends of checks closed with outcome Unknown: an end reported, with no
// check running under its key, under the runtime key of a check a turn
// boundary closed because no end was reported for it.
//
// Owns: the end-to-end tests of that closure's lifetime and of the late-end
// line, one per way a later end can arrive under the key: a command reusing
// it that starts no check, an end with no start, a rerun that capture
// capacity refused, an ACP frame that names its command by a title only, a
// declared check, and a rerun that starts a check of its own. Each asserts
// the line's wording and that it is told once.
// Does not own: closing the check at the turn's close or at the next turn's
// start, whose tests stay in engram_turn_checks.rs
// (`silent_check_outcome_a_started_check_with_no_end_is_closed_unknown`,
// `silent_check_outcome_a_check_a_cached_close_left_is_closed_at_the_next_turn_start`).
// Split out of src/tests/engram_turn_checks.rs, which is at its size
// threshold: `close_a_check_with_no_end` and the two reused-key tests moved
// here, rewritten for the line that states what was observed.
use super::*;

/// What every late-end line says, whatever end it tells.
const TOLD: &str = "was closed with outcome Unknown; it is not recorded";

/// Starts a recognised check under `key` whose end is never reported, and
/// closes the turn, which closes it with outcome Unknown. Clears the holder
/// line afterwards so what follows reads alone.
fn close_a_check_with_no_end(turn: &EOCheckedTurn, recorder: &mut SessionRecorder, key: &str) -> Value {
    close_a_command_with_no_end(turn, recorder, key, SIZE_TEST)
}

/// `close_a_check_with_no_end` for the check `command` starts.
fn close_a_command_with_no_end(
    turn: &EOCheckedTurn,
    recorder: &mut SessionRecorder,
    key: &str,
    command: &str,
) -> Value {
    recorder
        .command_started(key, command)
        .expect("the start records");
    turn.wait_for_snapshots();
    let request = turn.finish();
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1, "{request:#}");
    assert_eq!(checks[0]["observed_result"], "indeterminate");
    assert!(checks[0].get("finished_at").is_none());
    assert_ordinary_observation_has_no_native_credit(turn, &request);
    let closed = turn.record(|record| record.engram.pending_source_root_line.clone());
    assert!(
        closed.is_some_and(|line| line.contains("no end was reported")),
        "the check was closed with outcome Unknown"
    );
    turn.record_mut(|record| record.engram.pending_source_root_line = None);
    request
}

/// The holder line pending now, cleared, so the next read sees only what
/// follows.
fn take_line(turn: &EOCheckedTurn) -> String {
    let line = turn.record(|record| record.engram.pending_source_root_line.clone());
    turn.record_mut(|record| record.engram.pending_source_root_line = None);
    line.unwrap_or_default()
}

/// Reports the end of `command` under `key` with exit code `code`.
fn end(recorder: &mut SessionRecorder, key: &str, command: &str, code: i64) {
    recorder
        .command_completed_with_exit(
            key,
            command,
            "running 3 tests\ntest result: ok. 3 passed",
            CommandStatus::Success,
            EngramCommandExit::Code(code),
        )
        .expect("the end records");
}

/// How the size test is named in a holder line.
fn size_test_label() -> String {
    let command = engram_check_command(SIZE_TEST).expect("the fixture command is recognised");
    format!(
        "a {} test (check {})",
        command.program,
        engram_check_fingerprint(&command)
    )
}

/// How a command that is not a recognised check is named in a holder line.
fn text_label(text: &str) -> String {
    format!(
        "a command (text fingerprint {})",
        &sha256_hex(text.as_bytes())[..12]
    )
}

/// The late-end line for an end of `ended` with exit code `code` after the
/// size test was closed.
fn told_line(ended: &str, code: i64) -> String {
    format!(
        "an end (exit code {code}) was reported for {ended} under the same runtime id after {} \
         {TOLD}",
        size_test_label()
    )
}

/// A runtime may reuse a command key (Codex keys a command by its line when
/// an item has no id). A command started under the key of a check closed
/// with outcome Unknown that starts no check keeps the closure, and its end
/// is told once, as an end reported under that key, named by its text's
/// fingerprint and never by its text. An end reported again is not told.
#[test]
fn a_plain_command_reusing_the_key_is_told_once_as_an_observation() {
    let turn = EOCheckedTurn::start("late-end-reused-plain", true);
    let mut recorder = turn.recorder();
    close_a_check_with_no_end(&turn, &mut recorder, "reused");
    recorder
        .command_started("reused", "git status")
        .expect("the reusing start records");
    assert!(!take_line(&turn).contains(TOLD), "a start tells nothing");
    end(&mut recorder, "reused", "git status", 1);
    let line = take_line(&turn);
    assert!(
        line.contains(&told_line(&text_label("git status"), 1)),
        "{line}"
    );
    assert_eq!(line.matches(TOLD).count(), 1, "{line}");
    assert!(!line.contains("git status"), "no command text: {line}");
    end(&mut recorder, "reused", "git status", 1);
    assert!(!take_line(&turn).contains(TOLD), "told once");
}

/// An end reported under the key with no start of its own is told once. The
/// closure is then retired: a later start and end of the closed check's own
/// command under the key is not told again.
#[test]
fn an_end_reported_without_its_start_is_told_once() {
    let turn = EOCheckedTurn::start("late-end-no-start", true);
    let mut recorder = turn.recorder();
    close_a_check_with_no_end(&turn, &mut recorder, "lost-end");
    end(&mut recorder, "lost-end", "git status", 0);
    let line = take_line(&turn);
    assert!(
        line.contains(&told_line(&text_label("git status"), 0)),
        "{line}"
    );
    recorder
        .command_started("lost-end", SIZE_TEST)
        .expect("the later start records");
    end(&mut recorder, "lost-end", SIZE_TEST, 0);
    assert!(
        !take_line(&turn).contains(TOLD),
        "the closure was retired by the told end"
    );
}

/// A rerun of the closed check's command under its key that capture capacity
/// refuses starts no check, so it keeps the closure, and its end is told
/// once, named as the recognised check it is.
#[test]
fn a_rerun_that_capacity_refuses_is_told_once() {
    let turn = EOCheckedTurn::start("late-end-capacity-rerun", true);
    let mut recorder = turn.recorder();
    close_a_check_with_no_end(&turn, &mut recorder, "rerun");
    turn.begin("Run the successor ordinary check.");
    let workers = turn.record(|record| record.engram.capture_workers.clone());
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let pending = (0..ENGRAM_CAPTURE_WORKER_LIMIT)
        .map(|_| {
            let gate = gate.clone();
            EngramCapture::spawn(&workers, &Arc::new(CaptureLifetime::default()), move || {
                let (open, opened) = &*gate;
                let _ = opened
                    .wait_timeout_while(
                        open.lock().expect("gate mutex poisoned"),
                        DEADLOCK_GUARD,
                        |open| !*open,
                    )
                    .expect("gate mutex poisoned");
            })
        })
        .collect::<Vec<_>>();
    turn.run("rerun", SIZE_TEST, EngramCommandExit::Code(0), || {});
    let refused = turn.record(|record| {
        record
            .engram
            .active_turn_checks
            .iter()
            .all(|check| check.key != "rerun")
    });
    // Release every worker before asserting; no blocked fixture thread is
    // abandoned.
    let (open, opened) = &*gate;
    *open.lock().expect("gate mutex poisoned") = true;
    opened.notify_all();
    for capture in pending {
        capture
            .wait_until(std::time::Instant::now() + DEADLOCK_GUARD)
            .expect("the released worker finishes");
    }
    assert!(refused, "the capture capacity refuses the rerun's check");
    let line = take_line(&turn);
    assert!(line.contains(&told_line(&size_test_label(), 0)), "{line}");
    assert_eq!(line.matches(TOLD).count(), 1, "{line}");
    let request = turn.finish();
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1, "diagnostic capacity cannot lose ordinary facts: {request:#}");
    assert_eq!(checks[0]["observed_result"], "passed");
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

/// An ACP runtime may report a frame with no command line (its start names
/// the call by a title only) and end the call by that title. The sparse
/// start keeps the closure, and the title-only end is told once, named by
/// its text's fingerprint.
#[test]
fn an_acp_title_only_end_is_told_once() {
    const TITLE: &str = "Run the size test";
    let turn = EOCheckedTurn::start("late-end-acp-title", true);
    let mut recorder = turn.recorder();
    close_a_check_with_no_end(&turn, &mut recorder, "acp-call");
    recorder
        .command_started_in("acp-call", TITLE, None, None)
        .expect("the sparse start records");
    assert!(
        !take_line(&turn).contains(TOLD),
        "a sparse start tells nothing"
    );
    end(&mut recorder, "acp-call", TITLE, 0);
    let line = take_line(&turn);
    assert!(line.contains(&told_line(&text_label(TITLE), 0)), "{line}");
    assert_eq!(line.matches(TOLD).count(), 1, "{line}");
    end(&mut recorder, "acp-call", TITLE, 0);
    assert!(!take_line(&turn).contains(TOLD), "told once");
}

/// A declared check (termal-tests.toml) closed with outcome Unknown has its
/// later end told once like any other, although its command line is not a
/// built-in check.
#[test]
fn a_declared_checks_late_end_is_told_once() {
    let turn = EOCheckedTurn::start("late-end-declared", true);
    super::declared_checks::declare(
        &turn,
        super::declared_checks::DECLARED_TEST,
        super::declared_checks::DECLARED_ARTIFACT,
    );
    let mut recorder = turn.recorder();
    close_a_command_with_no_end(
        &turn,
        &mut recorder,
        "declared",
        super::declared_checks::DECLARED_TEST,
    );
    end(
        &mut recorder,
        "declared",
        super::declared_checks::DECLARED_TEST,
        0,
    );
    let line = take_line(&turn);
    assert!(
        line.contains("an end (exit code 0) was reported for a ")
            && line.contains(" under the same runtime id after a "),
        "{line}"
    );
    assert_eq!(line.matches(TOLD).count(), 1, "{line}");
    assert!(!line.contains("--logger"), "no command arguments: {line}");
    end(
        &mut recorder,
        "declared",
        super::declared_checks::DECLARED_TEST,
        0,
    );
    assert!(!take_line(&turn).contains(TOLD), "told once");
}

/// A check started under the key of a check closed with outcome Unknown is a
/// new run, even of the same command: it retires the closure, and neither its
/// end nor that end reported again is told as a late end. It is recorded as
/// itself.
#[test]
fn a_rerun_that_starts_a_check_retires_the_closure() {
    let turn = EOCheckedTurn::start("late-end-rerun-check", true);
    let mut recorder = turn.recorder();
    let first = close_a_check_with_no_end(&turn, &mut recorder, "reused");
    turn.begin("Run the successor ordinary check.");
    turn.run("reused", SIZE_TEST, EngramCommandExit::Code(0), || {});
    assert!(
        !take_line(&turn).contains(TOLD),
        "the rerun check's own end is no late end"
    );
    end(&mut recorder, "reused", SIZE_TEST, 0);
    assert!(
        !take_line(&turn).contains(TOLD),
        "nor is its end reported again"
    );
    let request = turn.finish();
    assert_ne!(request["occurrence"]["host_turn_ref"], first["occurrence"]["host_turn_ref"]);
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1, "only the successor's own check: {request:#}");
    assert_eq!(checks[0]["observed_result"], "passed");
    assert_ne!(checks[0]["host_check_id"], first["occurrence"]["observed_checks"][0]["host_check_id"]);
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}

/// The test-run rows the session of `turn` owns: each one's execution id and
/// whether it has ended.
fn session_test_runs(turn: &EOCheckedTurn) -> Vec<(String, bool)> {
    let inner = turn.state.inner.lock().unwrap();
    inner
        .test_run_observations
        .values()
        .filter(|row| row.summary.owner_session_id.as_deref() == Some(turn.session_id.as_str()))
        .map(|row| (row.execution_id.clone(), row.summary.ended_at.is_some()))
        .collect()
}

/// A start repeated in its own turn, as ACP repeats one (pending, then in
/// progress), is the same execution: it matches that start's test-run row,
/// adds no row and starts no second check, and the one end ends that row and
/// that check. Only an earlier turn's unended start is not this turn's.
#[test]
fn a_start_repeated_in_its_own_turn_matches_its_original_row() {
    let turn = EOCheckedTurn::start("repeated-start-same-turn", true);
    let mut recorder = turn.recorder();
    recorder.command_started("repeated", SIZE_TEST).expect("the start records");
    let original = session_test_runs(&turn);
    assert_eq!(original.len(), 1, "the start is one execution: {original:?}");
    assert!(!original[0].1, "the original start has not ended");
    for _ in 0..2 {
        recorder.command_started("repeated", SIZE_TEST).expect("the repeated start records");
        assert_eq!(session_test_runs(&turn), original, "a repeated start adds no row");
    }
    turn.wait_for_snapshots();
    end(&mut recorder, "repeated", SIZE_TEST, 0);
    turn.wait_for_snapshots();
    assert!(!take_line(&turn).contains(TOLD), "its end is no late end");
    assert_eq!(
        session_test_runs(&turn),
        vec![(original[0].0.clone(), true)],
        "the one end ends the original row"
    );
    let request = turn.finish();
    let checks = request["occurrence"]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1, "one check: {request:#}");
    assert_eq!(checks[0]["observed_result"], "passed");
    assert_ordinary_observation_has_no_native_credit(&turn, &request);
}
