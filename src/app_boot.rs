// `AppState` constructor — the single entry point for bringing a
// fresh or restored state into a healthy, ready-to-serve shape.
//
// This is the heaviest single function in the server. It:
//
// 1. Resolves canonical on-disk paths (`~/.termal/termal.sqlite`,
//    orchestrator templates dir) and loads persisted state if
//    present.
// 2. Builds the `StateInner` tree: sessions, projects, remotes,
//    workspace layouts, preferences, orchestrator instances.
// 3. Boot-time fixups via the helpers in `state_boot.rs`:
//    `import_discovered_codex_threads` (pull in threads found on
//    disk), `validate_projects_consistent` (abort on corruption),
//    `normalize_local_paths` (canonicalize workdirs),
//    `recover_interrupted_sessions` (reset Active/Approval sessions
//    back to Idle with runtime_reset_required).
// 4. Spawns the background persist thread (which drains
//    `collect_persist_delta` in a loop and writes to SQLite).
// 5. Spawns the SSE broadcaster thread (JSON-serialize state snapshots
//    off the state-mutex critical path — see `sse_broadcast.rs`).
// 6. Persists any boot-time fixups so the first mutation after
//    startup doesn't churn the whole file.
// 7. Restores remote SSE event bridges (`remote_sync.rs`).
// 8. (Non-test only) Spawns the workspace file watcher and
//    orchestrator transition resumer.
// 9. Calls `dispatch_orphaned_queued_prompts` so any queued prompts
//    that were stranded at shutdown fire their next turn immediately.
//
// Returns the `AppState` owning all of the above. The caller (in
// `main.rs`) then hands it to Axum + the HTTP server.
//
// Separated out of `state.rs` because the sheer length of this
// function made the types + struct definitions hard to navigate
// when editing; nothing else needs to live in this file.

const PERSIST_RETRY_SEED_DELAY: Duration = Duration::from_millis(250);
const PERSIST_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PersistWorkerRetryState {
    retry_after_failure: bool,
    retry_delay: Duration,
}

impl Default for PersistWorkerRetryState {
    fn default() -> Self {
        Self {
            retry_after_failure: false,
            retry_delay: PERSIST_RETRY_SEED_DELAY,
        }
    }
}

/// Outcome of a persist-worker wait. Distinguishes:
/// - a normal tick (process pending work and continue),
/// - a shutdown tick (process pending work one last time so the very last
///   commit reaches SQLite, then exit — see bugs.md "Server restart
///   without browser refresh can lose the last streamed message"),
/// - a clean exit (channel disconnected, nothing to do).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PersistWorkerWaitOutcome {
    Process,
    Shutdown,
    Exit,
}

impl PersistWorkerRetryState {
    fn wait_for_next_tick(
        &self,
        persist_rx: &mpsc::Receiver<PersistRequest>,
    ) -> PersistWorkerWaitOutcome {
        if self.retry_after_failure {
            match persist_rx.recv_timeout(self.retry_delay) {
                Ok(PersistRequest::Delta) => PersistWorkerWaitOutcome::Process,
                Ok(PersistRequest::Shutdown) => PersistWorkerWaitOutcome::Shutdown,
                // Synthetic retry tick: the previous attempt failed and the
                // backoff window expired with no new signal, so try again.
                Err(mpsc::RecvTimeoutError::Timeout) => PersistWorkerWaitOutcome::Process,
                Err(mpsc::RecvTimeoutError::Disconnected) => PersistWorkerWaitOutcome::Exit,
            }
        } else {
            match persist_rx.recv() {
                Ok(PersistRequest::Delta) => PersistWorkerWaitOutcome::Process,
                Ok(PersistRequest::Shutdown) => PersistWorkerWaitOutcome::Shutdown,
                Err(_) => PersistWorkerWaitOutcome::Exit,
            }
        }
    }

    fn record_result(&mut self, result: &Result<()>) {
        if result.is_err() {
            self.retry_after_failure = true;
            self.retry_delay =
                std::cmp::min(self.retry_delay * 2, PERSIST_RETRY_MAX_DELAY);
        } else {
            self.retry_after_failure = false;
            self.retry_delay = PERSIST_RETRY_SEED_DELAY;
        }
    }
}

impl AppState {
    /// Convenience constructor: resolves the default persistence
    /// paths from `default_workdir` and hands off to
    /// [`Self::new_with_paths`]. Callers that need explicit paths
    /// (tests, the remote-proxy bootstrap) use
    /// [`Self::new_with_paths`] directly.
    fn new(default_workdir: String) -> Result<Self> {
        let default_workdir = normalize_local_user_facing_path(&default_workdir);
        let persistence_path = resolve_persistence_path(&default_workdir);
        let orchestrator_templates_path = resolve_orchestrator_templates_path(&default_workdir);
        Self::new_with_paths(
            default_workdir,
            persistence_path,
            orchestrator_templates_path,
        )
    }

    /// Builds a fully-initialized [`AppState`] from explicit paths.
    ///
    /// Heavy-lifting entry point — see the file-level comment above
    /// for the numbered list of boot steps. Safe to call from tests
    /// with a temp path (the workspace file watcher + orchestrator
    /// transition resumer are `#[cfg(not(test))]` gated so a test
    /// AppState doesn't spawn background threads that outlive the
    /// test). Errors surface I/O / parse failures from the persisted
    /// state file; consistency failures from `state_boot.rs` helpers
    /// also bubble up here.
    fn new_with_paths(
        default_workdir: String,
        persistence_path: PathBuf,
        orchestrator_templates_path: PathBuf,
    ) -> Result<Self> {
        // Defensive: tests and other direct callers may pass an un-normalized workdir.
        let default_workdir = normalize_local_user_facing_path(&default_workdir);
        let mut inner = load_state(&persistence_path)?
            .unwrap_or_else(|| bootstrap_default_local_state(&default_workdir));
        let discovery_scopes = collect_codex_discovery_scopes(&default_workdir, &inner.projects);
        match discover_codex_threads(&default_workdir, &discovery_scopes) {
            Ok(discovered_threads) => {
                inner.import_discovered_codex_threads(&default_workdir, discovered_threads);
            }
            Err(err) => {
                eprintln!("codex discovery> failed to load Codex thread metadata: {err:#}");
            }
        }

        let agent_readiness_cache =
            Arc::new(RwLock::new(fresh_agent_readiness_cache(&default_workdir)));
        let (persist_tx, persist_rx) = mpsc::channel::<PersistRequest>();

        // `AppState::inner` is built here (rather than inside the struct
        // literal further below) so we can share an `Arc` clone with the
        // background persist thread. The thread briefly re-locks it on
        // each tick to collect the diff; see `StateInner::collect_persist_delta`.
        let inner_arc = Arc::new(Mutex::new(inner));
        let inner_for_persist = Arc::clone(&inner_arc);
        let persist_path_for_persist = Arc::new(persistence_path.clone());
        let persist_path_for_state = Arc::clone(&persist_path_for_persist);

        // Background persist thread: drains `PersistRequest::Delta`
        // wake signals and writes the accumulated diff to SQLite.
        //
        // On each signal the thread locks `inner_for_persist` briefly,
        // calls `StateInner::collect_persist_delta(watermark)` to build
        // the diff of sessions whose `mutation_stamp` advanced past
        // its own watermark plus the drained `removed_session_ids`,
        // releases the lock, and writes the delta with targeted
        // `INSERT OR UPDATE` per changed session and
        // `DELETE WHERE id = ?` per removed id. No `DELETE FROM sessions`
        // sweep is issued — unchanged rows stay untouched. See
        // `state.rs::PersistDelta` + `StateInner::collect_persist_delta`
        // for the delta contract.
        //
        // The thread owns a `SqlitePersistConnectionCache` so the SQLite
        // connection and schema-validation cost are amortized across
        // every queued write — previously every persist opened a fresh
        // connection and re-ran `ensure_sqlite_state_schema`, which
        // writes `schema_version` on every call.
        let persist_thread_handle = std::thread::Builder::new()
            .name("termal-persist".to_owned())
            .spawn(move || {
                #[cfg(not(test))]
                let mut cache = SqlitePersistConnectionCache::new();
                #[cfg_attr(test, allow(unused_mut, unused_variables))]
                let mut watermark: u64 = 0;
                let mut retry_state = PersistWorkerRetryState::default();
                loop {
                    let outcome = retry_state.wait_for_next_tick(&persist_rx);
                    if matches!(outcome, PersistWorkerWaitOutcome::Exit) {
                        break;
                    }
                    let mut should_exit_after_tick =
                        matches!(outcome, PersistWorkerWaitOutcome::Shutdown);
                    // Drain any queued signals — the delta collection
                    // below captures everything that has changed since
                    // the last tick regardless of how many Delta
                    // signals queued up, so extra signals are pure
                    // duplicates. A `Shutdown` request mixed in with
                    // queued deltas still flips the exit-after-tick
                    // flag so the very last delta reaches SQLite before
                    // we exit. See bugs.md "Server restart without
                    // browser refresh can lose the last streamed message".
                    while let Ok(req) = persist_rx.try_recv() {
                        if matches!(req, PersistRequest::Shutdown) {
                            should_exit_after_tick = true;
                        }
                    }

                    #[cfg(not(test))]
                    let result: Result<()> = (|| {
                        let delta = {
                            let mut inner = inner_for_persist.lock().expect("state mutex poisoned");
                            inner.collect_persist_delta(watermark)
                        };
                        let next_watermark = delta.watermark;
                        // Always upsert metadata (revision, preferences,
                        // projects, orchestrators, workspace_layouts).
                        // Mutation stamps only cover per-session changes,
                        // but commit_locked bumps `inner.revision` which
                        // must reach SQLite, and non-session fields can
                        // change without any session stamp moving. Empty
                        // `changed_sessions` + `removed_session_ids` is
                        // fine; the transaction just upserts one
                        // app_state row.
                        if let Err(err) =
                            persist_delta_via_cache(&mut cache, &persist_path_for_persist, &delta)
                        {
                            // On write failure, restore only the drained
                            // explicit `removed_session_ids` into `inner` so
                            // the next tick can retry the tombstones.
                            // Without this, a transient SQLite error
                            // (locked DB, disk full, I/O error) would
                            // silently leak an orphan `sessions` row
                            // into SQLite — `collect_persist_delta`
                            // drained the vec via `mem::take`, and
                            // since the watermark wasn't advanced the
                            // `changed_sessions` side auto-retries on
                            // the next tick, but the tombstone side
                            // has no equivalent per-row signal.
                            // `changed_sessions` and synthesized hidden-session
                            // deletes recover via mutation-stamp re-collection;
                            // only drained explicit tombstones need manual
                            // restoration.
                            if !delta.drained_explicit_tombstones.is_empty() {
                                let mut inner =
                                    inner_for_persist.lock().expect("state mutex poisoned");
                                inner.restore_drained_explicit_tombstones(
                                    &delta.drained_explicit_tombstones,
                                );
                            }
                            if !delta.drained_delegation_tombstones.is_empty() {
                                let mut inner =
                                    inner_for_persist.lock().expect("state mutex poisoned");
                                inner.restore_drained_delegation_tombstones(
                                    &delta.drained_delegation_tombstones,
                                );
                            }
                            return Err(err);
                        }
                        watermark = next_watermark;
                        Ok(())
                    })();

                    #[cfg(test)]
                    let result: Result<()> = {
                        // Tests run the old full-state JSON path so
                        // existing persist-related assertions keep
                        // working without knowing about stamps.
                        let persisted = {
                            let inner = inner_for_persist.lock().expect("state mutex poisoned");
                            PersistedState::from_inner(&inner)
                        };
                        persist_state_from_persisted(&persist_path_for_persist, &persisted)
                    };

                    if let Err(err) = &result {
                        eprintln!("[termal] background persist failed: {err:#}");
                    }
                    retry_state.record_result(&result);
                    if should_exit_after_tick {
                        break;
                    }
                }
            })
            .expect("failed to spawn persist thread");

        let state_events_sender = broadcast::channel::<String>(128).0;
        let (state_broadcast_tx, state_broadcast_rx) = mpsc::channel::<StateResponse>();

        // Background state-broadcast thread: drains queued state snapshots,
        // serializes each to JSON, and forwards the payload to the SSE
        // state-events broadcast channel. Coalesces queued snapshots to the
        // newest — intermediate revisions are safe to skip because a state
        // event is a full-state snapshot, not a delta. Subscribers converge
        // on the latest revision either way, and delta events fire in order
        // for every revision on a separate channel.
        let state_events_for_broadcast = state_events_sender.clone();
        std::thread::Builder::new()
            .name("termal-state-broadcast".to_owned())
            .spawn(move || {
                while let Ok(mut snapshot) = state_broadcast_rx.recv() {
                    while let Ok(newer) = state_broadcast_rx.try_recv() {
                        snapshot = newer;
                    }
                    match serde_json::to_string(&snapshot) {
                        Ok(payload) => {
                            let _ = state_events_for_broadcast.send(payload);
                        }
                        Err(err) => {
                            eprintln!(
                                "warning: failed to serialize SSE state snapshot at revision {}: {err}",
                                snapshot.revision,
                            );
                        }
                    }
                }
            })
            .expect("failed to spawn state broadcast thread");

        let state = Self {
            // Per-process UUID generated at boot. Every `StateResponse`
            // and `HealthResponse` carries this value so clients can
            // distinguish server-restart-driven revision rewinds from
            // out-of-order stale responses. A fresh UUID on every boot
            // guarantees the id changes exactly when the client should
            // accept a revision downgrade.
            server_instance_id: Uuid::new_v4().to_string(),
            default_workdir,
            persistence_path: persist_path_for_state,
            orchestrator_templates_path: Arc::new(orchestrator_templates_path),
            orchestrator_templates_lock: Arc::new(Mutex::new(())),
            review_documents_lock: Arc::new(Mutex::new(())),
            state_events: state_events_sender,
            delta_events: broadcast::channel(256).0,
            file_events: broadcast::channel(256).0,
            file_events_revision: Arc::new(AtomicU64::new(0)),
            persist_tx,
            persist_thread_handle: Arc::new(Mutex::new(Some(persist_thread_handle))),
            persist_worker_alive: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            shutdown_signal_tx: Arc::new(tokio::sync::watch::channel(false).0),
            state_broadcast_tx,
            shared_codex_runtime: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            test_acp_runtime_overrides: Arc::new(Mutex::new(Vec::new())),
            #[cfg(test)]
            test_agent_setup_failures: Arc::new(Mutex::new(Vec::new())),
            agent_readiness_cache,
            agent_readiness_refresh_lock: Arc::new(Mutex::new(())),
            remote_registry: Arc::new(
                std::thread::spawn(RemoteRegistry::new)
                    .join()
                    .expect("remote registry init thread panicked")?,
            ),
            remote_sse_fallback_resynced_revision: Arc::new(Mutex::new(HashMap::new())),
            remote_delta_replay_cache: Arc::new(Mutex::new(RemoteDeltaReplayCache::default())),
            remote_delta_hydrations_in_flight: Arc::new(Mutex::new(HashSet::new())),
            terminal_local_command_semaphore: Arc::new(tokio::sync::Semaphore::new(
                TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT,
            )),
            terminal_remote_command_semaphore: Arc::new(tokio::sync::Semaphore::new(
                TERMINAL_REMOTE_COMMAND_CONCURRENCY_LIMIT,
            )),
            stopping_orchestrator_ids: Arc::new(Mutex::new(HashSet::new())),
            stopping_orchestrator_session_ids: Arc::new(Mutex::new(HashMap::new())),
            inner: inner_arc,
        };
        state.seed_hidden_claude_spares();
        if let Err(err) = state.reconcile_delegation_waits_after_boot() {
            eprintln!("delegation wait> failed reconciling pending waits after boot: {err:#}");
        }
        {
            let inner = state.inner.lock().expect("state mutex poisoned");
            state.persist_internal_locked(&inner)?;
        }
        state.restore_remote_event_bridges();
        #[cfg(not(test))]
        state.spawn_workspace_file_watcher();
        if let Err(err) = state.resume_pending_orchestrator_transitions() {
            eprintln!("orchestrator> failed resuming pending transitions: {err:#}");
        }
        state.dispatch_orphaned_queued_prompts();
        Ok(state)
    }
}
