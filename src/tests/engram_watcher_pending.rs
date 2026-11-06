// Production ingress, coalescing and fence routing, including paths which
// have disappeared before delivery. No native filesystem watcher is started.
use super::*;
use notify::event::{CreateKind, ModifyKind, RemoveKind, RenameMode};

fn deliver(turn: &EOCheckedTurn, event: NotifyEvent) -> Vec<WorkspaceFileChangeEvent> {
    let mut pending = WorkspacePendingChanges::default();
    pending.ingest(
        Ok(event),
        EngramHost::watcher_arrival_now(),
        std::time::Instant::now(),
    );
    turn.state.process_workspace_dirty_batch(pending.take())
}

#[test]
fn watcher_pending_windows_delete_and_rename_in_own_scratch_do_not_fence() {
    let (turn, worktree) = ordinary_carried_turn("pending-own-scratch");
    ordinary_launch_gate(&turn, &worktree, false);
    let scratch = worktree.join(".tmp");
    fs::create_dir_all(&scratch).unwrap();
    let gone = scratch.join("gone.txt");
    fs::write(&gone, "fixture").unwrap();
    fs::remove_file(&gone).unwrap();
    assert_eq!(
        deliver(
            &turn,
            NotifyEvent::new(NotifyEventKind::Remove(RemoveKind::Any)).add_path(gone)
        )
        .len(),
        1
    );
    deliver(
        &turn,
        NotifyEvent::new(NotifyEventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path(scratch.join("old"))
            .add_path(scratch.join("new")),
    );
    assert!(turn.record(|r| r.engram.carried_checks[0].fence.is_none()));
    // The same production path must still fence a real source deletion.
    deliver(
        &turn,
        NotifyEvent::new(NotifyEventKind::Remove(RemoveKind::Any))
            .add_path(worktree.join("README.md")),
    );
    assert!(turn.record(|r| r.engram.carried_checks[0].fence.is_some()));
}

#[test]
fn watcher_pending_cargo_folder_create_and_rename_remain_candidates() {
    let (turn, worktree) = ordinary_carried_turn("pending-cargo-sibling");
    fs::write(worktree.join(".gitignore"), "target/\n").unwrap();
    ordinary_launch_gate(&turn, &worktree, false);
    let sibling = worktree.join("targetVsXPcL");
    fs::create_dir(&sibling).unwrap();
    deliver(
        &turn,
        NotifyEvent::new(NotifyEventKind::Create(CreateKind::Folder)).add_path(sibling.clone()),
    );
    assert!(turn.record(|r| r.engram.carried_checks[0].fence.is_none()));
    assert!(turn.record(|r| {
        !r.engram.carried_checks[0]
            .check
            .watcher_cargo_candidates
            .is_empty()
    }));
    fs::write(sibling.join("CACHEDIR.TAG"), "cargo cache\n").unwrap();
    fs::rename(&sibling, worktree.join("target")).unwrap();
    deliver(
        &turn,
        NotifyEvent::new(NotifyEventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path(sibling)
            .add_path(worktree.join("target")),
    );
    assert!(turn.record(|r| r.engram.carried_checks[0].fence.is_none()));
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    assert_carried_artifact_custody(&turn, &run, true);
}

#[test]
fn watcher_pending_nested_repository_marker_routes_to_nested_check_only() {
    let (turn, worktree) = ordinary_carried_turn("pending-nested-route");
    ordinary_launch_gate(&turn, &worktree, false);
    let nested = worktree.join(".tmp/nested");
    fs::create_dir_all(&nested).unwrap();
    run_git_test_command_output(&nested, &["init"]);
    // Two root targets share the observation owner; only routing is judged,
    // not terminal artifact custody for this synthetic second check.
    turn.record_mut(|r| {
        let mut nested_check = r.engram.carried_checks[0].clone();
        nested_check.check.target.root = fs::canonicalize(&nested).unwrap();
        r.engram.carried_checks.push(nested_check);
    });
    deliver(
        &turn,
        NotifyEvent::new(NotifyEventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path(nested.join("old"))
            .add_path(nested.join("new")),
    );
    turn.record(|r| {
        assert!(r.engram.carried_checks[0].fence.is_none());
        assert!(r.engram.carried_checks[1].fence.is_some());
    });
}

#[test]
fn watcher_pending_local_budget_collapse_does_not_fence_another_worktree() {
    let (turn, worktree) = ordinary_carried_turn("pending-other-root");
    ordinary_launch_gate(&turn, &worktree, false);
    let other = turn.root.join("other-repository");
    fs::create_dir_all(&other).unwrap();
    run_git_test_command_output(&other, &["init"]);
    let mut pending = WorkspacePendingChanges::default();
    for n in 0..6000 {
        let path = other.join(format!("src/d{}/f{}", n / 50, n % 50));
        pending.ingest(
            Ok(NotifyEvent::new(NotifyEventKind::Modify(ModifyKind::Any)).add_path(path)),
            EngramHost::watcher_arrival_now(),
            std::time::Instant::now(),
        );
    }
    let batch = pending.take();
    assert!(!batch.is_empty());
    assert!(
        batch
            .iter()
            .any(|(_, fact)| fact.kind == WorkspaceFileChangeKind::Subtree)
    );
    assert!(batch.iter().all(|(path, _)| path.starts_with(&other)));
    turn.state.process_workspace_dirty_batch(batch);
    assert!(turn.record(|r| r.engram.carried_checks[0].fence.is_none()));
}
