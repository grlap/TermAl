//! Opt-in, synthetic allocation comparison. No watcher, files, directories or
//! live AppState is created. The legacy model mirrors pre-trie queue/conversion;
//! it is NOT execution of an unchanged historical production binary.
use super::*;

fn memory() -> Value {
    let status = fs::read_to_string("/proc/self/status").expect("Linux counters");
    let fields = ["VmRSS:", "VmHWM:", "VmSize:", "VmData:"];
    Value::Object(
        fields
            .into_iter()
            .map(|field| {
                let kib: u64 = status
                    .lines()
                    .find(|line| line.starts_with(field))
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .parse()
                    .unwrap();
                (field.trim_end_matches(':').to_owned(), json!(kib * 1024))
            })
            .collect(),
    )
}

fn event(n: usize) -> NotifyEvent {
    NotifyEvent::new(NotifyEventKind::Create(notify::event::CreateKind::File)).add_path(
        PathBuf::from("/__termal_synthetic_watch_probe__/project/.tmp/large-fixture/source")
            .join(format!("synthetic-file-{n}.rs")),
    )
}

fn require_opt_in() {
    assert_eq!(std::env::consts::OS, "linux");
    assert_eq!(
        std::env::var("TERMAL_SYNTHETIC_WATCHER_DIAGNOSTIC").as_deref(),
        Ok("1")
    );
}

#[test]
#[ignore = "opt-in Linux synthetic memory diagnostic, no filesystem event burst"]
fn legacy_scope_fanout_memory_model() {
    require_opt_in();
    let scopes = (0..64)
        .map(|n| WorkspaceFileWatchScope {
            root_path: PathBuf::from("/__termal_synthetic_watch_probe__/project"),
            session_id: Some(format!("synthetic-session-{n}")),
        })
        .collect::<Vec<_>>();
    let (tx, rx) = mpsc::channel();
    let before = memory();
    for n in 0..4000 {
        let event = event(n);
        tx.send((
            EngramHost::watcher_arrival_now(),
            WorkspaceWatchObservation::capture(&event),
            event,
        ))
        .unwrap();
    }
    drop(tx);
    let held = memory();
    let mut pending = BTreeMap::<String, WorkspaceFileChangeEvent>::new();
    let mut drained = 0;
    for (_, _, event) in rx {
        drained += 1;
        for change in workspace_file_changes_from_notify_event(&event, &scopes) {
            let key = workspace_file_change_event_key(&change);
            pending
                .entry(key)
                .and_modify(|current| {
                    current.kind = merge_workspace_file_change_kind(current.kind, change.kind);
                })
                .or_insert(change);
        }
    }
    assert_eq!(drained, 4000);
    assert_eq!(pending.len(), 256000);
    let accumulated = memory();
    let owned_strings: usize = pending
        .iter()
        .map(|(key, change)| {
            key.capacity()
                + change.path.capacity()
                + change.root_path.as_ref().map_or(0, String::capacity)
                + change.session_id.as_ref().map_or(0, String::capacity)
        })
        .sum();
    std::hint::black_box(&pending);
    drop(pending);
    eprintln!(
        "WORKSPACE_MEMORY {}",
        json!({
            "mode":"legacy_model", "paths":4000, "matching_scopes":64, "pending_entries":256000,
            "owned_string_capacity_lower_bound":owned_strings, "before":before, "held":held,
            "accumulated":accumulated, "after_drop":memory(), "native_watchers":0,
            "limit":"Test-owned legacy loop with current shared converter, not a historical binary or whole-host attribution"
        })
    );
}

#[test]
#[ignore = "opt-in Linux synthetic memory diagnostic, no filesystem event burst"]
fn shared_trie_memory_effect() {
    require_opt_in();
    let before = memory();
    let mut pending = WorkspacePendingChanges::default();
    let now = std::time::Instant::now();
    for n in 0..4000 {
        pending.ingest(Ok(event(n)), EngramHost::watcher_arrival_now(), now);
    }
    let held = memory();
    let (nodes, charged_bytes) = pending.root.usage();
    assert!(nodes <= WORKSPACE_PENDING_NODES);
    assert!(charged_bytes + pending.root.delivery_bytes(0) <= WORKSPACE_PENDING_BYTES);
    let changes = pending.take();
    for n in 0..4000 {
        let path = event(n).paths.pop().unwrap();
        assert!(changes.iter().any(|(cover, fact)| cover == &path
            || fact.kind == WorkspaceFileChangeKind::Subtree && path.starts_with(cover)));
    }
    assert!(changes.len() < 4000);
    let entries = changes.len();
    let accumulated = memory();
    std::hint::black_box(&changes);
    drop(changes);
    eprintln!(
        "WORKSPACE_MEMORY {}",
        json!({
            "mode":"shared_trie", "paths":4000, "matching_scopes":64, "pending_entries":entries,
            "nodes":nodes, "charged_bytes":charged_bytes, "before":before, "held":held,
            "accumulated":accumulated, "after_drop":memory(), "native_watchers":0,
            "limit":"Current ingress, scope-independent storage; no OS backend, real host state or incident-causality claim"
        })
    );
}
