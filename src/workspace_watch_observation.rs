// Bounded diagnostic facts captured before watcher queueing/conversion.
// These clocks describe receipt and processing, never the time of the write.
// They do not authorize a source-event exemption or identify an OS writer.
#[derive(Clone, Copy)]
struct WorkspaceWatchObservation {
    observed: u64,
    received_at: std::time::Instant,
    observed_at: chrono::DateTime<chrono::Utc>,
    kind: notify::EventKind,
    tracker: Option<usize>,
    pid: Option<u32>,
    rescan: bool,
    info_present: bool,
    source_present: bool,
    path_count: usize,
}

impl WorkspaceWatchObservation {
    fn capture(event: &notify::Event) -> Self {
        // Info/source are backend-supplied strings. Retain their presence,
        // not their contents (which can contain paths or private details).
        Self {
            observed: engram_interference_tick(),
            received_at: std::time::Instant::now(),
            observed_at: chrono::Utc::now(),
            kind: event.kind,
            tracker: event.tracker(),
            pid: event.attrs.process_id(),
            rescan: event.need_rescan(),
            info_present: event.info().is_some(),
            source_present: event.source().is_some(),
            path_count: event.paths.len(),
        }
    }

    fn processing(&self, processed: u64, started_at: std::time::Instant) -> String {
        let tracker = self.tracker.map(|id| id.to_string());
        let pid = self.pid.map(|id| id.to_string());
        format!(
            "raw_kind={:?} tracker={} rescan={} writer_pid={} info_present={} source_present={} raw_paths={} observed={} observed_at={} processed={} converted={} queue_us={}",
            self.kind,
            tracker.as_deref().unwrap_or("unknown"),
            self.rescan,
            pid.as_deref().unwrap_or("unknown"),
            self.info_present,
            self.source_present,
            self.path_count,
            self.observed,
            self.observed_at
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            processed,
            engram_interference_tick(),
            started_at
                .saturating_duration_since(self.received_at)
                .as_micros()
                .min(u64::MAX as u128),
        )
    }
}

#[cfg(test)]
fn workspace_watch_unknown_observation() -> String {
    format!(
        "raw_kind=unknown observed=unknown writer_pid=unknown processed={} queue_us=unknown",
        engram_interference_tick(),
    )
}

const WORKSPACE_WATCH_PROVENANCE_LIMIT: usize = 1400;

#[cfg(test)]
thread_local! {
    static TEST_WORKSPACE_WATCH_DIAGNOSTICS: std::cell::RefCell<VecDeque<String>> =
        const { std::cell::RefCell::new(VecDeque::new()) };
}

#[cfg(test)]
fn take_workspace_watch_diagnostics() -> Vec<String> {
    TEST_WORKSPACE_WATCH_DIAGNOSTICS.with(|lines| lines.borrow_mut().drain(..).collect())
}

impl AppState {
    fn process_workspace_dirty_batch(&self, batch: Vec<(PathBuf, WorkspaceDirtyFact)>) -> Vec<WorkspaceFileChangeEvent> {
        let processed = engram_interference_tick();
        let started_at = std::time::Instant::now();
        let mut publication = Vec::new();
        for (path, fact) in batch {
            if fact.kind != WorkspaceFileChangeKind::Subtree
                && fact
                    .raw_kind
                    .as_ref()
                    .is_some_and(|kind| workspace_directory_metadata_hint(kind, &path))
            {
                continue;
            }
            // No scope fan-out: consumers match the source path themselves.
            let changes = workspace_file_changes_from_path(&path, fact.kind, &[]);
            let observation = fact.observation.map(|raw| raw.processing(processed, started_at))
                .unwrap_or_else(|| "raw_kind=unknown observed=unknown writer_pid=unknown".to_owned());
            let facts = format!(
                "{} coalesced=true first_order={} last_order={} subtree={}",
                observation,
                fact.first.order,
                fact.last.order,
                fact.kind == WorkspaceFileChangeKind::Subtree
            );
            // Keep each interval: a late arrival for a different path must not
            // cancel a pre-launch exemption for this path.
            self.engram_host()
                .observed_workspace_files_changed(&changes, &facts, Some(fact.last));
            publication.extend(changes);
        }
        self.record_active_turn_file_changes(&publication);
        self.publish_workspace_files_changed(publication.clone());
        publication
    }

    fn publish_workspace_watch_diagnostics(&self, diagnostics: Vec<String>) {
        #[cfg(test)]
        assert!(self.inner.is_not_held_by_current_thread_for_test());
        for diagnostic in diagnostics {
            debug_assert!(diagnostic.len() <= WORKSPACE_WATCH_PROVENANCE_LIMIT);
            eprintln!("file watch> {diagnostic}");
            #[cfg(test)]
            TEST_WORKSPACE_WATCH_DIAGNOSTICS.with(|lines| {
                let mut lines = lines.borrow_mut();
                if lines.len() == 8 {
                    lines.pop_front();
                }
                lines.push_back(diagnostic);
            });
        }
    }

    // Test entrypoint through the production ingress, drain and handoff.
    // UI publication/coalescing remains owned by the watcher loop. `arrival`
    // is when the notification reached the host, taken in the notify callback
    // (`EngramHost::watcher_arrival_now`); the Engram fence orders it against
    // a check's launch. The observation stays diagnostic only.
    #[cfg(test)]
    fn process_workspace_watch_event(
        &self,
        event: &notify::Event,
        observation: &WorkspaceWatchObservation,
        arrival: EngramWatcherArrival,
        _scopes: &[WorkspaceFileWatchScope],
    ) -> Vec<WorkspaceFileChangeEvent> {
        let mut pending = WorkspacePendingChanges::default();
        pending.ingest_observed(Ok(event.clone()), arrival, std::time::Instant::now(), Some(*observation));
        self.process_workspace_dirty_batch(pending.take())
    }
}
