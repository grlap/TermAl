// Owns workspace-watcher coverage for Cargo's temporary target-directory
// siblings, including their rename and the boundary with ordinary source
// paths. Does not own other carried-check fencing; uses the fixtures from
// engram_carried_checks.rs, where this module is registered.
use super::*;

fn change(path: &FsPath, kind: WorkspaceFileChangeKind) -> WorkspaceFileChangeEvent {
    WorkspaceFileChangeEvent {
        path: path.to_string_lossy().into_owned(),
        kind,
        root_path: None,
        session_id: None,
        mtime_ms: None,
        size_bytes: None,
    }
}

#[test]
fn cargo_target_sibling_changes_leave_a_carried_gate_unfenced_in_artifact_custody() {
    for renamed in [false, true] {
        let (turn, worktree) = ordinary_carried_turn(if renamed {
            "cargo-target-renamed"
        } else {
            "cargo-target-present"
        });
        fs::write(worktree.join(".gitignore"), "target/\n").unwrap();
        ordinary_launch_gate(&turn, &worktree, false);
        let watched = normalize_user_facing_path(&fs::canonicalize(&worktree).unwrap());
        let sibling = watched.join("targetVsXPcL");
        fs::create_dir(&sibling).unwrap();
        fs::write(sibling.join("CACHEDIR.TAG"), "cargo cache\n").unwrap();
        if renamed {
            fs::rename(&sibling, watched.join("target")).unwrap();
        } else {
            fs::create_dir(watched.join("target")).unwrap();
        }
        // Watcher delivery can happen after Cargo has renamed the directory.
        turn.state.note_engram_workspace_file_changes(&[
            change(&sibling, WorkspaceFileChangeKind::Created),
            change(
                &sibling.join("CACHEDIR.TAG"),
                WorkspaceFileChangeKind::Modified,
            ),
            change(
                &sibling.join("gone/deep/cache"),
                WorkspaceFileChangeKind::Deleted,
            ),
            change(&sibling, WorkspaceFileChangeKind::Deleted),
        ]);
        assert_eq!(
            turn.record(|record| record.engram.carried_checks[0].fence.clone()),
            None,
            "Cargo's temporary sibling must not fence the gate (renamed={renamed})"
        );
        if !renamed {
            // Leave only ignored target/ at the closing source snapshot.
            fs::remove_file(sibling.join("CACHEDIR.TAG")).unwrap();
            fs::remove_dir(&sibling).unwrap();
        }
        let run = finish_run(&worktree, "passed", &"f".repeat(64));
        turn.state.poll_engram_carried_runs();
        assert_carried_artifact_custody(&turn, &run, true);
    }
}

#[test]
fn cargo_target_sibling_early_create_then_rename_keeps_artifact_custody() {
    let (turn, worktree) = ordinary_carried_turn("cargo-target-early-create");
    fs::write(worktree.join(".gitignore"), "target/\n").unwrap();
    ordinary_launch_gate(&turn, &worktree, false);
    let watched = normalize_user_facing_path(&fs::canonicalize(&worktree).unwrap());
    let sibling = watched.join("targetVsXPcL");
    fs::create_dir(&sibling).unwrap();
    // Deliver immediately, before Cargo can tag and rename its new directory.
    turn.state
        .note_engram_workspace_file_changes(&[change(&sibling, WorkspaceFileChangeKind::Created)]);
    assert_eq!(
        turn.record(|record| record.engram.carried_checks[0].fence.clone()),
        None,
        "a candidate remains data rather than latching a source fence"
    );
    // A nonterminal poll must not judge the still-unrenamed candidate either.
    turn.state.poll_engram_carried_runs();
    assert!(turn.record(|record| record.engram.carried_checks[0].fence.is_none()));
    fs::write(sibling.join("CACHEDIR.TAG"), "cargo cache\n").unwrap();
    fs::rename(&sibling, watched.join("target")).unwrap();
    turn.state
        .note_engram_workspace_file_changes(&[change(&sibling, WorkspaceFileChangeKind::Deleted)]);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    assert_carried_artifact_custody(&turn, &run, true);
}

#[test]
fn cargo_target_sibling_reported_end_preserves_inherited_candidate_but_fences_new_delivery() {
    for inherited in [false, true] {
        let (turn, worktree) = ordinary_carried_turn(if inherited {
            "cargo-target-inherited-reported-end"
        } else {
            "cargo-target-reported-end"
        });
        fs::write(worktree.join(".gitignore"), "target/\n").unwrap();
        fs::create_dir(worktree.join("target")).unwrap();
        let watched = normalize_user_facing_path(&fs::canonicalize(&worktree).unwrap());
        let cwd = watched.to_string_lossy().into_owned();
        let command = if inherited { GATE } else { SIZE_TEST };
        turn.recorder()
            .command_started_in("reported-test", command, Some(command), Some(&cwd))
            .unwrap();
        turn.wait_for_snapshots();
        let event = change(
            &watched.join("targetVsXPcL"),
            WorkspaceFileChangeKind::Created,
        );
        if inherited {
            turn.state
                .note_engram_workspace_file_changes(&[event.clone()]);
        }
        assert_ordinary_observation_has_no_native_credit(&turn, &turn.finish());
        assert!(turn.record(|record| record.engram.carried_checks[0].ends_by_report));
        if inherited {
            let carried = turn.record(|record| record.engram.carried_checks[0].clone());
            assert!(carried.fence.is_none());
            assert_eq!(carried.check.watcher_cargo_candidates.len(), 1);
            assert!(carried.check.watcher_cargo_candidates[0].path.contains("targetVsXPcL"));
            // Closing preserves the candidate as data. The deleted
            // reported-end settlement can no longer consume or credit it.
            assert_ordinary_turn_has_no_native_requests(&turn);
        } else {
            turn.state.note_engram_workspace_file_changes(&[event]);
            assert!(turn.record(|record| {
                let carried = &record.engram.carried_checks[0];
                carried
                    .fence
                    .as_deref()
                    .is_some_and(|why| why.contains("targetVsXPcL"))
                    && carried.check.watcher_cargo_candidates.is_empty()
            }));
        }
    }
}

#[test]
fn cargo_target_sibling_seen_during_launch_stays_pending_when_carried() {
    let (turn, worktree) = ordinary_carried_turn("cargo-target-during-launch");
    fs::write(worktree.join(".gitignore"), "target/\n").unwrap();
    let watched = normalize_user_facing_path(&fs::canonicalize(&worktree).unwrap());
    let cwd = watched.to_string_lossy().into_owned();
    let mut recorder = turn.recorder();
    recorder
        .command_started_in("gate", GATE, Some(GATE), Some(&cwd))
        .unwrap();
    turn.wait_for_snapshots();
    let sibling = watched.join("targetVsXPcL");
    fs::create_dir(&sibling).unwrap();
    turn.state
        .note_engram_workspace_file_changes(&[change(&sibling, WorkspaceFileChangeKind::Created)]);
    recorder
        .command_completed_with_exit(
            "gate",
            GATE,
            "STARTED",
            CommandStatus::Success,
            EngramCommandExit::NotFinished,
        )
        .unwrap();
    assert_eq!(
        turn.record(|record| record.engram.carried_checks[0]
            .check
            .watcher_cargo_candidates
            .len()),
        1
    );
    fs::rename(&sibling, watched.join("target")).unwrap();
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    assert_carried_artifact_custody(&turn, &run, true);
}

#[test]
fn cargo_target_sibling_arriving_during_off_lock_poll_keeps_its_candidate() {
    let (turn, worktree) = ordinary_carried_turn("cargo-target-poll-window");
    fs::write(worktree.join(".gitignore"), "target/\n").unwrap();
    fs::create_dir(worktree.join("target")).unwrap();
    ordinary_launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let (paused, release) = install_test_engram_carried_poll_pause(&turn.state);
    let polling = {
        let state = turn.state.clone();
        std::thread::spawn(move || state.poll_engram_carried_runs())
    };
    paused.recv_timeout(DEADLOCK_GUARD).expect("actual off-lock artifact read");
    let watched = normalize_user_facing_path(&fs::canonicalize(&worktree).unwrap());
    turn.state.note_engram_workspace_file_changes(&[change(
        &watched.join("targetAb1234"), WorkspaceFileChangeKind::Deleted)]);
    release.send(()).unwrap();
    polling.join().unwrap();
    turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        assert!(carried.fence.is_none() && carried.check.watcher_exempted.is_none());
        assert_eq!(carried.check.watcher_cargo_candidates.len(), 1);
        let candidate = &carried.check.watcher_cargo_candidates[0];
        assert!(!candidate.before_launch);
        assert!(candidate.path.contains("targetAb1234"));
    });
    assert_carried_artifact_custody(&turn, &run, true);
}

#[test]
fn cargo_target_sibling_prelaunch_arrival_during_off_lock_poll_keeps_its_order() {
    let (turn, worktree) = ordinary_carried_turn("cargo-target-prelaunch-late");
    fs::write(worktree.join(".gitignore"), "target/\n").unwrap();
    fs::create_dir(worktree.join("target")).unwrap();
    let before = engram_watcher_arrival_now();
    ordinary_launch_gate(&turn, &worktree, false);
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let (paused, release) = install_test_engram_carried_poll_pause(&turn.state);
    let polling = {
        let state = turn.state.clone();
        std::thread::spawn(move || state.poll_engram_carried_runs())
    };
    paused.recv_timeout(DEADLOCK_GUARD).expect("actual off-lock artifact read");
    let watched = normalize_user_facing_path(&fs::canonicalize(&worktree).unwrap());
    turn.state.note_engram_workspace_file_changes_observed(
        &[change(&watched.join("targetAb1234"), WorkspaceFileChangeKind::Deleted)],
        Some(before));
    release.send(()).unwrap();
    polling.join().unwrap();
    turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        assert!(carried.fence.is_none() && carried.check.watcher_exempted.is_none());
        assert_eq!(carried.check.watcher_cargo_candidates.len(), 1);
        let candidate = &carried.check.watcher_cargo_candidates[0];
        assert!(candidate.before_launch);
        assert!(candidate.path.contains("targetAb1234"));
    });
    assert_carried_artifact_custody(&turn, &run, true);
}

#[test]
fn cargo_target_sibling_notified_between_registration_and_stamp_keeps_candidate_custody() {
    // The run registers, Cargo makes its temporary sibling, and only then is
    // the delayed command start stamped: the notification falls between the
    // run's registration and the stamp. A sibling is a candidate, not a
    // timing exemption or verification evidence.
    let (turn, worktree) = ordinary_carried_turn("cargo-target-between-registration-and-stamp");
    fs::write(worktree.join(".gitignore"), "target/\n").unwrap();
    let directory = review_runs(&worktree).join("test-carried");
    fs::create_dir_all(&directory).unwrap();
    engram_note_run_registered(&fs::canonicalize(&directory).unwrap());
    let registration = engram_run_registration_order(&directory).unwrap();
    let arrival = engram_watcher_arrival_now();
    ordinary_launch_gate(&turn, &worktree, false);
    assert!(registration < arrival.order);
    assert!(arrival.order < turn.record(|record| record.engram.carried_checks[0].check.started_order.expect("actual command start has a host order")));
    let watched = normalize_user_facing_path(&fs::canonicalize(&worktree).unwrap());
    let sibling = watched.join("targetAb1234");
    fs::create_dir(&sibling).unwrap();
    fs::rename(&sibling, watched.join("target")).unwrap();
    turn.state.note_engram_workspace_file_changes_observed(
        &[change(&sibling, WorkspaceFileChangeKind::Created)],
        Some(arrival),
    );
    assert!(
        turn.record(|record| record.engram.carried_checks[0].fence.is_none()
            && record.engram.carried_checks[0]
                .check
                .watcher_exempted
                .is_none()),
        "a sibling is kept as a candidate, not exempted"
    );
    turn.record(|record| {
        let candidates = &record.engram.carried_checks[0].check.watcher_cargo_candidates;
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].before_launch);
        assert!(candidates[0].path.contains("targetAb1234"));
    });
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    assert_carried_artifact_custody(&turn, &run, true);
}

#[test]
fn cargo_target_sibling_similar_source_names_still_fence_a_carried_gate() {
    for relative in [
        "targets.txt",
        "target-notes.md",
        "targetAb123",
        "targetAb12345",
        "targetAb_123",
        "TargetAb1234",
        "src/targetAb1234/cache",
        "target-notes.md/cache",
    ] {
        let (turn, worktree) = ordinary_carried_turn("cargo-target-source-control");
        fs::create_dir(worktree.join("target")).unwrap();
        ordinary_launch_gate(&turn, &worktree, false);
        let watched = normalize_user_facing_path(&fs::canonicalize(&worktree).unwrap());
        let path = watched.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "source\n").unwrap();
        turn.state.note_engram_workspace_file_changes(&[change(
            &path,
            WorkspaceFileChangeKind::Modified,
        )]);
        let fence = turn
            .record(|record| record.engram.carried_checks[0].fence.clone())
            .expect("an ordinary source path must still fence the gate");
        assert!(
            fence.contains(&path.to_string_lossy().to_string()),
            "{fence}"
        );
    }
}

#[test]
fn cargo_target_candidate_without_a_directory_is_data_not_a_verification_exemption() {
    for target_is_file in [false, true] {
        let (turn, worktree) = ordinary_carried_turn("cargo-target-no-destination");
        let target = worktree.join("target");
        if target_is_file {
            fs::write(&target, "ordinary file\n").unwrap();
        }
        assert!(!target.is_dir());
        assert_eq!(target.is_file(), target_is_file);
        ordinary_launch_gate(&turn, &worktree, false);
        let watched = normalize_user_facing_path(&fs::canonicalize(&worktree).unwrap());
        let sibling = watched.join("targetAb1234");
        fs::create_dir(&sibling).unwrap();
        turn.state.note_engram_workspace_file_changes(&[change(
            &sibling, WorkspaceFileChangeKind::Created)]);
        fs::remove_dir(&sibling).unwrap();
        let run = finish_run(&worktree, "passed", &"f".repeat(64));
        turn.state.poll_engram_carried_runs();
        turn.record(|record| {
            let carried = &record.engram.carried_checks[0];
            assert!(carried.fence.is_none());
            assert_eq!(carried.check.watcher_cargo_candidates.len(), 1);
            assert!(carried.check.watcher_cargo_candidates[0].path.contains("targetAb1234"));
            assert!(carried.check.watcher_exempted.is_none());
        });
        // The former target-directory credit decision was checkpoint-only.
        // The real artifact reader now preserves custody without any credit.
        assert_carried_artifact_custody(&turn, &run, true);
        assert!(!target.is_dir());
    }
}

#[test]
fn cargo_target_sibling_pending_overflow_fences_even_if_target_appears_later() {
    let (turn, worktree) = ordinary_carried_turn("cargo-target-pending-overflow");
    fs::write(worktree.join(".gitignore"), "target/\n").unwrap();
    ordinary_launch_gate(&turn, &worktree, false);
    let watched = normalize_user_facing_path(&fs::canonicalize(&worktree).unwrap());
    for index in 0..=ENGRAM_CARRIED_WATCH_DIRECTORY_LIMIT {
        let sibling = watched.join(format!("target{index:06}"));
        turn.state.note_engram_workspace_file_changes(&[change(
            &sibling,
            WorkspaceFileChangeKind::Created,
        )]);
    }
    fs::create_dir(watched.join("target")).unwrap();
    let fence = turn
        .record(|record| record.engram.carried_checks[0].fence.clone())
        .expect("pending-candidate overflow must fail closed");
    assert!(fence.contains("pending Cargo"), "{fence}");
    finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    assert_carried_fence_custody(&turn, "pending Cargo");
}
