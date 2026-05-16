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
// - **State snapshots** — the metadata-first `StateResponse` shape, serialized
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
// `publish_snapshot`. The handoff is a single latest-snapshot mailbox,
// so bursty commits overwrite superseded snapshots before they can
// accumulate. That keeps the state mutex off the slow-serialization
// critical path for commit-heavy routes like `put_workspace_layout`.
// When there is no broadcaster mailbox (notably: test builds that
// construct `AppState` without spawning the broadcaster thread) we fall back to
// synchronous serialize + broadcast so tests can still assert SSE
// behaviour.

impl AppState {
    /// Central commit path: bumps the revision, wakes the persist
    /// thread, and broadcasts a metadata-first state snapshot over SSE.
    ///
    /// Call this after *any* mutation that should be visible to
    /// clients. The metadata-first snapshot is suitable for mutations
    /// that affect many fields at once (settings, workspace layouts,
    /// project CRUD) or that touch the sessions array shape (create,
    /// remove, reorder). For narrow per-field updates, prefer
    /// [`Self::commit_delta_locked`] to avoid re-serializing the
    /// whole state.
    fn commit_locked(&self, inner: &mut StateInner) -> Result<u64> {
        let revision = self.bump_revision_and_persist_locked(inner)?;
        self.publish_state_locked(inner);
        Ok(revision)
    }

    /// Commits a newly visible session and wakes the background persist
    /// thread so the new record lands in SQLite without holding the
    /// state mutex across disk I/O.
    ///
    /// Previously this called `persist_created_session` synchronously
    /// under the caller's mutex lock, so every session create blocked
    /// every concurrent request (SSE publishers, other handlers, the
    /// background persist thread itself) behind a full SQLite
    /// transaction — connection open, `ensure_sqlite_state_schema`
    /// upsert, metadata + session INSERT OR UPDATE, commit with
    /// fsync. On slow disks that adds 10–100 ms to every session
    /// creation while holding the global state lock.
    ///
    /// The session's `mutation_stamp` was already advanced by
    /// `session_mut_by_index` / `push_session` in the caller, so
    /// `collect_persist_delta(watermark)` on the background thread
    /// picks this record up on the next tick and writes it via the
    /// cached `SqlitePersistConnectionCache`. The crash-before-
    /// persist window loses at most a just-created empty session
    /// shell (metadata + config, no user content) — the same
    /// durability posture `persist_internal_locked` already has for
    /// every subsequent mutation on the session.
    ///
    /// Test + shutdown fallback: when `persist_tx.send` fails
    /// (channel disconnected — tests construct `AppState` with a
    /// receiver-dropped channel; shutdown happens if the persist
    /// thread exited early), fall back to the original synchronous
    /// `persist_created_session` path. That fallback writes a full
    /// snapshot so any sibling mutations from the create flow (for
    /// example hidden-spare pool changes) land with the created
    /// session.
    fn commit_session_created_locked(
        &self,
        inner: &mut StateInner,
        record: &SessionRecord,
    ) -> Result<u64> {
        // Ordering note: the revision tick happens before the
        // persist-route selection to match `persist_internal_locked`
        // (see [`Self::persist_internal_locked`] / `bump_revision_and_persist_locked`).
        // On the fallback path, if `persist_created_session` returns
        // `Err` the revision has already advanced but no persist
        // work landed — the caller surfaces that as
        // `ApiError::internal`, the record is not in SQLite, and the
        // next successful commit produces a non-contiguous revision.
        // Nothing today requires strict contiguity (clients gap-detect
        // and resync via `/api/state`), and keeping the ordering
        // symmetric with the sibling means the two paths share one
        // durability posture.
        inner.revision += 1;
        if self.persist_tx.send(PersistRequest::Delta).is_err() {
            persist_created_session(&self.persistence_path, inner, record)?;
        }
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

    /// Drains any pending persist work and joins the background persist
    /// thread. Intended to run as the last step of a graceful shutdown
    /// (after `axum::serve` returns) so the very last `commit_locked` /
    /// `commit_persisted_delta_locked` reaches SQLite before the
    /// process exits — closing the durability window described in
    /// bugs.md "Server restart without browser refresh can lose the
    /// last streamed message".
    ///
    /// Idempotent: subsequent calls are a no-op once the handle has
    /// been taken. Safe to call from `AppState` clones — the handle is
    /// shared via `Arc<Mutex<Option<_>>>`. Test-only constructors that
    /// don't spawn the worker store `None`; this method then has no
    /// thread to wait on and returns immediately. The handle mutex is
    /// held through `handle.join()` so a concurrent shutdown caller cannot
    /// observe `None` and publish `persist_worker_alive == false` while
    /// the join owner is still waiting for the worker to exit.
    ///
    /// Sends `PersistRequest::Shutdown` and joins the thread. The
    /// worker's loop drains every queued `Delta` (including any
    /// commits queued between the shutdown signal and the worker's
    /// next iteration) and runs one final `collect_persist_delta` /
    /// `persist_delta_via_cache` pass before exiting. After `join()`
    /// returns this method performs one final synchronous full-state
    /// persist while holding `inner`, then flips `persist_worker_alive`
    /// before releasing `inner`. That closes the narrow window where a
    /// delta-only mutation could land after the worker's final
    /// collection but before shutdown returned.
    ///
    /// `commit_delta_locked` calls AFTER this method returns are safe:
    /// the alive flag is flipped to `false` only after `handle.join()`
    /// completes, so the synchronous fallback path is never racing the
    /// worker's still-in-progress final drain. `commit_locked` and
    /// `commit_persisted_delta_locked` already infer fallback from
    /// `persist_tx.send` failure when the worker has exited and the
    /// channel becomes disconnected after the last sender drops; that
    /// inference can lag while other `AppState` clones still hold senders,
    /// so callers that need strict durability for those variants should
    /// quiesce their producers before invoking this method.
    fn shutdown_persist_blocking(&self) {
        let mut guard = self
            .persist_thread_handle
            .lock()
            .expect("persist thread handle mutex poisoned");
        let handle = guard.take();
        let _shutdown_guard = guard;
        let Some(handle) = handle else {
            // Idempotent: any previous join owner held this mutex until
            // `handle.join()` returned and the alive flag was flipped, so
            // observing `None` here means the worker is truly stopped.
            self.persist_worker_alive
                .store(false, std::sync::atomic::Ordering::Release);
            return;
        };
        // Best-effort signal: if the channel is already disconnected
        // (worker panicked or exited early) there is nothing to drain
        // — the join below still waits for the OS thread to fully exit.
        // Important: we do NOT flip `persist_worker_alive` to `false`
        // here. Any concurrent `commit_delta_locked` call on another
        // `AppState` clone observing `alive == false` would synchronously
        // write a full-state snapshot in parallel with the worker's
        // still-running final drain — racing two writers on the same
        // file. The flag is flipped AFTER `handle.join()` returns, so
        // the synchronous fallback only fires when the worker is
        // guaranteed not to drain anything. See bugs.md "Post-shutdown
        // persistence writes are not durably ordered" for the failure
        // mode this ordering closes.
        let _ = self.persist_tx.send(PersistRequest::Shutdown);
        if let Err(err) = handle.join() {
            eprintln!(
                "[termal] persist worker join failed during graceful shutdown: {err:?}"
            );
        }
        // Worker has now exited (join returned). Persist one final
        // full-state snapshot while holding `inner`, then publish
        // `alive == false` before releasing that lock. This makes
        // shutdown a durability fence: mutations that landed while the
        // worker was joining are included in this write, and mutations
        // after this point see `alive == false` and take the synchronous
        // fallback in `commit_delta_locked`.
        let final_persist_result = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let persisted = PersistedState::from_inner(&inner);
            let result = persist_state_from_persisted(&self.persistence_path, &persisted);
            self.persist_worker_alive
                .store(false, std::sync::atomic::Ordering::Release);
            result
        };
        if let Err(err) = final_persist_result {
            eprintln!(
                "[termal] final synchronous persist failed during graceful shutdown: {err:#}"
            );
        }
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
        // Post-shutdown durability: this commit shape doesn't send its own
        // `PersistRequest::Delta`. Normally the next persist-triggering
        // commit (via `commit_locked` / `commit_persisted_delta_locked`)
        // wakes the worker, which then drains everything past its
        // watermark — including the mutation_stamps this commit just
        // bumped. After `shutdown_persist_blocking` has run, the worker
        // is gone, so no future signal will drain this. Fall back to a
        // synchronous full-state JSON write on the same path the
        // disconnected-channel test fallback uses.
        //
        // Acquire-load: paired with the Release-store in
        // `shutdown_persist_blocking`, which flips the flag AFTER
        // `handle.join()` returns. Observing `alive == false` therefore
        // means the worker thread has demonstrably exited, so the
        // synchronous write below cannot race the worker's final drain.
        // (Earlier rounds flipped the flag BEFORE the join, which left a
        // window where a concurrent fallback writer and the worker's
        // final drain raced on the same persistence path; see bugs.md
        // "Post-shutdown persistence writes are not durably ordered".)
        // Note: this fallback runs synchronous file I/O while holding
        // `&mut StateInner`. That is acceptable post-shutdown because no
        // concurrent producers should still be running by the time the
        // worker has joined.
        if !self
            .persist_worker_alive
            .load(std::sync::atomic::Ordering::Acquire)
        {
            let persisted = PersistedState::from_inner(inner);
            persist_state_from_persisted(&self.persistence_path, &persisted)?;
        }
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
    /// (JSON-serialized metadata-first `StateResponse` snapshots). Each new SSE
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

    /// Returns a fresh `tokio::sync::watch::Receiver` for the shutdown
    /// signal. Sticky: if `send(true)` already ran before this call, the
    /// returned receiver's `borrow_and_update()` immediately reads `true`,
    /// so SSE handlers accepted in the brief window between Ctrl+C and
    /// graceful-shutdown completion observe the signal regardless of
    /// timing. Used by `api_sse.rs::state_events` and the production
    /// graceful-shutdown plumbing in `main.rs::run_server`.
    fn subscribe_shutdown_signal(&self) -> tokio::sync::watch::Receiver<bool> {
        self.shutdown_signal_tx.subscribe()
    }

    /// Triggers graceful shutdown: SSE handlers that currently hold a
    /// receiver for this signal will exit their `tokio::select!` loops on
    /// the next iteration, and any handler started after this call will
    /// see the sticky `true` value during its initial `borrow_and_update()`
    /// pre-check. Idempotent — repeated calls just re-store `true`.
    ///
    /// `send_replace` rather than `send`: the latter returns an error and
    /// **does not update the value** when there are zero live receivers,
    /// which would defeat the sticky contract — a `trigger` call made
    /// before any SSE handler subscribes would silently fail and a later
    /// subscriber would see `false`. `send_replace` updates the value
    /// regardless of receiver count, which is the documented "sticky
    /// single-producer broadcast" pattern. See bugs.md "One-shot SSE
    /// shutdown notification can be missed before waiter registration".
    fn trigger_shutdown_signal(&self) {
        self.shutdown_signal_tx.send_replace(true);
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

    /// Publishes a `SessionCreated` delta for the remote-proxy
    /// session-upsert paths iff the upsert actually changed the
    /// local record.
    ///
    /// `remote_create_proxies.rs` and `remote_codex_proxies.rs`
    /// both run the same post-lock-drop flow: call
    /// `ensure_remote_proxy_session_record`, observe a `changed`
    /// boolean, and emit a `SessionCreated` delta only when that
    /// boolean is true. Emitting the delta unconditionally would
    /// be protocol-smell — the client silently drops same-revision
    /// deltas via `decideDeltaRevisionAction`, but advertising a
    /// mutation that did not happen leaks a subtle drift risk when
    /// two remote-proxy call sites start to diverge (e.g., one
    /// adds a new delta variant and the other misses it).
    ///
    /// Routing both sites through this helper keeps the
    /// "announce only when the record actually changed"
    /// invariant in one place. Takes an already-projected summary so
    /// callers build `SessionCreated` metadata from `SessionRecord`
    /// instead of cloning a full transcript-bearing `Session` and
    /// clearing its messages.
    ///
    /// Deliberately NOT used by
    /// `remote_routes.rs::apply_remote_delta_event`. That path
    /// forwards an inbound `SessionCreated` delta from a remote
    /// TermAl where the revision bump has already happened on
    /// the source. The inbound side's
    /// `should_skip_remote_applied_delta_revision` guard already
    /// filters true revision duplicates; gating THAT site on
    /// local-record `changed` would silence legitimate re-
    /// broadcasts (e.g., a duplicate apply whose local mirror
    /// was already in sync) and break chained-remote topologies
    /// where downstream clients have not yet seen the delta via
    /// any other path. Different semantic layer, different gate.
    fn announce_remote_session_created_if_changed(
        &self,
        changed: bool,
        revision: u64,
        session_id: &str,
        delta_session: Option<Session>,
    ) {
        if !changed {
            return;
        }
        let delta_session =
            delta_session.expect("changed remote session creation must include a delta summary");
        self.publish_delta(&DeltaEvent::SessionCreated {
            revision,
            session_id: session_id.to_owned(),
            session: delta_session,
        });
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

    /// Fire-and-forget metadata-first snapshot broadcast, paired with
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
    /// Sends the owned snapshot to the background broadcaster mailbox, whose
    /// thread serializes to JSON and forwards to `state_events` off the
    /// critical path. The mailbox retains only the latest pending snapshot.
    /// Falls back to synchronous serialize + broadcast if no mailbox exists
    /// (test builds that construct `AppState` manually without a broadcaster
    /// thread).
    fn publish_snapshot(&self, snapshot: StateResponse) {
        if let Some(mailbox) = &self.state_broadcast_mailbox {
            mailbox.publish(snapshot);
            return;
        }

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
