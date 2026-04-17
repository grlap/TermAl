// `AppState` constructor — the single entry point for bringing a
// fresh or restored state into a healthy, ready-to-serve shape.
//
// This is the heaviest single function in the server. It:
//
// 1. Resolves canonical on-disk paths (`~/.termal/sessions.json`,
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

        // Background persist thread: drains `PersistRequest` signals and
        // writes the delta or full snapshot.
        //
        // Normal operation: `Delta` signals. The thread locks
        // `inner_for_persist` briefly, collects the diff of sessions
        // whose `mutation_stamp` advanced past its own watermark plus
        // the drained `removed_session_ids`, releases the lock, writes
        // with targeted `INSERT OR UPDATE` / `DELETE WHERE id = ?`, and
        // advances its watermark.
        //
        // `Full` signals (startup and reset paths) ship a pre-built
        // `PersistedState` payload that replaces the entire sessions
        // table — necessary for initial disk layout and for the
        // JSON→SQLite import path where the thread cannot diff against
        // a previous state.
        //
        // The thread owns a `SqlitePersistConnectionCache` so the SQLite
        // connection and schema-validation cost are amortized across
        // every queued write — previously every persist opened a fresh
        // connection and re-ran `ensure_sqlite_state_schema`, which
        // writes `schema_version` on every call.
        std::thread::Builder::new()
            .name("termal-persist".to_owned())
            .spawn(move || {
                #[cfg(not(test))]
                let mut cache = SqlitePersistConnectionCache::new();
                #[cfg_attr(test, allow(unused_mut, unused_variables))]
                let mut watermark: u64 = 0;
                while let Ok(PersistRequest::Delta) = persist_rx.recv() {
                    // Drain any queued signals — the delta collection
                    // below captures everything that has changed since
                    // the last tick regardless of how many Delta
                    // signals queued up, so extra signals are pure
                    // duplicates.
                    while persist_rx.try_recv().is_ok() {}

                    #[cfg(not(test))]
                    let result: Result<()> = (|| {
                        let delta = {
                            let mut inner = inner_for_persist
                                .lock()
                                .expect("state mutex poisoned");
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
                        persist_delta_via_cache(
                            &mut cache,
                            &persist_path_for_persist,
                            &delta,
                        )?;
                        watermark = next_watermark;
                        Ok(())
                    })();

                    #[cfg(test)]
                    let result: Result<()> = {
                        // Tests run the old full-state JSON path so
                        // existing persist-related assertions keep
                        // working without knowing about stamps.
                        let persisted = {
                            let inner = inner_for_persist
                                .lock()
                                .expect("state mutex poisoned");
                            PersistedState::from_inner(&inner)
                        };
                        persist_state_from_persisted(
                            &persist_path_for_persist,
                            &persisted,
                        )
                    };

                    if let Err(err) = result {
                        eprintln!("[termal] background persist failed: {err:#}");
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
            state_broadcast_tx,
            shared_codex_runtime: Arc::new(Mutex::new(None)),
            agent_readiness_cache,
            agent_readiness_refresh_lock: Arc::new(Mutex::new(())),
            remote_registry: Arc::new(
                std::thread::spawn(RemoteRegistry::new)
                    .join()
                    .expect("remote registry init thread panicked")?,
            ),
            remote_sse_fallback_resynced_revision: Arc::new(Mutex::new(HashMap::new())),
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
