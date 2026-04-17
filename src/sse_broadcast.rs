// Commit + persist + SSE broadcast pipeline for `AppState`.
//
// Every client-visible mutation in TermAl lands here. The contract is
// a monotonic `revision` counter on `StateInner`: each `commit_locked`
// call bumps it by one, clients subscribe to the revision stream via
// SSE, and clients that reconnect can resume from a known revision
// and replay any deltas they missed. The counter is load-bearing for
// the remote-proxy sync path too — see `remote_sync.rs` for how
// remote followers compare their own applied revision against the
// source's.
//
// Two flavours of event leave the backend:
//
// - **State snapshots** — the full `StateResponse` shape, serialized
//   once per commit and pushed on the `state_events` channel. Used by
//   new SSE clients and remote proxies that need a ground-truth
//   snapshot after reconnect.
// - **Delta events** (`DeltaEvent` in `wire.rs`) — narrow per-field
//   updates pushed on the `delta_events` channel. Clients that stay
//   connected apply them to their local copy without re-parsing the
//   whole state tree.
//
// There is also a third channel — `file_events` — for file-system
// watch notifications (see `workspace_watch.rs`). These are not tied
// to the revision counter and fire independently whenever a watched
// path changes; they exist so the UI's workspace tree can repaint
// without waiting for a full state refresh, and because file-change
// bursts would overwhelm the state channel.
//
// Mutation-stamp → persist pattern. `commit_locked` bumps every
// mutated session's `mutation_stamp` via `session_mut_by_index`, and
// the background persist thread wakes on each commit, collects the
// diff of sessions whose stamp is newer than its last watermark, and
// writes them to disk. This keeps the state mutex off the I/O path.
// The `_delta_*` variants of commit exist for mutations that change
// *only* a narrow per-session field; they skip the full snapshot
// broadcast and emit a delta event instead. Internal bookkeeping
// (watermarks, readiness cache eviction, etc.) that shouldn't tick
// the client-visible revision uses [`Self::persist_internal_locked`].
//
// Snapshot serialization offload. `publish_state_locked` builds the
// `StateResponse` *inside* the state lock (required — snapshot
// fields read `inner`) but hands the owned snapshot off to a
// dedicated broadcaster thread for JSON serialization via
// `publish_snapshot`. That keeps the state mutex off the slow-
// serialization critical path for commit-heavy routes like
// `put_workspace_layout`. When the broadcaster channel is
// disconnected (notably: test builds that construct `AppState`
// without spawning the broadcaster thread) we fall back to
// synchronous serialize + broadcast so tests can still assert SSE
// behaviour.

impl AppState {
    /// Central commit path: bumps the revision, wakes the persist
    /// thread, and broadcasts a full state snapshot over SSE.
    ///
    /// Call this after *any* mutation that should be visible to
    /// clients. The full snapshot is suitable for mutations that
    /// affect many fields at once (settings, workspace layouts,
    /// project CRUD) or that touch the sessions array shape (create,
    /// remove, reorder). For narrow per-field updates, prefer
    /// [`Self::commit_delta_locked`] to avoid re-serializing the
    /// whole state.
    fn commit_locked(&self, inner: &mut StateInner) -> Result<u64> {
        let revision = self.bump_revision_and_persist_locked(inner)?;
        self.publish_state_locked(inner);
        Ok(revision)
    }

    /// Commits a newly visible session without cloning every historical
    /// message. SQLite production persistence can update global counters plus
    /// the created session row; test JSON persistence keeps the legacy full
    /// snapshot path so existing persistence tests stay representative.
    fn commit_session_created_locked(
        &self,
        inner: &mut StateInner,
        record: &SessionRecord,
    ) -> Result<u64> {
        inner.revision += 1;
        persist_created_session(&self.persistence_path, inner, record)?;
        Ok(inner.revision)
    }

    // Internal bookkeeping changes should be persisted without advancing the client-visible revision.
    /// Persists internal locked.
    ///
    /// Sends a `PersistRequest::Delta` wake signal to the background
    /// persist thread; the thread then locks `inner` briefly on its
    /// own to collect the diff of sessions whose `mutation_stamp` is
    /// past its internal watermark. This means `commit_locked` no
    /// longer pays to clone `PersistedState::from_inner(inner)` under
    /// the state mutex on every mutation — for a visible-session list
    /// with long transcripts, that clone used to dominate the
    /// mutation hot path.
    ///
    /// In `#[cfg(test)]` builds, `AppState` is typically constructed
    /// manually with a disconnected persist channel; the send fails
    /// and we fall back to the old synchronous JSON persist so
    /// existing test infrastructure keeps working.
    fn persist_internal_locked(&self, inner: &StateInner) -> Result<()> {
        if self.persist_tx.send(PersistRequest::Delta).is_err() {
            // Channel disconnected — synchronous fallback for tests
            // and any shutdown path where the persist thread has
            // already exited. Build the full persist payload here
            // because we have no background worker to do it.
            let persisted = PersistedState::from_inner(inner);
            persist_state_from_persisted(&self.persistence_path, &persisted)?;
        }
        Ok(())
    }

    // Delta-producing changes advance the revision without publishing a full snapshot; the delta event
    // carries the new revision instead. Persisting the full state on every streamed chunk makes
    // long responses increasingly slow, so durable persistence is deferred until the next
    // non-delta commit.
    /// Commit variant that bumps the revision + wakes the persist
    /// thread but broadcasts a delta event instead of a full snapshot.
    ///
    /// Callers are expected to have already emitted the matching
    /// `DeltaEvent` via [`Self::publish_delta`] before calling this —
    /// the revision bump then happens atomically under the same lock
    /// so clients see the delta and revision tick together. Used from
    /// the per-session mutation helpers in `session_messages.rs` and
    /// `turn_lifecycle.rs` where full snapshots would be overkill.
    fn commit_delta_locked(&self, inner: &mut StateInner) -> Result<u64> {
        inner.revision += 1;
        Ok(inner.revision)
    }

    // Some live-update paths still need durable persistence, but should not force a full-state
    // SSE snapshot when a small targeted delta is enough for the UI.
    /// Like [`Self::commit_delta_locked`] but used by paths where
    /// the mutation has already been persisted (or will be persisted
    /// out-of-band) — skips the wake-persist-thread step.
    fn commit_persisted_delta_locked(&self, inner: &mut StateInner) -> Result<u64> {
        self.bump_revision_and_persist_locked(inner)
    }

    /// Inner helper: ticks `inner.revision` by one and signals the
    /// background persist thread to pick up whatever sessions got
    /// stamped by this commit. Does not broadcast — the caller
    /// (`commit_locked` or the delta variants) publishes separately.
    fn bump_revision_and_persist_locked(&self, inner: &mut StateInner) -> Result<u64> {
        inner.revision += 1;
        self.persist_internal_locked(inner)?;
        Ok(inner.revision)
    }

    /// Returns a receiver on the `state_events` broadcast channel
    /// (JSON-serialized full `StateResponse` snapshots). Each new SSE
    /// subscriber in `api.rs` calls this to start receiving
    /// post-revision snapshots; the initial snapshot is sent
    /// separately from the connect handler.
    fn subscribe_events(&self) -> broadcast::Receiver<String> {
        self.state_events.subscribe()
    }

    /// Returns a receiver on the `delta_events` broadcast channel
    /// (JSON-serialized `DeltaEvent` payloads). Used by SSE clients
    /// that want narrow per-field updates instead of full snapshots.
    fn subscribe_delta_events(&self) -> broadcast::Receiver<String> {
        self.delta_events.subscribe()
    }

    /// Returns a receiver on the `file_events` broadcast channel
    /// (JSON-serialized `WorkspaceFileChangeEvent` batches). Fires
    /// independently of the revision counter — driven by the
    /// workspace file watcher in `workspace_watch.rs`.
    fn subscribe_file_events(&self) -> broadcast::Receiver<String> {
        self.file_events.subscribe()
    }

    /// Serializes a `DeltaEvent` to JSON and fans it out on the
    /// delta-events channel. Callers typically follow up with
    /// [`Self::commit_delta_locked`] under the same lock so the
    /// revision tick and the delta land atomically from the client's
    /// perspective.
    fn publish_delta(&self, event: &DeltaEvent) {
        if let Ok(payload) = serde_json::to_string(event) {
            let _ = self.delta_events.send(payload);
        }
    }

    /// Broadcasts a batch of file-change events on the `file_events`
    /// channel and also records them against the currently active
    /// turn (if any) via [`Self::record_active_turn_file_changes`].
    /// Called from the workspace file watcher thread.
    #[cfg_attr(test, allow(dead_code))]
    fn publish_workspace_files_changed(&self, changes: Vec<WorkspaceFileChangeEvent>) {
        if changes.is_empty() {
            return;
        }

        let event = WorkspaceFilesChangedEvent {
            revision: self.file_events_revision.fetch_add(1, Ordering::Relaxed) + 1,
            changes,
        };
        if let Ok(payload) = serde_json::to_string(&event) {
            let _ = self.file_events.send(payload);
        }
    }

    /// Attaches the given file-change events to any active turn in
    /// the session whose workdir scope overlaps the changed paths.
    /// This is how the "files changed during this turn" UI widget
    /// gets its data — the events are stored on the active
    /// `TurnRecord` and replayed as part of the final turn summary.
    fn record_active_turn_file_changes(&self, changes: &[WorkspaceFileChangeEvent]) {
        if changes.is_empty() {
            return;
        }

        let session_scoped_change_paths = changes
            .iter()
            .filter(|change| change.session_id.as_deref().is_some_and(|value| !value.trim().is_empty()))
            .map(|change| change.path.trim().to_owned())
            .collect::<HashSet<_>>();
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let now = std::time::Instant::now();
        let mut late_summary_session_indexes = Vec::<usize>::new();
        for index in 0..inner.sessions.len() {
            let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
            if record.is_remote_proxy() || record.hidden {
                continue;
            }

            let is_active_turn = record.active_turn_start_message_count.is_some();
            let is_grace_turn = record
                .active_turn_file_change_grace_deadline
                .is_some_and(|deadline| now <= deadline);
            if !is_active_turn && !is_grace_turn {
                record.active_turn_file_change_grace_deadline = None;
                continue;
            }

            for change in changes {
                let path = change.path.trim();
                if change.session_id.is_none() && session_scoped_change_paths.contains(path) {
                    continue;
                }
                if change
                    .session_id
                    .as_deref()
                    .is_some_and(|session_id| session_id != record.session.id)
                {
                    continue;
                }

                if path.is_empty() || !path_contains(&record.session.workdir, FsPath::new(path)) {
                    continue;
                }

                record
                    .active_turn_file_changes
                    .entry(path.to_owned())
                    .and_modify(|kind| *kind = merge_workspace_file_change_kind(*kind, change.kind))
                    .or_insert(change.kind);
            }

            if !is_active_turn && is_grace_turn && !record.active_turn_file_changes.is_empty() {
                late_summary_session_indexes.push(index);
            }
        }

        if late_summary_session_indexes.is_empty() {
            return;
        }

        for index in late_summary_session_indexes {
            let message_id = inner.next_message_id();
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            push_active_turn_file_changes_on_record(record, message_id);
            record.active_turn_file_change_grace_deadline = None;
        }
        if let Err(err) = self.commit_locked(&mut inner) {
            eprintln!(
                "state warning> failed to persist late turn file-change summary: {err:#}"
            );
        }
    }

    /// Fire-and-forget full-snapshot broadcast, paired with
    /// [`Self::publish_delta`] for deltas; serialization errors are
    /// logged but do not propagate.
    ///
    /// The snapshot is built from `inner` while the caller still holds
    /// the state mutex (required — `inner` fields are read here), but
    /// JSON serialization is offloaded to a dedicated broadcaster
    /// thread via [`Self::publish_snapshot`]. This keeps the state
    /// mutex off the serialization critical path for requests (e.g.,
    /// `put_workspace_layout`) that commit under the lock.
    fn publish_state_locked(&self, inner: &StateInner) {
        let snapshot = self.snapshot_from_inner(inner);
        self.publish_snapshot(snapshot);
    }

    /// Publishes a pre-built snapshot as an SSE state event.
    ///
    /// Sends the owned snapshot to the background broadcaster thread,
    /// which serializes to JSON and forwards to `state_events` off the
    /// critical path. Falls back to synchronous serialize + broadcast
    /// if the channel is disconnected (test builds that construct
    /// `AppState` manually without a broadcaster thread).
    fn publish_snapshot(&self, snapshot: StateResponse) {
        if let Err(mpsc::SendError(snapshot)) = self.state_broadcast_tx.send(snapshot) {
            match serde_json::to_string(&snapshot) {
                Ok(payload) => {
                    let _ = self.state_events.send(payload);
                }
                Err(err) => {
                    eprintln!(
                        "warning: failed to serialize SSE state snapshot at revision {}: {err}",
                        snapshot.revision,
                    );
                }
            }
        }
    }
}
