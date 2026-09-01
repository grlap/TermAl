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
// 4. Spawns the dedicated coordination-cleanup worker. Durable project
//    deletion outbox entries are handed to it only after their primary-state
//    commit, so large board cascades never block boot or termal.sqlite writes.
// 5. Spawns the background persist thread (which drains
//    the split-lock persist-delta collector in a loop and writes to SQLite).
// 6. Spawns the SSE broadcaster thread (JSON-serialize state snapshots
//    off the state-mutex critical path — see `sse_broadcast.rs`).
// 7. Persists any boot-time fixups so the first mutation after
//    startup doesn't churn the whole file.
// 8. Restores remote SSE event bridges (`remote_sync.rs`).
// 9. (Non-test only) Spawns the workspace file watcher and
//    orchestrator transition resumer.
// 10. Performs the one-time broad recovery of durable unread mailbox wake-ups,
//    then re-kicks only committed workflow-owned queue heads. Mailbox and
//    ordinary user prompts remain visible but dormant: a process restart is not
//    an activation event for either source.
//
// Returns the `AppState` owning all of the above. The caller (in
// `main.rs`) then hands it to Axum + the HTTP server.
//
// Separated out of `state.rs` because the sheer length of this
// function made the types + struct definitions hard to navigate
// when editing; nothing else needs to live in this file.

const PERSIST_RETRY_SEED_DELAY: Duration = Duration::from_millis(250);
const PERSIST_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);
const COORDINATION_CLEANUP_RETRY_SEED_DELAY: Duration = Duration::from_millis(250);
const COORDINATION_CLEANUP_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CoordinationCleanupRequest {
    Process,
    Shutdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CoordinationCleanupWaitOutcome {
    Process,
    Shutdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CoordinationCleanupRetryState {
    retry_pending: bool,
    retry_delay: Duration,
}

impl Default for CoordinationCleanupRetryState {
    fn default() -> Self {
        Self {
            retry_pending: false,
            retry_delay: COORDINATION_CLEANUP_RETRY_SEED_DELAY,
        }
    }
}

impl CoordinationCleanupRetryState {
    fn wait_for_next_tick(
        &self,
        cleanup_rx: &mpsc::Receiver<CoordinationCleanupRequest>,
    ) -> CoordinationCleanupWaitOutcome {
        if self.retry_pending {
            let deadline = std::time::Instant::now()
                .checked_add(self.retry_delay)
                .unwrap_or_else(std::time::Instant::now);
            loop {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    return CoordinationCleanupWaitOutcome::Process;
                }
                match cleanup_rx.recv_timeout(remaining) {
                    Ok(CoordinationCleanupRequest::Shutdown) => {
                        return CoordinationCleanupWaitOutcome::Shutdown;
                    }
                    Ok(CoordinationCleanupRequest::Process) => {
                        // The durable outbox remains level-triggered. Persist
                        // commits can therefore emit redundant Process wakes
                        // while a failed scope is backing off; consume those
                        // wakes without shortening the current retry window.
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        return CoordinationCleanupWaitOutcome::Process;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return CoordinationCleanupWaitOutcome::Shutdown;
                    }
                }
            }
        }
        match cleanup_rx.recv() {
            Ok(CoordinationCleanupRequest::Process) => CoordinationCleanupWaitOutcome::Process,
            Ok(CoordinationCleanupRequest::Shutdown) | Err(_) => {
                CoordinationCleanupWaitOutcome::Shutdown
            }
        }
    }

    fn record_pending(&mut self, pending: bool) {
        if pending {
            self.retry_pending = true;
            self.retry_delay = std::cmp::min(
                self.retry_delay * 2,
                COORDINATION_CLEANUP_RETRY_MAX_DELAY,
            );
        } else {
            self.retry_pending = false;
            self.retry_delay = COORDINATION_CLEANUP_RETRY_SEED_DELAY;
        }
    }
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

    fn should_exit_after_tick(&self, shutdown_requested: bool) -> bool {
        // A failed primary-state write must keep shutdown blocked so the last
        // mutation is not lost. Coordination cleanup runs on its own worker;
        // its unfinished outbox entries remain durable and never affect this
        // primary-state exit decision.
        shutdown_requested && !self.retry_after_failure
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct CoordinationCleanupPass {
    completed: usize,
    pending: bool,
}

fn process_pending_coordination_scope_deletions<F>(
    inner: &Arc<StateMutex<StateInner>>,
    mut delete_scope: F,
) -> CoordinationCleanupPass
where
    F: FnMut(&str) -> Result<bool>,
{
    let pending_scope_deletions = {
        inner
            .lock()
            .expect("state mutex poisoned")
            .pending_coordination_scope_deletions
            .iter()
            .cloned()
            .collect::<Vec<_>>()
    };
    let mut completed = Vec::new();
    for scope_project_id in pending_scope_deletions {
        match delete_scope(&scope_project_id) {
            Ok(_) => completed.push(scope_project_id),
            Err(err) => {
                let retryable = err
                    .downcast_ref::<CoordinationBoardStoreError>()
                    .is_some_and(|store_error| {
                        store_error.kind == CoordinationBoardStoreErrorKind::Retryable
                    });
                eprintln!(
                    "[termal] coordination cleanup for deleted project \
                     `{scope_project_id}` {} leaving its durable outbox item queued for a \
                     later cleanup pass: {err:#}",
                    if retryable {
                        "is temporarily busy;"
                    } else {
                        "failed;"
                    }
                );
            }
        }
    }
    let mut inner = inner.lock().expect("state mutex poisoned");
    let mut completed_count = 0;
    for scope_project_id in completed {
        if inner
            .pending_coordination_scope_deletions
            .remove(&scope_project_id)
        {
            completed_count += 1;
        }
    }
    CoordinationCleanupPass {
        completed: completed_count,
        pending: !inner.pending_coordination_scope_deletions.is_empty(),
    }
}

fn process_pending_response_board_project_detachments<F>(
    inner: &Arc<StateMutex<StateInner>>,
    mut detach_project_tab: F,
) -> CoordinationCleanupPass
where
    F: FnMut(&str, &str) -> std::result::Result<(), String>,
{
    let pending_detachments = {
        inner
            .lock()
            .expect("state mutex poisoned")
            .pending_response_board_project_detachments
            .iter()
            .map(|(project_id, last_project_name)| {
                (project_id.clone(), last_project_name.clone())
            })
            .collect::<Vec<_>>()
    };
    let mut completed = Vec::new();
    for (project_id, last_project_name) in pending_detachments {
        match detach_project_tab(&project_id, &last_project_name) {
            Ok(()) => completed.push((project_id, last_project_name)),
            Err(err) => {
                eprintln!(
                    "[termal] response-board tab detachment for deleted project \
                     `{project_id}` failed; leaving its durable outbox item queued for a \
                     later cleanup pass: {err}"
                );
            }
        }
    }
    let mut inner = inner.lock().expect("state mutex poisoned");
    let mut completed_count = 0;
    for (project_id, last_project_name) in completed {
        if inner
            .pending_response_board_project_detachments
            .get(&project_id)
            .is_some_and(|pending_name| pending_name == &last_project_name)
        {
            inner
                .pending_response_board_project_detachments
                .remove(&project_id);
            completed_count += 1;
        }
    }
    CoordinationCleanupPass {
        completed: completed_count,
        pending: !inner
            .pending_response_board_project_detachments
            .is_empty(),
    }
}

fn run_coordination_cleanup_worker(
    cleanup_rx: mpsc::Receiver<CoordinationCleanupRequest>,
    inner: Arc<StateMutex<StateInner>>,
    coordination_board_store: Arc<CoordinationBoardStore>,
    persistence_path: Arc<PathBuf>,
    persist_tx: mpsc::Sender<PersistRequest>,
) {
    let mut retry_state = CoordinationCleanupRetryState::default();
    loop {
        if matches!(
            retry_state.wait_for_next_tick(&cleanup_rx),
            CoordinationCleanupWaitOutcome::Shutdown
        ) {
            break;
        }
        // Coalesce redundant wake signals. Shutdown wins before any new
        // cleanup starts, so graceful shutdown never begins another possibly
        // large cascade while it is trying to join this worker.
        let mut shutdown_requested = false;
        while let Ok(request) = cleanup_rx.try_recv() {
            if matches!(request, CoordinationCleanupRequest::Shutdown) {
                shutdown_requested = true;
            }
        }
        if shutdown_requested {
            break;
        }

        let coordination_pass =
            process_pending_coordination_scope_deletions(&inner, |scope_project_id| {
            coordination_board_store.delete_scope_for_project_lifecycle(scope_project_id)
        });
        let response_board_pass = process_pending_response_board_project_detachments(
            &inner,
            |project_id, last_project_name| {
                convert_deleted_project_response_board_tab(
                    persistence_path.as_path(),
                    project_id,
                    last_project_name,
                )
                .map_err(|err| err.message)
            },
        );
        if coordination_pass.completed + response_board_pass.completed > 0
            && persist_tx.send(PersistRequest::Delta).is_err()
        {
            // The primary worker has stopped. The on-disk outbox deliberately
            // still contains these idempotently completed scopes, so the next
            // boot can replay them and durably clear the bookkeeping.
            break;
        }
        retry_state.record_pending(coordination_pass.pending || response_board_pass.pending);
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
        #[cfg(test)]
        TEST_ENGRAM_BOOT_TRANSPORT.with(|slot| {
            if let Some(transport) = slot.borrow().clone() {
                inner.engram_host_adapter = Arc::new(EngramHostAdapter { transport });
            }
        });
        let discovery_scopes = collect_codex_discovery_scopes(&default_workdir, &inner.projects);
        match discover_codex_threads(&default_workdir, &discovery_scopes) {
            Ok(discovery) => {
                let DiscoveredCodexThreads {
                    delegation_thread_ids,
                    threads,
                    mut subagent_thread_ids,
                } = discovery;
                subagent_thread_ids.extend(delegation_thread_ids);
                let removed_child_sessions =
                    inner.prune_auto_imported_codex_child_sessions(&subagent_thread_ids);
                if removed_child_sessions > 0 {
                    eprintln!(
                        "codex discovery> removed {removed_child_sessions} auto-imported child session(s)"
                    );
                }
                inner.import_discovered_codex_threads(&default_workdir, threads);
            }
            Err(err) => {
                eprintln!("codex discovery> failed to load Codex thread metadata: {err:#}");
            }
        }

        let agent_readiness_cache =
            Arc::new(RwLock::new(fresh_agent_readiness_cache(&default_workdir)));
        let (persist_tx, persist_rx) = mpsc::channel::<PersistRequest>();

        let persist_path_for_persist = Arc::new(persistence_path.clone());
        let persist_path_for_state = Arc::clone(&persist_path_for_persist);
        let coordination_path = resolve_coordination_persistence_path(&persistence_path);
        // Coordination bootstrap is a hard boot barrier: a fresh current schema
        // is initialized, or an existing database is validated, before either
        // store exists, before the persist worker starts, and therefore before
        // run_server can expose an HTTP listener. Legacy local schemas are
        // rejected with reset guidance rather than migrated.
        bootstrap_coordination_database(&coordination_path)?;
        let mailbox_store = Arc::new(MailboxStore::open(&coordination_path)?);
        // Mailboxes and the level-triggered board deliberately share the small
        // coordination database and its FIFO writer admission, while session
        // and transcript persistence remain isolated in termal.sqlite.
        let coordination_board_store =
            Arc::new(CoordinationBoardStore::open(&coordination_path)?);
        // `AppState::inner` is built here (rather than inside the struct
        // literal further below) so we can share an `Arc` clone with the
        // background persist thread. The thread briefly re-locks it on
        // each tick to collect the diff; see
        // `collect_persist_delta_from_shared_state`.
        let inner_arc = Arc::new(StateMutex::new(inner));
        let inner_for_persist = Arc::clone(&inner_arc);
        let inner_for_coordination_cleanup = Arc::clone(&inner_arc);
        let coordination_board_store_for_persist = Arc::clone(&coordination_board_store);
        let coordination_board_store_for_cleanup = Arc::clone(&coordination_board_store);
        let persist_path_for_coordination_cleanup = Arc::clone(&persist_path_for_persist);
        let persist_tx_for_coordination_cleanup = persist_tx.clone();
        let (coordination_cleanup_tx, coordination_cleanup_rx) =
            mpsc::channel::<CoordinationCleanupRequest>();
        let (coordination_cleanup_done_tx, coordination_cleanup_done_rx) = mpsc::channel::<()>();
        // Scope cascades run outside both boot and the primary persistence
        // worker. The project-removal outbox is already durable before this
        // worker is signaled, and a missing project cannot authorize new board
        // access while cleanup is pending. A large scope or a backlog of
        // scopes therefore cannot delay termal.sqlite persistence or opening
        // the HTTP listener.
        let coordination_cleanup_thread_handle = std::thread::Builder::new()
            .name("termal-coordination-cleanup".to_owned())
            .spawn(move || {
                run_coordination_cleanup_worker(
                    coordination_cleanup_rx,
                    inner_for_coordination_cleanup,
                    coordination_board_store_for_cleanup,
                    persist_path_for_coordination_cleanup,
                    persist_tx_for_coordination_cleanup,
                );
                let _ = coordination_cleanup_done_tx.send(());
            })
            .expect("failed to spawn coordination cleanup thread");

        // Background persist thread: drains `PersistRequest::Delta`
        // wake signals and writes the accumulated diff to SQLite.
        //
        // On each signal the thread captures a lightweight plan under
        // `inner_for_persist`, then snapshots each selected session/delegation
        // in a separate lock acquisition. This prevents a batch of large
        // transcripts from becoming one application-wide lock hold. It then
        // writes the delta with targeted
        // `INSERT OR UPDATE` per changed session and
        // `DELETE WHERE id = ?` per removed id. No `DELETE FROM sessions`
        // sweep is issued — unchanged rows stay untouched. See
        // `state.rs::PersistDeltaPlan` +
        // `collect_persist_delta_from_shared_state`
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
                let mut cache = SqlitePersistConnectionCache::new();
                let mut watermark: u64 = 0;
                let mut prompt_history_carry = BTreeSet::new();
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

                    let result: Result<()> = (|| {
                        let delta = collect_persist_delta_from_shared_state_with_prompt_history_carry(
                            &inner_for_persist,
                            watermark,
                            &prompt_history_carry,
                        );
                        let next_watermark = delta.watermark;
                        let next_prompt_history_carry = delta
                            .deferred_prompt_history_session_ids
                            .iter()
                            .cloned()
                            .collect::<BTreeSet<_>>();
                        let pending_scope_deletions = delta
                            .metadata
                            .pending_coordination_scope_deletions
                            .iter()
                            .cloned()
                            .collect::<Vec<_>>();
                        let has_pending_response_board_detachments = !delta
                            .metadata
                            .pending_response_board_project_detachments
                            .is_empty();
                        // Always upsert metadata (revision, preferences,
                        // projects, orchestrators, workspace_layouts).
                        // Mutation stamps only cover per-session changes,
                        // but commit_locked bumps `inner.revision` which
                        // must reach SQLite, and non-session fields can
                        // change without any session stamp moving. Empty
                        // `changed_sessions` + `removed_session_ids` is
                        // fine; the transaction just upserts one
                        // app_state row.
                        let persisted_session_ids = match persist_delta_via_cache(
                            &mut cache,
                            &persist_path_for_persist,
                            &delta,
                        ) {
                            Ok(persisted_session_ids) => persisted_session_ids,
                            Err(err) => {
                                // On write failure, restore only the drained
                                // explicit `removed_session_ids` into `inner` so
                                // the next tick can retry the tombstones.
                                // Without this, a transient SQLite error
                                // (locked DB, disk full, I/O error) would
                                // silently leak an orphan `sessions` row
                                // into SQLite — persist-delta planning
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
                        };
                        {
                            let mut inner =
                                inner_for_persist.lock().expect("state mutex poisoned");
                            inner.trim_persisted_session_tails(
                                next_watermark,
                                &persisted_session_ids,
                            );
                        }
                        watermark = next_watermark;
                        prompt_history_carry = next_prompt_history_carry;
                        if !pending_scope_deletions.is_empty()
                            || has_pending_response_board_detachments
                        {
                            // Only a successful primary commit authorizes
                            // secondary cleanup. The dedicated worker removes
                            // completed outbox items in memory and wakes this
                            // worker again to persist that bookkeeping.
                            let _ = coordination_cleanup_tx
                                .send(CoordinationCleanupRequest::Process);
                        }
                        Ok(())
                    })();

                    if let Err(err) = &result {
                        eprintln!("[termal] background persist failed: {err:#}");
                    }
                    retry_state.record_result(&result);
                    if retry_state.should_exit_after_tick(should_exit_after_tick) {
                        break;
                    }
                }
                let _ = coordination_cleanup_tx.send(CoordinationCleanupRequest::Shutdown);
                // A scope cascade already in progress must not make graceful
                // shutdown wait indefinitely. Keep interrupting until the
                // worker reports completion (or its completion sender drops
                // after a panic); the short repeated check also closes the
                // race where the first interrupt lands immediately before a
                // SQLite statement starts. Then join so no cleanup thread
                // outlives its temp paths or AppState on Windows.
                loop {
                    coordination_board_store_for_persist.interrupt_current_operation();
                    match coordination_cleanup_done_rx.recv_timeout(Duration::from_millis(5)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
                if let Err(err) = coordination_cleanup_thread_handle.join() {
                    eprintln!(
                        "[termal] coordination cleanup worker join failed during shutdown: {err:?}"
                    );
                }
            })
            .expect("failed to spawn persist thread");

        let state_events_sender = broadcast::channel::<String>(128).0;
        let state_broadcast_mailbox = Arc::new(StateBroadcastMailbox::default());

        // Background state-broadcast thread: drains a bounded ordered mailbox
        // of state snapshots and delta payloads, serializes snapshots to JSON,
        // and forwards each payload to the matching SSE broadcast channel.
        // Consecutive state snapshots coalesce before they reach this thread,
        // but a snapshot queued before a delta must be sent before that delta;
        // otherwise the browser can see delta N+1 while still waiting for
        // state N and trigger an avoidable `/api/state` repair fetch. If a
        // large snapshot stalls this thread, the mailbox drops the oldest
        // pending work at capacity and clients repair any revision gap through
        // the existing `/api/state` recovery path.
        let state_events_for_broadcast = state_events_sender.clone();
        let delta_events_sender = broadcast::channel(256).0;
        let delta_events_for_broadcast = delta_events_sender.clone();
        let state_broadcast_mailbox_for_thread = state_broadcast_mailbox.clone();
        std::thread::Builder::new()
            .name("termal-state-broadcast".to_owned())
            .spawn(move || {
                loop {
                    forward_state_broadcast_work(
                        state_broadcast_mailbox_for_thread.recv_next(),
                        &state_events_for_broadcast,
                        &delta_events_for_broadcast,
                    );
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
            local_http_base_url: Arc::new(Mutex::new(None)),
            persistence_path: persist_path_for_state,
            mailbox_store,
            coordination_board_store,
            orchestrator_templates_path: Arc::new(orchestrator_templates_path),
            orchestrator_templates_lock: Arc::new(Mutex::new(())),
            review_documents_lock: Arc::new(Mutex::new(())),
            state_events: state_events_sender,
            delta_events: delta_events_sender,
            file_events: broadcast::channel(256).0,
            file_events_revision: Arc::new(AtomicU64::new(0)),
            persist_tx,
            persist_thread_handle: Arc::new(Mutex::new(Some(persist_thread_handle))),
            persist_worker_alive: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            shutdown_signal_tx: Arc::new(tokio::sync::watch::channel(false).0),
            state_broadcast_mailbox: Some(state_broadcast_mailbox),
            telegram_relay_runtime: Arc::new(Mutex::new(TelegramRelayRuntime::default())),
            shared_codex_runtime: Arc::new(Mutex::new(None)),
            shared_codex_exit_claims: Arc::new(Mutex::new(HashSet::new())),
            agent_runtime_spawning_enabled: true,
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
            remote_lifecycle_actions_in_flight: Arc::new(Mutex::new(HashSet::new())),
            terminal_local_command_semaphore: Arc::new(tokio::sync::Semaphore::new(
                TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT,
            )),
            terminal_remote_command_semaphore: Arc::new(tokio::sync::Semaphore::new(
                TERMINAL_REMOTE_COMMAND_CONCURRENCY_LIMIT,
            )),
            stopping_orchestrator_ids: Arc::new(Mutex::new(HashSet::new())),
            stopping_orchestrator_session_ids: Arc::new(Mutex::new(HashMap::new())),
            inner: inner_arc,
            #[cfg(test)]
            test_temp_root: None,
        };
        let remote_config_publication = {
            let inner = state.inner.lock().expect("state mutex poisoned");
            // Persist first so a boot failure cannot abandon a publication
            // before its retired-connection teardown phase. The registry is
            // new here, but preserving the publish/finish invariant keeps this
            // path safe if boot ever seeds connections earlier.
            state.persist_internal_locked(&inner)?;
            // Seed settings-owned registry authority before any restored bridge
            // can issue a remote request after restart.
            state.remote_registry.publish_configs(&inner.preferences.remotes)
        };
        let bridges_to_restart = state
            .remote_registry
            .finish_config_publication(remote_config_publication);
        if !bridges_to_restart.is_empty() {
            eprintln!(
                "[termal] boot remote registry unexpectedly returned bridge restarts before restore: {}",
                bridges_to_restart.join(", ")
            );
            for remote_id in bridges_to_restart {
                state.start_remote_event_bridge_by_id(&remote_id);
            }
        }
        state.restore_remote_event_bridges();
        #[cfg(not(test))]
        state.spawn_workspace_file_watcher();
        // Runtime-resuming boot work is deferred to `run_post_listen_boot`, invoked by
        // `run_server` only AFTER the HTTP listener is bound and the base URL is published.
        // Resuming a Codex session launches the shared app-server, which bakes a TermAl MCP
        // bridge config from that URL and connects back to this backend. Running it
        // pre-listen points the bridge at an unlistening backend. Tests have no HTTP server
        // or real runtimes, so they run it inline here to preserve behavior.
        #[cfg(test)]
        state.run_post_listen_boot()?;
        Ok(state)
    }

    /// Boot work that RESUMES or SPAWNS session runtimes. MUST run only after the HTTP
    /// listener is bound and `set_local_http_base_url` has published the real address, so the
    /// TermAl MCP bridges these runtimes spawn are configured with the correct base URL and
    /// can reach a listening backend. Each step self-persists / self-commits, so
    /// deferring this work past the boot persist above is safe.
    #[cfg(test)]
    fn run_post_listen_boot(&self) -> Result<()> {
        let engram_plan = self.prepare_engram_sessions_for_boot_recovery()?;
        self.run_post_listen_boot_after_readiness(engram_plan);
        Ok(())
    }

    /// Marks every affected session as recovering synchronously, then runs the
    /// expensive recovery sequence on a detached worker while Axum serves.
    fn start_post_listen_boot(&self) -> Result<std::thread::JoinHandle<()>> {
        let engram_plan = self.prepare_engram_sessions_for_boot_recovery()?;
        let state = self.clone();
        std::thread::Builder::new()
            .name("termal-post-listen-boot".to_owned())
            .spawn(move || state.run_post_listen_boot_after_readiness(engram_plan))
            .context("failed spawning post-listen boot recovery worker")
    }

    fn run_post_listen_boot_after_readiness(&self, engram_plan: EngramBootRecoveryPlan) {
        // Structured reviewer envelopes live in coordination.sqlite. Recover
        // them before wait reconciliation so a restart between mailbox append
        // and primary-state recording cannot resume a parent with a false
        // unavailable result.
        self.reconcile_durable_delegation_review_submissions_after_boot();
        // Engram routing tokens and open grants are host-private session
        // state. Recover them before any mailbox/workflow pass can dispatch a
        // prompt. Recovery runs in bounded batches while HTTP remains live;
        // the per-session readiness fence blocks only affected sessions until
        // their own recovery result is committed.
        self.recover_prepared_engram_sessions_after_boot(engram_plan);
        // Materialize unread mailbox wakes before either workflow reconciler can
        // queue and dispatch a durable resume. That preserves FIFO ordering:
        // the workflow activation drains the recovered mailbox wake first.
        self.reconcile_unread_mailbox_wakeups_after_boot();
        if let Err(err) = self.reconcile_delegation_waits_after_boot() {
            eprintln!("delegation wait> failed reconciling pending waits after boot: {err:#}");
        }
        if let Err(err) = self.resume_pending_orchestrator_transitions() {
            eprintln!("orchestrator> failed resuming pending transitions: {err:#}");
        }
        self.dispatch_orphaned_workflow_prompts();
    }
}
