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
fn cargo_target_sibling_changes_leave_a_carried_gate_unfenced_and_credited() {
    for renamed in [false, true] {
        let (turn, worktree) = named_turn(if renamed {
            "cargo-target-renamed"
        } else {
            "cargo-target-present"
        });
        fs::write(worktree.join(".gitignore"), "target/\n").unwrap();
        launch_gate(&turn, &worktree, false);
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
        let checkpoint = turn.finish();
        assert_credited(&turn, &checkpoint, &run, "succeeded");
    }
}

#[test]
fn cargo_target_sibling_early_create_then_rename_is_credited() {
    let (turn, worktree) = named_turn("cargo-target-early-create");
    fs::write(worktree.join(".gitignore"), "target/\n").unwrap();
    launch_gate(&turn, &worktree, false);
    let watched = normalize_user_facing_path(&fs::canonicalize(&worktree).unwrap());
    let sibling = watched.join("targetVsXPcL");
    fs::create_dir(&sibling).unwrap();
    // Deliver immediately, before Cargo can tag and rename its new directory.
    turn.state
        .note_engram_workspace_file_changes(&[change(&sibling, WorkspaceFileChangeKind::Created)]);
    assert_eq!(
        turn.record(|record| record.engram.carried_checks[0].fence.clone()),
        None,
        "a candidate must wait for terminal settlement, not latch a fence"
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
    let checkpoint = turn.finish();
    assert_credited(&turn, &checkpoint, &run, "succeeded");
}

#[test]
fn cargo_target_sibling_does_not_exempt_a_reported_end_carry() {
    for inherited in [false, true] {
        let (turn, worktree) = named_turn(if inherited {
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
        turn.finish();
        assert!(turn.record(|record| record.engram.carried_checks[0].ends_by_report));
        if inherited {
            let carried = turn.record(|record| record.engram.carried_checks[0].clone());
            let result = engram_settle_carried_check(
                &carried,
                &Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                std::time::Instant::now() + DEADLOCK_GUARD,
            );
            assert!(
                result.is_err_and(|why| why.contains("targetVsXPcL")),
                "an inherited candidate must not bypass reported-end settlement"
            );
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
    let (turn, worktree) = named_turn("cargo-target-during-launch");
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
    let checkpoint = turn.finish();
    assert_credited(&turn, &checkpoint, &run, "succeeded");
}

#[test]
fn cargo_target_sibling_arriving_after_off_lock_settlement_is_not_credited() {
    let (turn, worktree) = named_turn("cargo-target-after-settlement");
    fs::write(worktree.join(".gitignore"), "target/\n").unwrap();
    fs::create_dir(worktree.join("target")).unwrap();
    launch_gate(&turn, &worktree, false);
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
    let watched = normalize_user_facing_path(&fs::canonicalize(&worktree).unwrap());
    turn.state.note_engram_workspace_file_changes(&[change(
        &watched.join("targetAb1234"),
        WorkspaceFileChangeKind::Deleted,
    )]);
    gate.release();
    finishing.join().unwrap();
    let checkpoint = turn
        .transport
        .requests()
        .into_iter()
        .find(|request| request.request["operation"] == "turn_checkpoint")
        .unwrap()
        .request;
    assert_refused(&turn, &checkpoint, "targetAb1234");
}

#[test]
fn cargo_target_sibling_notified_before_the_launch_but_handled_in_settlement_is_deferred() {
    // The same delivery during off-lock settlement as above, but of a
    // notification that reached the host before the launch: the settlement
    // already under way never judged it, so the gate stays carried for the
    // next one, which credits it because target/ is a directory.
    let (turn, worktree) = named_turn("cargo-target-prelaunch-late");
    fs::write(worktree.join(".gitignore"), "target/\n").unwrap();
    fs::create_dir(worktree.join("target")).unwrap();
    let before = engram_watcher_arrival_now();
    launch_gate(&turn, &worktree, false);
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
    let watched = normalize_user_facing_path(&fs::canonicalize(&worktree).unwrap());
    turn.state.note_engram_workspace_file_changes_observed(
        &[change(
            &watched.join("targetAb1234"),
            WorkspaceFileChangeKind::Deleted,
        )],
        Some(before),
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
        "the settlement that missed the candidate credits nothing: {checkpoint:#}"
    );
    let carried = turn.record(|record| record.engram.carried_checks[0].clone());
    assert!(
        carried.fence.is_none()
            && carried.check.watcher_exempted.is_none()
            && carried
                .check
                .watcher_cargo_candidates
                .iter()
                .any(|candidate| candidate.before_launch),
        "the gate stays carried with the pre-launch candidate for its next settlement"
    );
    match engram_settle_carried_check(
        &carried,
        &Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        std::time::Instant::now() + DEADLOCK_GUARD,
    ) {
        Ok(Some(_)) => {}
        Ok(None) => panic!("the next settlement's snapshots were not ready"),
        Err(why) => panic!("target/ is a directory, so the candidate is credited: {why}"),
    }
}

#[test]
fn cargo_target_sibling_notified_after_registration_but_before_a_late_stamp_is_credited() {
    // The run registers, Cargo makes its temporary sibling, and only then is
    // the delayed command start stamped: the notification falls between the
    // run's registration and the stamp. A sibling takes no timing exemption;
    // it is judged by target/ at terminal settlement as always.
    let (turn, worktree) = named_turn("cargo-target-between-registration-and-stamp");
    fs::write(worktree.join(".gitignore"), "target/\n").unwrap();
    let directory = review_runs(&worktree).join("test-carried");
    fs::create_dir_all(&directory).unwrap();
    engram_note_run_registered(&fs::canonicalize(&directory).unwrap());
    let arrival = engram_watcher_arrival_now();
    launch_gate(&turn, &worktree, false);
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
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    let checkpoint = turn.finish();
    assert_credited(&turn, &checkpoint, &run, "succeeded");
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
        let (turn, worktree) = named_turn("cargo-target-source-control");
        fs::create_dir(worktree.join("target")).unwrap();
        launch_gate(&turn, &worktree, false);
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
fn cargo_target_sibling_without_the_target_directory_still_fences_a_carried_gate() {
    for target_is_file in [false, true] {
        let (turn, worktree) = named_turn("cargo-target-no-destination");
        if target_is_file {
            fs::write(worktree.join("target"), "ordinary file\n").unwrap();
        }
        launch_gate(&turn, &worktree, false);
        let watched = normalize_user_facing_path(&fs::canonicalize(&worktree).unwrap());
        let sibling = watched.join("targetAb1234");
        fs::create_dir(&sibling).unwrap();
        turn.state.note_engram_workspace_file_changes(&[change(
            &sibling,
            WorkspaceFileChangeKind::Created,
        )]);
        // The name alone never grants credit: the destination is judged at
        // terminal settlement even if the watcher initially keeps it pending.
        fs::remove_dir(&sibling).unwrap();
        finish_run(&worktree, "passed", &"f".repeat(64));
        turn.state.poll_engram_carried_runs();
        let checkpoint = turn.finish();
        assert_refused(&turn, &checkpoint, "targetAb1234");
    }
}

#[test]
fn cargo_target_sibling_pending_overflow_fences_even_if_target_appears_later() {
    let (turn, worktree) = named_turn("cargo-target-pending-overflow");
    fs::write(worktree.join(".gitignore"), "target/\n").unwrap();
    launch_gate(&turn, &worktree, false);
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
    let checkpoint = turn.finish();
    assert_refused(&turn, &checkpoint, "pending Cargo");
}
