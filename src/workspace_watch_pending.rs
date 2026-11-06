// Callback-owned dirty paths. One component trie is shared by every scope;
// overflow widens a notification instead of discarding it. No filesystem I/O
// or session lookup runs while this structure is locked.
const WORKSPACE_PENDING_NODES: usize = 4096;
const WORKSPACE_PENDING_BYTES: usize = 256 * 1024;
const WORKSPACE_PENDING_CHILDREN: usize = 64;
const WORKSPACE_PENDING_DEPTH: usize = 64;
const WORKSPACE_PENDING_PATH_BYTES: usize = 16 * 1024;

// Lexical overlap only; never resolves filesystem paths under StateInner.
fn workspace_subtree_contains(path: &str, root: &FsPath) -> bool {
    if path.is_empty() {
        return true;
    }
    let key = |path: &FsPath| {
        let text = normalize_user_facing_path(path)
            .to_string_lossy()
            .replace('\\', "/");
        #[cfg(any(windows, target_os = "macos"))]
        let text = text.to_lowercase();
        text.trim_end_matches('/').to_owned()
    };
    let path = key(FsPath::new(path));
    let root = key(root);
    path == root || root.starts_with(&(path + "/"))
}

#[derive(Clone, Copy)]
struct WorkspaceDirtyFact {
    kind: WorkspaceFileChangeKind,
    first: EngramWatcherArrival,
    last: EngramWatcherArrival,
    raw_kind: Option<NotifyEventKind>,
    observation: Option<WorkspaceWatchObservation>,
}

impl WorkspaceDirtyFact {
    fn merge(&mut self, other: Self) {
        self.kind = merge_workspace_file_change_kind(self.kind, other.kind);
        if self.raw_kind != other.raw_kind {
            self.raw_kind = None;
        }
        if other.first.order < self.first.order {
            self.first = other.first;
            self.observation = other.observation;
        }
        if other.last.order > self.last.order {
            self.last = other.last;
        }
    }
}

struct WorkspaceDirtyNode {
    children: BTreeMap<std::ffi::OsString, WorkspaceDirtyNode>,
    // Summary includes descendants, even after they have been collapsed.
    summary: Option<WorkspaceDirtyFact>,
    // A path's own fact never includes its children's facts. Only an actual
    // collapse emits the aggregate, otherwise metadata filtering needs this.
    terminal: Option<WorkspaceDirtyFact>,
    subtree: bool,
    nodes: usize,
    bytes: usize,
    delivery: usize,
}

impl Default for WorkspaceDirtyNode {
    fn default() -> Self {
        Self {
            children: BTreeMap::new(),
            summary: None,
            terminal: None,
            subtree: false,
            nodes: 1,
            bytes: std::mem::size_of::<Self>(),
            delivery: 0,
        }
    }
}

impl WorkspaceDirtyNode {
    // Child totals are cached: insertion only revisits the path and at most
    // CHILDREN siblings per level, never recursively scans the whole trie.
    fn refresh(&mut self, prefix: usize) {
        self.nodes = 1;
        self.bytes = std::mem::size_of::<Self>();
        self.delivery = if self.terminal.is_some() {
            prefix + std::mem::size_of::<WorkspaceDirtyFact>()
        } else {
            0
        };
        for (key, child) in &self.children {
            self.nodes += child.nodes;
            self.bytes += child.bytes + key.capacity() * std::mem::size_of::<u16>() + 128;
            self.delivery += child.delivery;
        }
    }

    fn collapse(&mut self, prefix: usize) {
        self.children.clear();
        self.subtree = true;
        if let Some(fact) = &mut self.summary {
            fact.kind = WorkspaceFileChangeKind::Subtree;
        }
        self.terminal = self.summary;
        self.refresh(prefix);
    }

    fn insert(&mut self, parts: &[std::ffi::OsString], fact: WorkspaceDirtyFact, prefix: usize) {
        match &mut self.summary {
            Some(summary) => summary.merge(fact),
            None => self.summary = Some(fact),
        }
        if self.subtree {
            self.terminal = self.summary.map(|mut fact| {
                fact.kind = WorkspaceFileChangeKind::Subtree;
                fact
            });
            return;
        }
        let Some((head, tail)) = parts.split_first() else {
            match &mut self.terminal {
                Some(own) => own.merge(fact),
                None => self.terminal = Some(fact),
            }
            if fact.kind == WorkspaceFileChangeKind::Subtree {
                self.collapse(prefix);
            }
            self.refresh(prefix);
            return;
        };
        if !self.children.contains_key(head) && self.children.len() >= WORKSPACE_PENDING_CHILDREN {
            self.collapse(prefix);
            return;
        }
        self.children.entry(head.clone()).or_default().insert(
            tail,
            fact,
            prefix + head.len() * 2 + 1,
        );
        self.refresh(prefix);
    }

    fn usage(&self) -> (usize, usize) {
        (self.nodes, self.bytes)
    }

    fn delivery_bytes(&self, _prefix: usize) -> usize {
        self.delivery
    }

    // Overflow follows only the inserted path. Prefer its deepest ancestor
    // that pays the deficit, rather than widening an unrelated deeper path.
    fn collapse_candidate(
        &self,
        parts: &[std::ffi::OsString],
        need_nodes: usize,
        need_bytes: usize,
        prefix: usize,
    ) -> Option<usize> {
        if let Some((head, tail)) = parts.split_first() {
            if let Some(depth) = self.children.get(head).and_then(|child| {
                child.collapse_candidate(
                    tail,
                    need_nodes,
                    need_bytes,
                    prefix + head.len() * 2 + 1,
                )
            }) {
                return Some(depth + 1);
            }
        }
        let saved_bytes = (self.bytes + self.delivery).saturating_sub(
            std::mem::size_of::<Self>() + prefix + std::mem::size_of::<WorkspaceDirtyFact>(),
        );
        (!self.children.is_empty()
            && self.nodes - 1 >= need_nodes
            && saved_bytes >= need_bytes)
            .then_some(0)
    }

    fn collapse_at(&mut self, parts: &[std::ffi::OsString], prefix: usize) {
        if let Some((head, tail)) = parts.split_first() {
            self.children
                .get_mut(head)
                .expect("selected collapse node")
                .collapse_at(tail, prefix + head.len() * 2 + 1);
            self.refresh(prefix);
        } else {
            self.collapse(prefix);
        }
    }

    fn drain(self, path: &mut PathBuf, out: &mut Vec<(PathBuf, WorkspaceDirtyFact)>) {
        if let Some(fact) = self.terminal {
            out.push((path.clone(), fact));
        }
        for (part, child) in self.children {
            let previous = path.clone();
            path.push(part);
            child.drain(path, out);
            *path = previous;
        }
    }
}

#[derive(Default)]
struct WorkspacePendingChanges {
    root: WorkspaceDirtyNode,
    oldest: Option<std::time::Instant>,
    newest: Option<std::time::Instant>,
    last_error_log: Option<std::time::Instant>,
}

impl WorkspacePendingChanges {
    fn ingest(
        &mut self,
        result: notify::Result<NotifyEvent>,
        arrival: EngramWatcherArrival,
        now: std::time::Instant,
    ) {
        let observation = result.as_ref().ok().map(WorkspaceWatchObservation::capture);
        self.ingest_observed(result, arrival, now, observation);
    }

    fn ingest_observed(
        &mut self,
        result: notify::Result<NotifyEvent>,
        arrival: EngramWatcherArrival,
        now: std::time::Instant,
        observation: Option<WorkspaceWatchObservation>,
    ) {
        match result {
            Ok(event) if !event.need_rescan() => {
                if let Some(mut kind) = workspace_file_change_kind(&event.kind) {
                    if matches!(
                        event.kind,
                        NotifyEventKind::Create(notify::event::CreateKind::Folder)
                            | NotifyEventKind::Remove(
                                notify::event::RemoveKind::Folder | notify::event::RemoveKind::Any
                            )
                            | NotifyEventKind::Modify(notify::event::ModifyKind::Name(_))
                    ) {
                        kind = WorkspaceFileChangeKind::Subtree;
                    }
                    for path in &event.paths {
                        if path.as_os_str().len() > WORKSPACE_PENDING_PATH_BYTES
                            || !is_ignored_workspace_file_event_path(path)
                        {
                            self.insert(
                                path,
                                WorkspaceDirtyFact {
                                    kind,
                                    first: arrival,
                                    last: arrival,
                                    raw_kind: Some(event.kind),
                                    observation,
                                },
                                now,
                            );
                        }
                    }
                }
            }
            _ => self.insert(
                FsPath::new(""),
                WorkspaceDirtyFact {
                    kind: WorkspaceFileChangeKind::Subtree,
                    first: arrival,
                    last: arrival,
                    raw_kind: None,
                    observation,
                },
                now,
            ),
        }
    }

    fn insert(&mut self, path: &FsPath, fact: WorkspaceDirtyFact, now: std::time::Instant) {
        self.oldest.get_or_insert(now);
        self.newest = Some(now);
        // Only an unplaceable path or backend loss needs a global marker.
        // Long paths retain the deepest bounded ancestor instead.
        if !path.is_absolute()
            || path
                .components()
                .any(|part| part == std::path::Component::ParentDir)
        {
            self.root.insert(&[], fact, 0);
            self.root.collapse(0);
            return;
        }
        let mut parts = Vec::new();
        let mut retained_bytes = 0;
        let mut fact = fact;
        for part in path.components() {
            retained_bytes += part.as_os_str().len() * 2 + 1;
            if parts.len() >= WORKSPACE_PENDING_DEPTH
                || retained_bytes > WORKSPACE_PENDING_PATH_BYTES
            {
                fact.kind = WorkspaceFileChangeKind::Subtree;
                break;
            }
            parts.push(part.as_os_str().to_owned());
        }
        self.root.insert(&parts, fact, 0);
        let (nodes, bytes) = self.root.usage();
        if nodes > WORKSPACE_PENDING_NODES
            || bytes + self.root.delivery_bytes(0) > WORKSPACE_PENDING_BYTES
        {
            let depth = self.root.collapse_candidate(
                &parts,
                nodes.saturating_sub(WORKSPACE_PENDING_NODES),
                (bytes + self.root.delivery).saturating_sub(WORKSPACE_PENDING_BYTES),
                0,
            ).expect("root collapse always fits the budget");
            self.root
                .collapse_at(&parts[..depth], 0);
        }
    }

    // The callback logs outside this mutex, with no backend-controlled text.
    fn error_log_due(&mut self, now: std::time::Instant) -> bool {
        if self
            .last_error_log
            .is_some_and(|at| now.saturating_duration_since(at) < Duration::from_secs(30))
        {
            return false;
        }
        self.last_error_log = Some(now);
        true
    }

    fn due(&self, now: std::time::Instant) -> bool {
        self.oldest
            .is_some_and(|at| now.saturating_duration_since(at) >= Duration::from_secs(1))
            || self
                .newest
                .is_some_and(|at| now.saturating_duration_since(at) >= Duration::from_millis(250))
    }

    fn take(&mut self) -> Vec<(PathBuf, WorkspaceDirtyFact)> {
        self.oldest = None;
        self.newest = None;
        let mut out = Vec::new();
        std::mem::take(&mut self.root).drain(&mut PathBuf::new(), &mut out);
        out
    }
}

#[cfg(test)]
mod workspace_pending_tests {
    use super::*;

    fn fact(order: u64) -> WorkspaceDirtyFact {
        let arrival = EngramWatcherArrival {
            order,
            at: std::time::SystemTime::UNIX_EPOCH,
        };
        WorkspaceDirtyFact {
            kind: WorkspaceFileChangeKind::Modified,
            first: arrival,
            last: arrival,
            raw_kind: None,
            observation: None,
        }
    }

    #[test]
    fn burst_is_bounded_and_every_path_is_covered() {
        let mut pending = WorkspacePendingChanges::default();
        let root = std::env::current_dir().unwrap();
        let paths: Vec<_> = (0..20000)
            .map(|i| root.join(format!("fixture/d{}/f{i}", i / 100)))
            .collect();
        let now = std::time::Instant::now();
        for (i, path) in paths.iter().enumerate() {
            pending.insert(path, fact(i as u64 + 1), now);
            let (nodes, bytes) = pending.root.usage();
            assert!(nodes <= WORKSPACE_PENDING_NODES);
            assert!(bytes <= WORKSPACE_PENDING_BYTES);
        }
        let batch = pending.take();
        for path in paths {
            assert!(batch.iter().any(|(covered, f)| covered == &path
                || f.kind == WorkspaceFileChangeKind::Subtree && path.starts_with(covered)));
        }
        assert!(batch.iter().any(|(_, f)| f.last.order == 20000));
    }

    #[test]
    fn repeats_preserve_interval_and_never_postpone_oldest_deadline() {
        let mut pending = WorkspacePendingChanges::default();
        let path = std::env::current_dir().unwrap().join("src/file.rs");
        let now = std::time::Instant::now();
        for i in 0..11 {
            pending.insert(&path, fact(i + 1), now + Duration::from_millis(i * 100));
        }
        assert!(pending.due(now + Duration::from_secs(1)));
        let batch = pending.take();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].1.first.order, 1);
        assert_eq!(batch[0].1.last.order, 11);
        assert!(!pending.due(now + Duration::from_secs(3)));
    }

    #[test]
    fn ingest_create_then_delete_keeps_one_modified_fact_and_arrival_interval() {
        // No disk entry survives this pair; notifications still describe a
        // real write interval, rather than being cancelled as a no-op.
        let path = PathBuf::from(if cfg!(windows) {
            "C:/transient-fixture/file.rs"
        } else {
            "/transient-fixture/file.rs"
        });
        let now = std::time::Instant::now();
        let first = EngramWatcherArrival {
            at: std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(41),
            ..fact(41).first
        };
        let last = EngramWatcherArrival {
            at: std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(42),
            ..fact(42).last
        };
        let mut pending = WorkspacePendingChanges::default();
        pending.ingest(
            Ok(NotifyEvent::new(NotifyEventKind::Create(notify::event::CreateKind::File))
                .add_path(path.clone())),
            first,
            now,
        );
        pending.ingest(
            Ok(NotifyEvent::new(NotifyEventKind::Remove(notify::event::RemoveKind::File))
                .add_path(path.clone())),
            last,
            now + Duration::from_millis(1),
        );
        let batch = pending.take();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].0, path);
        assert_eq!(batch[0].1.kind, WorkspaceFileChangeKind::Modified);
        assert_eq!(batch[0].1.first.order, first.order);
        assert_eq!(batch[0].1.first.at, first.at);
        assert_eq!(batch[0].1.last.order, last.order);
        assert_eq!(batch[0].1.last.at, last.at);
    }

    #[test]
    fn ingest_separate_windows_rename_from_and_to_keeps_both_subtrees() {
        let root = PathBuf::from(if cfg!(windows) {
            "C:/rename-fixture"
        } else {
            "/rename-fixture"
        });
        let old = root.join("old");
        let new = root.join("new");
        let mut pending = WorkspacePendingChanges::default();
        let now = std::time::Instant::now();
        for (order, mode, path) in [
            (1, notify::event::RenameMode::From, &old),
            (2, notify::event::RenameMode::To, &new),
        ] {
            pending.ingest(
                Ok(NotifyEvent::new(NotifyEventKind::Modify(notify::event::ModifyKind::Name(mode)))
                    .add_path(path.clone())),
                fact(order).first,
                now,
            );
        }
        let batch = pending.take();
        assert_eq!(batch.len(), 2);
        for (path, order) in [(&old, 1), (&new, 2)] {
            let (_, fact) = batch.iter().find(|(covered, _)| covered == path)
                .expect("both rename endpoints retained");
            assert_eq!(fact.kind, WorkspaceFileChangeKind::Subtree);
            assert_eq!(fact.first.order, order);
            assert_eq!(fact.last.order, order);
        }
    }

    #[test]
    fn long_paths_keep_the_deepest_bounded_ancestor() {
        let mut pending = WorkspacePendingChanges::default();
        let root = std::env::current_dir().unwrap();
        let now = std::time::Instant::now();
        pending.insert(&root.join("src/a"), fact(1), now);
        pending.insert(&root.join("src2/a"), fact(2), now);
        assert_eq!(pending.take().len(), 2);
        pending.insert(
            &root.join("a".repeat(WORKSPACE_PENDING_PATH_BYTES + 1)),
            fact(3),
            now,
        );
        let batch = pending.take();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].0, root);
        assert_eq!(batch[0].1.kind, WorkspaceFileChangeKind::Subtree);
        let deep = (0..WORKSPACE_PENDING_DEPTH + 20)
            .fold(root.clone(), |path, n| path.join(format!("d{n}")));
        pending.insert(&deep, fact(4), now);
        let batch = pending.take();
        assert!(!batch[0].0.as_os_str().is_empty());
        assert!(deep.starts_with(&batch[0].0));
        assert_ne!(deep, batch[0].0);
    }

    #[test]
    fn own_metadata_fact_is_not_replaced_by_descendant_summary() {
        let root = std::env::current_dir().unwrap().join("directory");
        let mut pending = WorkspacePendingChanges::default();
        let mut own = fact(1);
        own.raw_kind = Some(NotifyEventKind::Modify(notify::event::ModifyKind::Any));
        pending.insert(&root, own, std::time::Instant::now());
        let mut child = fact(2);
        child.kind = WorkspaceFileChangeKind::Deleted;
        pending.insert(&root.join("child"), child, std::time::Instant::now());
        let batch = pending.take();
        assert_eq!(batch.len(), 2);
        let own = batch.iter().find(|(path, _)| path == &root).unwrap().1;
        assert_eq!(own.kind, WorkspaceFileChangeKind::Modified);
        assert_eq!(
            own.raw_kind,
            Some(NotifyEventKind::Modify(notify::event::ModifyKind::Any))
        );
        assert_eq!(own.last.order, 1);
    }

    #[test]
    fn byte_budget_does_not_widen_a_deeper_path_in_another_root() {
        let root = std::env::current_dir().unwrap();
        let isolated = root.join(
            "isolated-root/one/two/three/four/five/six/seven/eight/nine/source.rs",
        );
        let burst = root.join("burst-root/src");
        let mut pending = WorkspacePendingChanges::default();
        let now = std::time::Instant::now();
        pending.insert(&isolated, fact(1), now);
        // Fewer than CHILDREN siblings at every level: widening here must
        // come from the total byte budget, not the per-node child limit.
        let paths: Vec<_> = (0..3000)
            .map(|n| burst.join(format!("d{}/f{}.rs", n / 48, n % 48)))
            .collect();
        for (n, path) in paths.iter().enumerate() {
            pending.insert(path, fact(n as u64 + 2), now);
            assert!(pending.root.nodes <= WORKSPACE_PENDING_NODES);
            assert!(pending.root.bytes + pending.root.delivery <= WORKSPACE_PENDING_BYTES);
        }
        let batch = pending.take();
        let isolated_fact = batch.iter().find(|(path, _)| path == &isolated)
            .expect("the unrelated deeper path stays exact").1;
        assert_eq!(isolated_fact.kind, WorkspaceFileChangeKind::Modified);
        assert_eq!(isolated_fact.first.order, 1);
        assert_eq!(isolated_fact.last.order, 1);
        assert!(batch.iter().any(|(path, fact)| path.starts_with(&burst)
            && fact.kind == WorkspaceFileChangeKind::Subtree));
        assert!(batch.iter().all(|(path, _)| path == &isolated || path.starts_with(&burst)));
        for path in paths {
            assert!(batch.iter().any(|(cover, fact)| cover == &path
                || fact.kind == WorkspaceFileChangeKind::Subtree && path.starts_with(cover)));
        }
    }

    #[test]
    fn byte_budget_collapse_stays_local_and_cached_accounting_is_exact() {
        fn audit(node: &WorkspaceDirtyNode, prefix: usize) -> (usize, usize, usize) {
            let mut total = (
                1,
                std::mem::size_of::<WorkspaceDirtyNode>(),
                if node.terminal.is_some() {
                    prefix + std::mem::size_of::<WorkspaceDirtyFact>()
                } else {
                    0
                },
            );
            for (key, child) in &node.children {
                let stats = audit(child, prefix + key.len() * 2 + 1);
                total.0 += stats.0;
                total.1 += stats.1 + key.capacity() * std::mem::size_of::<u16>() + 128;
                total.2 += stats.2;
            }
            assert_eq!((node.nodes, node.bytes, node.delivery), total);
            total
        }
        let root = std::env::current_dir().unwrap();
        let a = root.join("alpha");
        let b = root.join("beta/unchanged.rs");
        let mut pending = WorkspacePendingChanges::default();
        let now = std::time::Instant::now();
        pending.insert(&b, fact(1), now);
        let paths: Vec<_> = (0..3000)
            .map(|n| a.join(format!("d{}/f{}", n / 50, n % 50)))
            .collect();
        for (n, path) in paths.iter().enumerate() {
            pending.insert(path, fact(n as u64 + 2), now);
            let (nodes, bytes, delivery) = audit(&pending.root, 0);
            assert!(
                nodes <= WORKSPACE_PENDING_NODES && bytes + delivery <= WORKSPACE_PENDING_BYTES
            );
        }
        let batch = pending.take();
        assert!(
            batch
                .iter()
                .any(|(path, fact)| path == &b && fact.kind != WorkspaceFileChangeKind::Subtree)
        );
        assert!(
            batch.iter().any(|(path, fact)| path.starts_with(&a)
                && fact.kind == WorkspaceFileChangeKind::Subtree)
        );
        assert!(
            batch
                .iter()
                .all(|(path, _)| path == &b || path.starts_with(&a))
        );
        assert!(
            paths
                .iter()
                .all(|path| batch.iter().any(|(cover, fact)| cover == path
                    || fact.kind == WorkspaceFileChangeKind::Subtree && path.starts_with(cover)))
        );
    }

    #[test]
    fn empty_non_rescan_events_do_not_invalidate_and_errors_are_rate_limited() {
        let mut pending = WorkspacePendingChanges::default();
        let now = std::time::Instant::now();
        for kind in [
            NotifyEventKind::Other,
            NotifyEventKind::Access(notify::event::AccessKind::Any),
        ] {
            pending.ingest(Ok(NotifyEvent::new(kind)), fact(1).first, now);
        }
        assert!(pending.take().is_empty());
        assert!(pending.error_log_due(now));
        assert!(!pending.error_log_due(now + Duration::from_secs(29)));
        assert!(pending.error_log_due(now + Duration::from_secs(30)));
    }

    #[test]
    fn rename_delete_and_backend_loss_keep_covering_notifications() {
        // Virtual paths avoid coupling ingress filtering to the checkout's
        // ignored build-directory ancestors. This test touches no filesystem.
        let root = PathBuf::from(if cfg!(windows) {
            "C:/workspace-trie-fixture"
        } else {
            "/workspace-trie-fixture"
        });
        let old = root.join("old/subdir");
        let new = root.join("new/subdir");
        let now = std::time::Instant::now();
        let mut pending = WorkspacePendingChanges::default();
        pending.ingest(
            Ok(
                NotifyEvent::new(NotifyEventKind::Modify(notify::event::ModifyKind::Name(
                    notify::event::RenameMode::Both,
                )))
                .add_path(old.clone())
                .add_path(new.clone()),
            ),
            fact(1).first,
            now,
        );
        let batch = pending.take();
        assert_eq!(batch.len(), 2);
        for path in [&old, &new] {
            assert!(
                batch
                    .iter()
                    .any(|(p, f)| p == path && f.kind == WorkspaceFileChangeKind::Subtree)
            );
        }
        pending.ingest(
            Ok(
                NotifyEvent::new(NotifyEventKind::Remove(notify::event::RemoveKind::Folder))
                    .add_path(new),
            ),
            fact(2).first,
            now,
        );
        assert_eq!(pending.take()[0].1.kind, WorkspaceFileChangeKind::Subtree);
        pending.ingest(Err(notify::Error::generic("overflow")), fact(3).first, now);
        let batch = pending.take();
        assert!(batch[0].0.as_os_str().is_empty());
        assert_eq!(batch[0].1.kind, WorkspaceFileChangeKind::Subtree);
    }
}
