// Watcher delivery order and wall-clock diagnostics remain ordinary custody
// facts. A terminal artifact is read without checkpoint settlement, replay or
// automatic verification; registration chronology is retained independently.
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
fn carried_launch_ms(turn: &EOCheckedTurn) -> u64 {
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

/// The actual retained watcher fence, independently of removed checkpoint output.
fn recorded_fence(turn: &EOCheckedTurn) -> String {
    turn.record(|record| record.engram.carried_checks[0].fence.clone())
        .expect("the watcher recorded its cause")
}

#[test]
fn coalesced_path_crossing_launch_uses_latest_arrival_and_keeps_first_provenance() {
    let (turn, worktree) = ordinary_carried_turn("coalesced-watcher-interval");
    let before = engram_watcher_arrival_now();
    ordinary_launch_gate(&turn, &worktree, false);
    let after = engram_watcher_arrival_now();
    let mut pending = WorkspacePendingChanges::default();
    let path = fs::canonicalize(&worktree).unwrap().join("README.md");
    for arrival in [before, after] {
        pending.insert(
            &path,
            WorkspaceDirtyFact {
                kind: WorkspaceFileChangeKind::Modified,
                first: arrival,
                last: arrival,
                raw_kind: None,
                observation: None,
            },
            std::time::Instant::now(),
        );
    }
    let batch = pending.take();
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].1.first.order, before.order);
    assert_eq!(batch[0].1.last.order, after.order);
    turn.state.process_workspace_dirty_batch(batch);
    assert!(turn.record(|record| record.engram.carried_checks[0].fence.is_some()));
}

#[test]
fn covering_parent_marker_reaches_check_in_nested_worktree_without_scratch_exemption() {
    let (turn, worktree) = ordinary_carried_turn("coalesced-watcher-directory");
    ordinary_launch_gate(&turn, &worktree, false);
    let arrival = engram_watcher_arrival_now();
    let parent = fs::canonicalize(&worktree)
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    turn.state.process_workspace_dirty_batch(vec![(
        parent,
        WorkspaceDirtyFact {
            kind: WorkspaceFileChangeKind::Subtree,
            first: arrival,
            last: arrival,
            raw_kind: None,
            observation: None,
        },
    )]);
    assert!(turn.record(|record| record.engram.carried_checks[0].fence.is_some()));
}

#[test]
fn a_late_report_before_launch_keeps_unfenced_artifact_custody() {
    // The 2026-10-22 shape: a patch applied minutes before the launch, whose
    // notification the host handled only after the launch.
    let (turn, worktree) = ordinary_carried_turn("watcher-time-older");
    let arrival = engram_watcher_arrival_now();
    ordinary_launch_gate(&turn, &worktree, false);
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
    assert_carried_artifact_custody(&turn, &run, true);
}

#[test]
fn a_change_notified_before_a_launch_still_being_made_does_not_fence_it() {
    // The same late report, handled while the launch command is still
    // running: the launching check must not carry it into its fence.
    let (turn, worktree) = ordinary_carried_turn("watcher-time-launching");
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
    assert_carried_artifact_custody(&turn, &run, true);
}

#[test]
fn a_change_notified_at_or_after_the_launch_still_refuses_and_names_its_times() {
    let (turn, worktree) = ordinary_carried_turn("watcher-time-during");
    ordinary_launch_gate(&turn, &worktree, false);
    let during = carried_launch_ms(&turn) + 1_000;
    let arrival = engram_watcher_arrival_now();
    let changed = fs::canonicalize(&worktree).unwrap().join("README.md");
    turn.state.note_engram_workspace_file_changes_observed(
        &[watched_change(&changed, Some(during))],
        Some(arrival),
    );
    finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_fence_custody(&turn, "a file in its worktree changed");
    let line = recorded_fence(&turn);
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
    let (turn, worktree) = ordinary_carried_turn("watcher-time-backdated");
    ordinary_launch_gate(&turn, &worktree, false);
    let launch = carried_launch_ms(&turn);
    let changed = fs::canonicalize(&worktree).unwrap().join("README.md");
    turn.state.note_engram_workspace_file_changes_observed(
        &[watched_change(&changed, Some(launch - 180_000))],
        Some(engram_watcher_arrival_now()),
    );
    finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_fence_custody(&turn, "a file in its worktree changed");
    assert!(
        recorded_fence(&turn).contains(&format!("modified at {}", shown_time(launch - 180_000))),
        "the refusal shows the old modification time it did not trust"
    );
}

#[test]
fn a_pre_stamp_exemption_preserves_registration_before_arrival_without_verification() {
    let (turn, worktree) = ordinary_carried_turn("watcher-time-registered-first");
    let directory = register_run(&worktree);
    let registration = engram_run_registration_order(&directory).expect("actual host registration");
    let arrival = engram_watcher_arrival_now();
    ordinary_launch_gate(&turn, &worktree, false);
    let (launch_order, launch) = turn.record(|record| {
        let check = &record.engram.carried_checks[0].check;
        (
            check
                .started_order
                .expect("actual command start has a host order"),
            launch_ms(&check.started_at),
        )
    });
    assert!(registration < arrival.order && arrival.order < launch_order);
    let changed = fs::canonicalize(&worktree).unwrap().join("README.md");
    turn.state.note_engram_workspace_file_changes_observed(
        &[watched_change(&changed, Some(launch - 2_000))],
        Some(arrival),
    );
    turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        assert!(carried.fence.is_none());
        let (cause, order) = carried.check.watcher_exempted.as_ref().unwrap();
        assert_eq!(*order, arrival.order);
        assert!(cause.contains("README.md"), "{cause}");
    });
    // Registration chronology remains evidence; the deleted checkpoint
    // settlement no longer turns it into a verification decision.
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    assert_carried_artifact_custody(&turn, &run, true);
}

#[test]
fn an_exemption_with_unknown_registration_remains_uncredited_custody() {
    let (turn, worktree) = ordinary_carried_turn("watcher-time-unregistered");
    let arrival = engram_watcher_arrival_now();
    ordinary_launch_gate(&turn, &worktree, false);
    let launch = carried_launch_ms(&turn);
    let changed = fs::canonicalize(&worktree).unwrap().join("README.md");
    turn.state.note_engram_workspace_file_changes_observed(
        &[watched_change(&changed, Some(launch - 2_000))],
        Some(arrival),
    );
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    assert_eq!(
        engram_run_registration_order(&run),
        None,
        "filesystem discovery cannot invent this host's registration order"
    );
    turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        assert!(carried.fence.is_none());
        assert_eq!(
            carried.check.watcher_exempted.as_ref().unwrap().1,
            arrival.order
        );
    });
    assert_carried_artifact_custody(&turn, &run, true);
}

#[test]
fn a_reported_end_retains_prior_exemption_but_new_delivery_fences_it() {
    let (turn, worktree) = ordinary_carried_turn("watcher-time-reported-end");
    let watched = normalize_user_facing_path(&fs::canonicalize(&worktree).unwrap());
    let cwd = watched.to_string_lossy().into_owned();
    let arrival = engram_watcher_arrival_now();
    turn.recorder()
        .command_started_in("reported-test", GATE, Some(GATE), Some(&cwd))
        .unwrap();
    turn.wait_for_snapshots();
    let change = watched_change(&watched.join("README.md"), Some(0));
    turn.state
        .note_engram_workspace_file_changes_observed(&[change.clone()], Some(arrival));
    assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
    turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        assert!(carried.ends_by_report && carried.fence.is_none());
        assert_eq!(
            carried.check.watcher_exempted.as_ref().unwrap().1,
            arrival.order
        );
    });
    // Even a pre-launch arrival delivered after ordinary closing is fenced:
    // a reported end has no artifact-registration boundary to excuse it.
    turn.state
        .note_engram_workspace_file_changes_observed(&[change], Some(arrival));
    turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        assert!(carried.ends_by_report);
        assert!(carried.fence.as_deref().unwrap().contains("README.md"));
    });
    assert_ordinary_turn_has_no_native_requests(&turn);
}

#[test]
fn an_exemption_delivered_during_off_lock_artifact_read_is_not_lost() {
    let (turn, worktree) = ordinary_carried_turn("watcher-time-poll-window");
    let directory = register_run(&worktree);
    let registration = engram_run_registration_order(&directory).unwrap();
    let arrival = engram_watcher_arrival_now();
    ordinary_launch_gate(&turn, &worktree, false);
    let launch = carried_launch_ms(&turn);
    let canonical = fs::canonicalize(&worktree).unwrap();
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let (paused, release) = install_test_engram_carried_poll_pause(&turn.state);
    let polling = {
        let state = turn.state.clone();
        std::thread::spawn(move || state.poll_engram_carried_runs())
    };
    paused
        .recv_timeout(DEADLOCK_GUARD)
        .expect("real poll paused after off-lock reads");
    turn.state.note_engram_workspace_file_changes_observed(
        &[watched_change(
            &canonical.join("README.md"),
            Some(launch - 2_000),
        )],
        Some(arrival),
    );
    release.send(()).unwrap();
    polling.join().unwrap();
    turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        let (cause, order) = carried.check.watcher_exempted.as_ref().unwrap();
        assert!(cause.contains("README.md"), "{cause}");
        assert_eq!(*order, arrival.order);
        assert!(
            registration < *order
                && *order
                    < carried
                        .check
                        .started_order
                        .expect("actual command start has a host order")
        );
        assert!(carried.fence.is_none());
    });
    assert_carried_artifact_custody(&turn, &run, true);
}

#[test]
fn a_change_after_the_launch_still_refuses_a_gate_an_earlier_change_left_exempted() {
    let (turn, worktree) = ordinary_carried_turn("watcher-time-exempted-then-written");
    let before = engram_watcher_arrival_now();
    ordinary_launch_gate(&turn, &worktree, false);
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
    assert_carried_fence_custody(&turn, "a file in its worktree changed");
    assert!(
        recorded_fence(&turn).contains("later.txt"),
        "the refusal names the change after the launch: {}",
        recorded_fence(&turn)
    );
}

#[test]
fn a_notification_after_the_launch_still_refuses_when_the_wall_clock_stepped_back() {
    // The host clock moved backward after the launch, so the notification's
    // wall time reads before the launch: only the watcher order shows it came
    // after it.
    let (turn, worktree) = ordinary_carried_turn("watcher-time-clock-back");
    ordinary_launch_gate(&turn, &worktree, false);
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
    assert_carried_fence_custody(&turn, "a file in its worktree changed");
}

#[test]
fn a_change_whose_notification_time_is_unknown_still_refuses_and_says_so() {
    // Neither an old modification time nor a missing one, as of a deleted
    // file, credits a change the host cannot place in time.
    for (label, mtime_ms) in [
        ("watcher-time-unknown-old", Some(0u64)),
        ("watcher-time-unknown-none", None),
    ] {
        let (turn, worktree) = ordinary_carried_turn(label);
        ordinary_launch_gate(&turn, &worktree, false);
        let mtime_ms = mtime_ms.map(|_| carried_launch_ms(&turn) - 180_000);
        let changed = fs::canonicalize(&worktree).unwrap().join("README.md");
        turn.state
            .note_engram_workspace_file_changes(&[watched_change(&changed, mtime_ms)]);
        finish_run(&worktree, "passed", &"f".repeat(64));
        assert_carried_fence_custody(&turn, "a file in its worktree changed");
        if mtime_ms.is_none() {
            assert!(
                recorded_fence(&turn).contains("modification time unknown"),
                "the refusal says the change's time is unknown: {}",
                recorded_fence(&turn)
            );
        }
    }
}
