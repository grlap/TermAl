// Raw watcher notifications through the carried-run checkpoint. Equal endpoint
// contents cannot excuse a real leaf write, including a write then restore.
use super::*;
use notify::event::{CreateKind, DataChange, MetadataKind, ModifyKind, RemoveKind, RenameMode};

fn instruction_fixture(label: &str) -> (CheckedTurn, PathBuf, PathBuf) {
    let (turn, worktree) = named_turn(label);
    let directory = worktree.join(".claude");
    fs::create_dir_all(directory.join("commands")).unwrap();
    fs::write(directory.join("settings.json"), "{}\n").unwrap();
    fs::write(
        directory.join("commands").join("review.md"),
        "Review source.\n",
    )
    .unwrap();
    run_git_test_command_output(&worktree, &["add", "--", ".claude"]);
    let directory = fs::canonicalize(directory).unwrap();
    (turn, worktree, directory)
}

fn deliver_raw(
    turn: &CheckedTurn,
    path: &FsPath,
    kind: NotifyEventKind,
) -> Vec<WorkspaceFileChangeEvent> {
    let event = NotifyEvent::new(kind).add_path(path.to_path_buf());
    let changes = workspace_file_changes_from_notify_event(&event, &[]);
    turn.state.record_active_turn_file_changes(&changes);
    turn.state.engram_host().workspace_files_changed(&changes);
    changes
}

#[test]
fn watcher_metadata_directory_hint_keeps_unchanged_gate_credit() {
    for (label, kind) in [
        ("directory-any", ModifyKind::Any),
        (
            "directory-time",
            ModifyKind::Metadata(MetadataKind::WriteTime),
        ),
        (
            "directory-permissions",
            ModifyKind::Metadata(MetadataKind::Permissions),
        ),
    ] {
        let (turn, worktree, directory) = instruction_fixture(label);
        launch_gate(&turn, &worktree, false);
        deliver_raw(&turn, &directory, NotifyEventKind::Modify(kind));
        let run = finish_run(&worktree, "passed", &"f".repeat(64));
        let checkpoint = turn.finish();
        assert_credited(&turn, &checkpoint, &run, "succeeded");
    }
}

#[test]
fn watcher_metadata_real_child_write_then_restore_still_refuses_credit() {
    for relative in ["settings.json", "commands/review.md"] {
        let (turn, worktree, directory) = instruction_fixture("directory-child-write");
        let path = directory.join(relative);
        let original = fs::read(&path).unwrap();
        launch_gate(&turn, &worktree, false);
        fs::write(&path, "changed\n").unwrap();
        let changes = deliver_raw(&turn, &path, NotifyEventKind::Modify(ModifyKind::Any));
        assert_eq!(changes.len(), 1, "the real child event remains observable");
        fs::write(&path, &original).unwrap();
        finish_run(&worktree, "passed", &"f".repeat(64));
        let checkpoint = turn.finish();
        assert_refused(&turn, &checkpoint, "a file in its worktree changed");
    }
}

#[test]
fn watcher_metadata_namespace_events_on_existing_directory_still_refuse_credit() {
    for kind in [
        NotifyEventKind::Create(CreateKind::Folder),
        NotifyEventKind::Remove(RemoveKind::Folder),
        NotifyEventKind::Modify(ModifyKind::Name(RenameMode::To)),
        NotifyEventKind::Modify(ModifyKind::Name(RenameMode::From)),
        NotifyEventKind::Modify(ModifyKind::Data(DataChange::Any)),
        NotifyEventKind::Any,
        NotifyEventKind::Other,
    ] {
        let (turn, worktree, directory) = instruction_fixture("directory-namespace");
        launch_gate(&turn, &worktree, false);
        // A delayed delete/rename may be handled after a replacement exists.
        // Its current directory type must not erase the event's operation.
        let changes = deliver_raw(&turn, &directory, kind);
        assert_eq!(changes.len(), 1);
        finish_run(&worktree, "passed", &"f".repeat(64));
        let checkpoint = turn.finish();
        assert_refused(&turn, &checkpoint, "a file in its worktree changed");
    }
}

#[test]
fn watcher_metadata_unknown_or_leaf_paths_still_refuse_credit() {
    for (relative, kind) in [
        ("gone", ModifyKind::Any),
        (
            "settings.json",
            ModifyKind::Metadata(MetadataKind::WriteTime),
        ),
        ("commands/review.md", ModifyKind::Data(DataChange::Any)),
    ] {
        let (turn, worktree, directory) = instruction_fixture("directory-unknown");
        launch_gate(&turn, &worktree, false);
        let changes = deliver_raw(
            &turn,
            &directory.join(relative),
            NotifyEventKind::Modify(kind),
        );
        assert_eq!(changes.len(), 1);
        finish_run(&worktree, "passed", &"f".repeat(64));
        let checkpoint = turn.finish();
        assert_refused(&turn, &checkpoint, "a file in its worktree changed");
    }
}

fn directory_link(link: &FsPath, target: &FsPath) {
    #[cfg(windows)]
    {
        let output = Command::new("cmd")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .expect("create the fixture junction without symlink privilege");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[test]
fn watcher_metadata_link_or_link_ancestor_keeps_the_fence() {
    for child in [false, true] {
        let (turn, worktree, directory) = instruction_fixture("directory-link");
        let target = worktree.join("source-target");
        fs::create_dir_all(target.join("child")).unwrap();
        let link = directory.join("linked");
        directory_link(&link, &target);
        launch_gate(&turn, &worktree, false);
        let path = if child {
            link.join("child")
        } else {
            link.clone()
        };
        let changes = deliver_raw(&turn, &path, NotifyEventKind::Modify(ModifyKind::Any));
        assert_eq!(
            changes.len(),
            1,
            "neither a link nor its target child is exempt"
        );
        finish_run(&worktree, "passed", &"f".repeat(64));
        let checkpoint = turn.finish();
        assert_refused(&turn, &checkpoint, "a file in its worktree changed");
        // Remove only the link itself, never recurse into its target.
        #[cfg(windows)]
        fs::remove_dir(&link).unwrap();
        #[cfg(unix)]
        fs::remove_file(&link).unwrap();
    }
}

#[test]
fn watcher_metadata_replaced_directory_keeps_its_namespace_fence() {
    let (turn, worktree, directory) = instruction_fixture("directory-replaced");
    launch_gate(&turn, &worktree, false);
    let saved = worktree.join("saved-instructions");
    fs::rename(&directory, &saved).unwrap();
    fs::create_dir(&directory).unwrap();
    let changes = deliver_raw(
        &turn,
        &directory,
        NotifyEventKind::Remove(RemoveKind::Folder),
    );
    assert_eq!(
        changes.len(),
        1,
        "a late removal cannot be mistaken for metadata"
    );
    deliver_raw(
        &turn,
        &directory,
        NotifyEventKind::Create(CreateKind::Folder),
    );
    finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();
    assert_refused(&turn, &checkpoint, "a file in its worktree changed");
}

#[test]
fn watcher_metadata_child_create_delete_or_rename_remains_observable() {
    for action in ["create", "delete", "rename"] {
        let (turn, worktree, directory) = instruction_fixture("directory-child-namespace");
        let path = directory.join("settings.json");
        let original = fs::read(&path).unwrap();
        launch_gate(&turn, &worktree, false);
        let event = match action {
            "create" => {
                let created = directory.join("commands/new.md");
                fs::write(&created, "new instruction\n").unwrap();
                NotifyEvent::new(NotifyEventKind::Create(CreateKind::File)).add_path(created)
            }
            "delete" => {
                fs::remove_file(&path).unwrap();
                NotifyEvent::new(NotifyEventKind::Remove(RemoveKind::File)).add_path(path.clone())
            }
            "rename" => {
                let moved = directory.join("renamed.json");
                fs::rename(&path, &moved).unwrap();
                NotifyEvent::new(NotifyEventKind::Modify(ModifyKind::Name(RenameMode::Both)))
                    .add_path(path.clone())
                    .add_path(moved)
            }
            _ => unreachable!(),
        };
        let changes = workspace_file_changes_from_notify_event(&event, &[]);
        assert_eq!(changes.len(), event.paths.len());
        turn.state.record_active_turn_file_changes(&changes);
        turn.state.engram_host().workspace_files_changed(&changes);
        // Equal final file contents must not cancel an already witnessed write.
        match action {
            "create" => fs::remove_file(directory.join("commands/new.md")).unwrap(),
            "delete" => fs::write(&path, &original).unwrap(),
            "rename" => fs::rename(directory.join("renamed.json"), &path).unwrap(),
            _ => unreachable!(),
        }
        finish_run(&worktree, "passed", &"f".repeat(64));
        let checkpoint = turn.finish();
        assert_refused(&turn, &checkpoint, "a file in its worktree changed");
    }
}

#[test]
fn watcher_metadata_mixed_batch_keeps_the_file_event() {
    let (turn, worktree, directory) = instruction_fixture("directory-mixed-batch");
    launch_gate(&turn, &worktree, false);
    let path = directory.join("settings.json");
    let event = NotifyEvent::new(NotifyEventKind::Modify(ModifyKind::Any))
        .add_path(directory)
        .add_path(path.clone());
    let changes = workspace_file_changes_from_notify_event(&event, &[]);
    assert_eq!(changes.len(), 1);
    assert_eq!(
        changes[0].path,
        normalize_user_facing_path(&path)
            .to_string_lossy()
            .into_owned()
    );
    turn.state.engram_host().workspace_files_changed(&changes);
    finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();
    assert_refused(&turn, &checkpoint, "a file in its worktree changed");
}
