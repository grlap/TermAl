// Source-file watcher refusals must retain diagnostic facts without treating
// unchanged endpoint bytes as proof that no write happened.
use super::*;

fn source_change(path: &FsPath) -> WorkspaceFileChangeEvent {
    WorkspaceFileChangeEvent {
        path: path.to_string_lossy().into_owned(),
        kind: WorkspaceFileChangeKind::Modified,
        root_path: None,
        session_id: None,
        mtime_ms: None,
        size_bytes: None,
    }
}

#[test]
fn watcher_provenance_source_refusal_retains_available_and_unknown_facts() {
    take_workspace_watch_diagnostics();
    let (turn, worktree) = named_turn("watcher-source-provenance");
    let path = fs::canonicalize(&worktree).unwrap().join("README.md");
    let before = fs::read(&path).unwrap();
    launch_gate(&turn, &worktree, false);

    // This ingress has no raw Notify event. Its absence must be stated,
    // rather than inventing a notification subtype, timing or writer PID.
    turn.state
        .note_engram_workspace_file_changes(&[source_change(&path)]);
    assert_eq!(fs::read(&path).unwrap(), before);
    let fence = turn
        .record(|record| record.engram.carried_checks[0].fence.clone())
        .expect("the uncertain source event must still fence the run");
    // No arrival and no modification time: the cause says both are unknown.
    assert_eq!(
        fence,
        format!(
            "a file in its worktree changed: {}; modification time unknown",
            path.display()
        )
    );
    let diagnostic = turn
        .record(|record| {
            record.engram.carried_checks[0]
                .check
                .watcher_provenance
                .clone()
        })
        .expect("the first refusal retains separate diagnostic facts");
    assert_eq!(take_workspace_watch_diagnostics(), vec![diagnostic.clone()]);
    finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();
    assert_refused(&turn, &checkpoint, "a file in its worktree changed");
    let fence = diagnostic;
    assert!(fence.contains("watcher provenance:"), "{fence}");
    for fact in [
        "raw_kind=unknown",
        "observed=unknown",
        "writer_pid=unknown",
        "routed_root=",
        "check_key_sha256=",
        "check_sequence=",
    ] {
        assert!(fence.contains(fact), "missing {fact}: {fence}");
    }
}

#[test]
fn watcher_provenance_late_write_then_restore_still_refuses_credit() {
    let (turn, worktree) = named_turn("watcher-source-restored");
    let path = fs::canonicalize(&worktree).unwrap().join("README.md");
    let before = fs::read(&path).unwrap();
    launch_gate(&turn, &worktree, false);
    fs::write(&path, b"transient source edit").unwrap();
    fs::write(&path, &before).unwrap();
    assert_eq!(fs::read(&path).unwrap(), before);

    finish_run(&worktree, "passed", &"f".repeat(64));
    turn.state.poll_engram_carried_runs();
    // A delayed notification can describe a real write during the run.
    // Matching snapshots and an already-terminal run must not waive it.
    turn.state
        .note_engram_workspace_file_changes(&[source_change(&path)]);
    let checkpoint = turn.finish();
    assert_refused(&turn, &checkpoint, "a file in its worktree changed");
}

#[test]
fn watcher_provenance_raw_metadata_survives_queueing_conversion_and_settlement() {
    use notify::event::{MetadataKind, ModifyKind};
    take_workspace_watch_diagnostics();
    let (turn, worktree) = named_turn("watcher-source-raw");
    let root = fs::canonicalize(&worktree).unwrap();
    let path = root.join("README.md");
    let before = fs::read(&path).unwrap();
    let mut event = notify::Event::new(notify::EventKind::Modify(ModifyKind::Metadata(
        MetadataKind::WriteTime,
    )))
    .add_path(path.clone())
    .set_tracker(73)
    .set_process_id(991);
    let private_info = "private backend detail".repeat(10_000);
    event.attrs.set_info(&private_info);
    // Receipt can precede check registration while handling is delayed.
    // The observation's receipt clock is diagnostic ordering only, not a
    // credit exemption. The Engram arrival given here follows the launch, so
    // the change fences; an arrival before a launch is judged by its own
    // order (src/tests/engram_watcher_change_time.rs).
    let observation = WorkspaceWatchObservation::capture(&event);
    launch_gate(&turn, &worktree, false);
    let changes = turn.state.process_workspace_watch_event(
        &event,
        &observation,
        EngramHost::watcher_arrival_now(),
        &[],
    );
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].kind, WorkspaceFileChangeKind::Modified);
    let delivered_path = normalize_user_facing_path(&path);
    assert_eq!(changes[0].path, delivered_path.to_string_lossy());
    assert_eq!(fs::read(&path).unwrap(), before);
    let legacy_fence = turn
        .record(|record| record.engram.carried_checks[0].fence.clone())
        .unwrap();
    // The converted event carries the file's modification time and the
    // notification's arrival, both named in the cause.
    assert!(
        legacy_fence.starts_with(&format!(
            "a file in its worktree changed: {}; modified at ",
            delivered_path.display()
        )) && legacy_fence.contains(", seen at "),
        "{legacy_fence}"
    );
    let fence = turn
        .record(|record| {
            record.engram.carried_checks[0]
                .check
                .watcher_provenance
                .clone()
        })
        .unwrap();
    for fact in [
        "raw_kind=Modify(Metadata(WriteTime))".to_owned(),
        "tracker=73".to_owned(),
        "writer_pid=991".to_owned(),
        "info_present=true".to_owned(),
        format!("observed={}", observation.observed),
        format!(
            "routed_root_sha256={}",
            sha256_hex(engram_path_key(&root).as_bytes())
        ),
        format!("check_key_sha256={}", sha256_hex(b"gate")),
        format!("grant_sha256={}", sha256_hex(CHECK_GRANT.as_bytes())),
    ] {
        assert!(fence.contains(&fact), "missing {fact}: {fence}");
    }
    let clock = |name: &str| {
        fence
            .split_whitespace()
            .find_map(|part| {
                part.strip_prefix(name)?
                    .trim_end_matches(']')
                    .parse::<u64>()
                    .ok()
            })
            .unwrap()
    };
    assert!(observation.observed < clock("processed="));
    assert!(clock("processed=") < clock("converted="));
    assert!(clock("converted=") < clock("delivered="));
    assert!(clock("delivered=") < clock("fenced="));
    assert!(!fence.contains("private backend detail"));
    assert!(fence.len() < 900);
    assert!(!fence.contains(CHECK_GRANT));

    // A second notification cannot replace the first refusal or grow its
    // history, even when the later notification carries other attributes.
    let another = notify::Event::new(notify::EventKind::Other).add_path(path);
    turn.state.process_workspace_watch_event(
        &another,
        &WorkspaceWatchObservation::capture(&another),
        EngramHost::watcher_arrival_now(),
        &[],
    );
    assert_eq!(
        turn.record(|record| record.engram.carried_checks[0].fence.clone()),
        Some(legacy_fence)
    );
    assert_eq!(
        turn.record(|record| record.engram.carried_checks[0]
            .check
            .watcher_provenance
            .clone()),
        Some(fence.clone())
    );
    assert_eq!(take_workspace_watch_diagnostics(), vec![fence]);
    let check = turn.record(|record| record.engram.carried_checks[0].check.clone());
    let bounded =
        engram_watcher_fence_diagnostic(&"private".repeat(2000), &"x".repeat(5000), &check);
    assert!(bounded.len() <= WORKSPACE_WATCH_PROVENANCE_LIMIT);
    assert!(!bounded.contains("private"));
    assert!(bounded.contains(&format!("check_key_sha256={}", sha256_hex(b"gate"))));
    finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();
    assert_refused(&turn, &checkpoint, "a file in its worktree changed");
}

#[test]
fn watcher_provenance_real_source_edits_replacements_and_renames_remain_fenced() {
    use notify::event::{DataChange, ModifyKind, RenameMode};
    for (label, kind) in [
        (
            "data",
            notify::EventKind::Modify(ModifyKind::Data(DataChange::Content)),
        ),
        (
            "replace",
            notify::EventKind::Create(notify::event::CreateKind::File),
        ),
        (
            "rename",
            notify::EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
        ),
    ] {
        let (turn, worktree) = named_turn(&format!("watcher-source-{label}"));
        let path = fs::canonicalize(&worktree).unwrap().join("README.md");
        let bytes = fs::read(&path).unwrap();
        launch_gate(&turn, &worktree, false);
        let mut event = notify::Event::new(kind).add_path(path.clone());
        if label == "replace" {
            fs::remove_file(&path).unwrap();
            fs::write(&path, &bytes).unwrap();
        } else if label == "rename" {
            let renamed = path.with_file_name("renamed.md");
            fs::rename(&path, &renamed).unwrap();
            fs::rename(&renamed, &path).unwrap();
            event.paths.push(renamed);
        } else {
            fs::write(&path, b"changed source").unwrap();
            fs::write(&path, &bytes).unwrap();
        }
        turn.state.process_workspace_watch_event(
            &event,
            &WorkspaceWatchObservation::capture(&event),
            EngramHost::watcher_arrival_now(),
            &[],
        );
        finish_run(&worktree, "passed", &"f".repeat(64));
        let checkpoint = turn.finish();
        assert_refused(&turn, &checkpoint, "a file in its worktree changed");
    }
}

#[test]
fn watcher_provenance_other_worktree_and_access_events_do_not_gain_a_fence() {
    let (turn, worktree) = named_turn("watcher-source-routing");
    launch_gate(&turn, &worktree, false);
    let elsewhere = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
        .add_path(turn.root.join("README.md"));
    turn.state.process_workspace_watch_event(
        &elsewhere,
        &WorkspaceWatchObservation::capture(&elsewhere),
        EngramHost::watcher_arrival_now(),
        &[],
    );
    let access = notify::Event::new(notify::EventKind::Access(notify::event::AccessKind::Any))
        .add_path(worktree.join("README.md"));
    assert!(
        turn.state
            .process_workspace_watch_event(
                &access,
                &WorkspaceWatchObservation::capture(&access),
                EngramHost::watcher_arrival_now(),
                &[]
            )
            .is_empty()
    );
    assert!(turn.record(|record| record.engram.carried_checks[0].fence.is_none()));
    let run = finish_run(&worktree, "passed", &"f".repeat(64));
    let checkpoint = turn.finish();
    assert_credited(&turn, &checkpoint, &run, "succeeded");
}

#[test]
fn watcher_provenance_queue_duration_excludes_conversion_and_attribution_work() {
    let event = notify::Event::new(notify::EventKind::Other);
    let mut observation = WorkspaceWatchObservation::capture(&event);
    observation.received_at -= Duration::from_secs(1);
    // Simulate handling seven microseconds after receipt, followed by slow
    // conversion. Formatting later must not charge that work to queue time.
    let handling_started = observation.received_at + Duration::from_micros(7);
    let facts = observation.processing(engram_interference_tick(), handling_started);
    assert!(
        facts.split_whitespace().any(|field| field == "queue_us=7"),
        "{facts}"
    );
}
