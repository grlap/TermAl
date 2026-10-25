// Owns the tests of how a watcher-seen file change is judged against a
// launcher gate by when its notification reached the host, in the host's own
// watcher order: a change notified before both the gate's launch and its
// run's registration but handled after them leaves the gate credited; a
// change notified at or after the launch, or after the run registered, or
// for a run this host never saw register, still refuses it, as does one on a
// test settled by its reported end; an old modification time or a wall clock
// that stepped backward never credits a change; a change whose notification
// time is unknown still refuses it; and the refusal names the change's times.
// Does not own the other carried-gate fences or the scratch and Cargo sibling
// exemptions (src/tests/engram_carried_checks.rs, whose fixtures this child
// module uses). New module, a child of src/tests/engram_carried_checks.rs so
// that file does not grow further.
use super::*;

/// A watcher change of `path` whose file was last modified at `mtime_ms`.
fn watched_change(path: &FsPath, mtime_ms: Option<u64>) -> WorkspaceFileChangeEvent {
    WorkspaceFileChangeEvent {
        path: path.to_string_lossy().into_owned(),
        kind: WorkspaceFileChangeKind::Modified,
        root_path: None,
        session_id: None,
        mtime_ms,
        size_bytes: None,
    }
}

/// The time `ms` Unix milliseconds stand for.
fn at_ms(ms: u64) -> std::time::SystemTime {
    std::time::UNIX_EPOCH + Duration::from_millis(ms)
}

/// The time the refusal line shows for a wall-clock `time`.
fn shown_system_time(time: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The launch time of `started_at`, in Unix milliseconds.
fn launch_ms(started_at: &str) -> u64 {
    engram_parse_time(started_at)
        .expect("the launch time parses")
        .timestamp_millis() as u64
}

/// The launch time of the turn's only carried gate, in Unix milliseconds.
fn carried_launch_ms(turn: &CheckedTurn) -> u64 {
    launch_ms(&turn.record(|record| record.engram.carried_checks[0].check.started_at.clone()))
}

/// The time the refusal line shows for `ms`.
fn shown_time(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .expect("the time is representable")
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// The directory of the gate's run in `worktree`, made now, with the host
/// told that the run registered there now, as the launcher's registration
/// does before the run reads anything (`engram_note_run_registered`).
fn register_run(worktree: &FsPath) -> PathBuf {
    let directory = review_runs(worktree).join("test-carried");
    fs::create_dir_all(&directory).expect("the run directory is made");
    engram_note_run_registered(
        &fs::canonicalize(&directory).expect("the run directory canonicalizes"),
    );
    directory
}

/// The line the holder was told about the turn's gate.
fn holder_line(turn: &CheckedTurn) -> String {
    turn.record(|record| record.engram.pending_source_root_line.clone())
        .expect("the holder is told")
}

#[test]
fn a_late_report_of_a_change_notified_before_the_launch_leaves_the_gate_credited() {
    // The 2026-10-22 shape: a patch applied minutes before the launch, whose
    // notification the host handled only after the launch.
    let (turn, worktree) = named_turn("watcher-time-older");
    let arrival = engram_watcher_arrival_now();
    launch_gate(&turn, &worktree, false);
    let before = carried_launch_ms(&turn) - 180_000;
    let changed = fs::canonicalize(&worktree).unwrap().join("README.md");
    turn.state.note_engram_workspace_file_changes_observed(
        &[watched_change(&changed, Some(before))],
        Some(arrival),
    );
    assert!(
        turn.record(|record| record.engram.carried_checks[0].fence.is_none()),
        "a change notified before the launch is in its start snapshot"
    );
    // The run registers after the notification, before it reads anything.
    register_run(&worktree);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    let checkpoint = turn.finish();

    assert_credited(&turn, &checkpoint, &run, "succeeded");
}

#[test]
fn a_change_notified_before_a_launch_still_being_made_does_not_fence_it() {
    // The same late report, handled while the launch command is still
    // running: the launching check must not carry it into its fence.
    let (turn, worktree) = named_turn("watcher-time-launching");
    let cwd = fs::canonicalize(&worktree)
        .expect("the worktree canonicalizes")
        .to_string_lossy()
        .into_owned();
    let arrival = engram_watcher_arrival_now();
    let mut recorder = turn.recorder();
    recorder
        .command_started_in("gate", GATE, Some(GATE), Some(&cwd))
        .expect("the launch should record");
    turn.wait_for_snapshots();
    let started_at = turn.record(|record| {
        record
            .engram
            .active_turn_checks
            .iter()
            .find(|check| check.end.is_none())
            .expect("the launch is an open check")
            .started_at
            .clone()
    });
    let before = launch_ms(&started_at) - 180_000;
    let changed = fs::canonicalize(&worktree).unwrap().join("README.md");
    turn.state.note_engram_workspace_file_changes_observed(
        &[watched_change(&changed, Some(before))],
        Some(arrival),
    );
    recorder
        .command_completed_with_exit(
            "gate",
            GATE,
            "STARTED",
            CommandStatus::Success,
            EngramCommandExit::NotFinished,
        )
        .expect("the launch result should record");
    assert_eq!(turn.record(|record| record.engram.carried_checks.len()), 1);
    assert!(
        turn.record(|record| record.engram.carried_checks[0].fence.is_none()),
        "the launching check carries no fence for a change notified before it"
    );
    register_run(&worktree);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    let checkpoint = turn.finish();

    assert_credited(&turn, &checkpoint, &run, "succeeded");
}

#[test]
fn a_change_notified_at_or_after_the_launch_still_refuses_and_names_its_times() {
    let (turn, worktree) = named_turn("watcher-time-during");
    launch_gate(&turn, &worktree, false);
    let during = carried_launch_ms(&turn) + 1_000;
    let arrival = engram_watcher_arrival_now();
    let changed = fs::canonicalize(&worktree).unwrap().join("README.md");
    turn.state.note_engram_workspace_file_changes_observed(
        &[watched_change(&changed, Some(during))],
        Some(arrival),
    );
    finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();

    assert_refused(&turn, &checkpoint, "a file in its worktree changed");
    let line = holder_line(&turn);
    assert!(
        line.contains(&format!("modified at {}", shown_time(during)))
            && line.contains(&format!("seen at {}", shown_system_time(arrival.at))),
        "the refusal names the change's times: {line}"
    );
}

#[test]
fn an_old_modification_time_notified_during_the_run_still_refuses() {
    // A rename, an archive extract or `touch -d` can bring an old modification
    // time into the worktree while the gate runs, and a swap restored before
    // settlement leaves both source snapshots equal: only the notification's
    // own time shows it came after the launch.
    let (turn, worktree) = named_turn("watcher-time-backdated");
    launch_gate(&turn, &worktree, false);
    let launch = carried_launch_ms(&turn);
    let changed = fs::canonicalize(&worktree).unwrap().join("README.md");
    turn.state.note_engram_workspace_file_changes_observed(
        &[watched_change(&changed, Some(launch - 180_000))],
        Some(engram_watcher_arrival_now()),
    );
    finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();

    assert_refused(&turn, &checkpoint, "a file in its worktree changed");
    assert!(
        holder_line(&turn).contains(&format!("modified at {}", shown_time(launch - 180_000))),
        "the refusal shows the old modification time it did not trust"
    );
}

#[test]
fn an_exempted_change_still_refuses_when_its_run_registered_before_the_notification() {
    // The host stamps a launch when it takes the command's start, which can
    // lag the launcher's own start: a change notified before the stamp may
    // then have reached the run. The run registers with the host before it
    // reads anything, so the exemption holds only for a notification ordered
    // before that registration, whatever the wall clock says of either.
    let (turn, worktree) = named_turn("watcher-time-registered-first");
    register_run(&worktree);
    let arrival = engram_watcher_arrival_now();
    launch_gate(&turn, &worktree, false);
    let launch = carried_launch_ms(&turn);
    let changed = fs::canonicalize(&worktree).unwrap().join("README.md");
    turn.state.note_engram_workspace_file_changes_observed(
        &[watched_change(&changed, Some(launch - 2_000))],
        Some(arrival),
    );
    assert!(
        turn.record(|record| record.engram.carried_checks[0].fence.is_none()),
        "the change is exempted when handled, before its run is known"
    );
    finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    let checkpoint = turn.finish();

    assert_refused(&turn, &checkpoint, "not before its run registered");
}

#[test]
fn an_exempted_change_still_refuses_when_this_host_never_saw_its_run_register() {
    // A run registered before a host restart, or never registered, has no
    // registration in this process's watcher order to place the change by.
    let (turn, worktree) = named_turn("watcher-time-unregistered");
    let arrival = engram_watcher_arrival_now();
    launch_gate(&turn, &worktree, false);
    let launch = carried_launch_ms(&turn);
    let changed = fs::canonicalize(&worktree).unwrap().join("README.md");
    turn.state.note_engram_workspace_file_changes_observed(
        &[watched_change(&changed, Some(launch - 2_000))],
        Some(arrival),
    );
    finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    let checkpoint = turn.finish();

    assert_refused(&turn, &checkpoint, "not before its run registered");
}

#[test]
fn a_test_settled_by_its_reported_end_never_keeps_an_exemption() {
    // A Codex foreground gate still running when its turn closes is carried
    // to its reported end, which has no run registration to place a change
    // by: a change exempted while it launched refuses it.
    let (turn, worktree) = named_turn("watcher-time-reported-end");
    let watched = normalize_user_facing_path(&fs::canonicalize(&worktree).unwrap());
    let cwd = watched.to_string_lossy().into_owned();
    let arrival = engram_watcher_arrival_now();
    turn.recorder()
        .command_started_in("reported-test", GATE, Some(GATE), Some(&cwd))
        .expect("the launch should record");
    turn.wait_for_snapshots();
    turn.state.note_engram_workspace_file_changes_observed(
        &[watched_change(&watched.join("README.md"), Some(0))],
        Some(arrival),
    );
    turn.finish();
    let carried = turn.record(|record| record.engram.carried_checks[0].clone());
    assert!(
        carried.ends_by_report
            && carried.fence.is_none()
            && carried.check.watcher_exempted.is_some(),
        "the running test is carried to its reported end with the exemption"
    );
    let settled = engram_settle_carried_check(
        &carried,
        &Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        std::time::Instant::now() + DEADLOCK_GUARD,
    );
    assert!(
        settled.is_err_and(|why| why.contains("README.md") && why.contains("reported end")),
        "an exemption must not reach a reported-end settlement"
    );
}

#[test]
fn an_exemption_made_during_settlement_is_judged_by_the_next_settlement() {
    // A pre-stamp notification handled while an off-lock settlement runs
    // lands on the live gate only: that settlement must not credit it, and
    // the next one refuses it, since its run registered before it.
    let (turn, worktree) = named_turn("watcher-time-settlement-window");
    register_run(&worktree);
    let arrival = engram_watcher_arrival_now();
    launch_gate(&turn, &worktree, false);
    let launch = carried_launch_ms(&turn);
    let canonical = fs::canonicalize(&worktree).expect("the worktree canonicalizes");
    finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    let runtime_token = turn.record(|record| record.runtime.runtime_token().unwrap());
    let gate = install_test_engram_turn_report_gate(&turn.state, &turn.session_id);
    let finishing = {
        let state = turn.state.clone();
        let session_id = turn.session_id.clone();
        std::thread::spawn(move || {
            state
                .finish_turn_ok_if_runtime_matches(&session_id, &runtime_token)
                .unwrap();
        })
    };
    gate.wait_until_claimed();
    turn.state.note_engram_workspace_file_changes_observed(
        &[watched_change(
            &canonical.join("README.md"),
            Some(launch - 2_000),
        )],
        Some(arrival),
    );
    gate.release();
    finishing.join().unwrap();
    let checkpoint = turn
        .transport
        .requests()
        .into_iter()
        .find(|request| request.request["operation"] == "turn_checkpoint")
        .unwrap()
        .request;
    assert!(
        checkpoint.get("verification_evidence").is_none(),
        "the settlement that missed the exemption credits nothing: {checkpoint:#}"
    );
    let carried = turn.record(|record| record.engram.carried_checks[0].clone());
    assert!(carried.check.watcher_exempted.is_some());
    match engram_settle_carried_check(
        &carried,
        &Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        std::time::Instant::now() + DEADLOCK_GUARD,
    ) {
        Err(why) => assert!(why.contains("not before its run registered"), "{why}"),
        Ok(_) => panic!("a run registered before the notification must not be credited"),
    }
}

#[test]
fn a_change_after_the_launch_still_refuses_a_gate_an_earlier_change_left_exempted() {
    let (turn, worktree) = named_turn("watcher-time-exempted-then-written");
    let before = engram_watcher_arrival_now();
    launch_gate(&turn, &worktree, false);
    let launch = carried_launch_ms(&turn);
    let canonical = fs::canonicalize(&worktree).unwrap();
    turn.state.note_engram_workspace_file_changes_observed(
        &[watched_change(
            &canonical.join("early.txt"),
            Some(launch - 2_000),
        )],
        Some(before),
    );
    assert!(turn.record(|record| record.engram.carried_checks[0].fence.is_none()));
    turn.state.note_engram_workspace_file_changes_observed(
        &[watched_change(
            &canonical.join("later.txt"),
            Some(launch + 1_000),
        )],
        Some(engram_watcher_arrival_now()),
    );
    finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();

    assert_refused(&turn, &checkpoint, "a file in its worktree changed");
    assert!(
        holder_line(&turn).contains("later.txt"),
        "the refusal names the change after the launch: {}",
        holder_line(&turn)
    );
}

#[test]
fn a_notification_after_the_launch_still_refuses_when_the_wall_clock_stepped_back() {
    // The host clock moved backward after the launch, so the notification's
    // wall time reads before the launch: only the watcher order shows it came
    // after it.
    let (turn, worktree) = named_turn("watcher-time-clock-back");
    launch_gate(&turn, &worktree, false);
    let launch = carried_launch_ms(&turn);
    let arrival = EngramWatcherArrival {
        order: engram_watcher_order_tick(),
        at: at_ms(launch - 180_000),
    };
    let changed = fs::canonicalize(&worktree).unwrap().join("README.md");
    turn.state.note_engram_workspace_file_changes_observed(
        &[watched_change(&changed, Some(launch - 180_000))],
        Some(arrival),
    );
    finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();

    assert_refused(&turn, &checkpoint, "a file in its worktree changed");
}

#[test]
fn a_change_whose_notification_time_is_unknown_still_refuses_and_says_so() {
    // Neither an old modification time nor a missing one, as of a deleted
    // file, credits a change the host cannot place in time.
    for (label, mtime_ms) in [
        ("watcher-time-unknown-old", Some(0u64)),
        ("watcher-time-unknown-none", None),
    ] {
        let (turn, worktree) = named_turn(label);
        launch_gate(&turn, &worktree, false);
        let mtime_ms = mtime_ms.map(|_| carried_launch_ms(&turn) - 180_000);
        let changed = fs::canonicalize(&worktree).unwrap().join("README.md");
        turn.state
            .note_engram_workspace_file_changes(&[watched_change(&changed, mtime_ms)]);
        finish_run(&worktree, "passed", &"f".repeat(64));
        let checkpoint = turn.finish();

        assert_refused(&turn, &checkpoint, "a file in its worktree changed");
        if mtime_ms.is_none() {
            assert!(
                holder_line(&turn).contains("modification time unknown"),
                "the refusal says the change's time is unknown: {}",
                holder_line(&turn)
            );
        }
    }
}
