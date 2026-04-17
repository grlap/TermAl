/*
State and persistence core
                 +------------------------+
REST / runtimes ->| AppState              |-> state_events
remote bridge  -> | - coordination shell  |-> delta_events
                  | - remote registry      |
                  | - shared Codex runtime |
                  +-----------+------------+
                              |
                              v
                       +------+------+
                       | StateInner  |
                       | projects    |
                       | sessions    |
                       | orchestrators
                       | workspaces  |
                       +------+------+
                              |
                              v
                    ~/.termal/sessions.json
AppState owns live coordination primitives that should not be serialized.
StateInner is the durable model plus counters and indexes protected by one
mutex.
*/

/// Tracks app state.
/// Signals a pending persist.
///
/// The background persist thread owns an `Arc<Mutex<StateInner>>` and
/// collects the diff itself on each tick — it locks briefly, filters
/// sessions by `mutation_stamp > watermark`, clones only that subset
/// plus app metadata, drains `removed_session_ids`, then releases the
/// lock and writes to SQLite. `PersistRequest` therefore carries only
/// the wake signal; the full `PersistedState` snapshot that earlier
/// versions cloned under the state mutex is no longer needed.
///
/// Kept as a unit-only enum (rather than `()`) so a future reset /
/// restore flow can add variants without touching every call site.
enum PersistRequest {
    /// Incremental persist: the thread looks up the current
    /// `last_mutation_stamp` and writes only the sessions that
    /// advanced past the thread's own watermark.
    Delta,
}

/// The diff the persist thread writes on each tick.
///
/// Built inside `AppState::inner` by
/// [`StateInner::collect_persist_delta`]. `changed_sessions` is the
/// subset of sessions whose `mutation_stamp` advanced past the
/// thread's watermark; `removed_session_ids` is the union of explicit
/// removals and sessions that flipped to hidden since the last
/// persist. The persist thread then writes the delta to SQLite with
/// a targeted `INSERT OR UPDATE` per changed session and a targeted
/// `DELETE WHERE id = ?` per removed id — no `DELETE FROM sessions`
/// sweep.
#[cfg_attr(test, allow(dead_code))]
struct PersistDelta {
    metadata: PersistedState,
    changed_sessions: Vec<PersistedSessionRecord>,
    removed_session_ids: Vec<String>,
    watermark: u64,
}

#[derive(Clone)]
struct AppState {
    default_workdir: String,
    persistence_path: Arc<PathBuf>,
    orchestrator_templates_path: Arc<PathBuf>,
    /// Must not be held at the same time as `self.inner`; template file I/O happens
    /// outside the main state mutex so we never invert lock ordering.
    orchestrator_templates_lock: Arc<Mutex<()>>,
    /// Must not be held at the same time as `self.inner`; review file I/O stays
    /// outside the main state mutex so disk writes do not stall unrelated state work.
    review_documents_lock: Arc<Mutex<()>>,
    state_events: broadcast::Sender<String>,
    delta_events: broadcast::Sender<String>,
    file_events: broadcast::Sender<String>,
    #[cfg_attr(test, allow(dead_code))]
    file_events_revision: Arc<AtomicU64>,
    /// Background persistence channel. `persist_internal_locked` sends a
    /// pre-cloned `PersistedState` snapshot through this channel; a
    /// dedicated thread serializes it to JSON and writes the file so the
    /// state mutex is never held during I/O.
    persist_tx: mpsc::Sender<PersistRequest>,
    /// Background SSE state-broadcast channel. `publish_snapshot` sends a
    /// pre-built `StateResponse` through this channel; a dedicated thread
    /// serializes it to JSON and forwards the payload to `state_events`,
    /// so the state mutex is never held during the O(sessions × messages)
    /// serialization pass. The broadcaster coalesces queued snapshots to
    /// the newest, which is safe because state events are idempotent
    /// full-state snapshots — subscribers always converge on the latest
    /// revision either way, and delta events (`publish_delta`) still fire
    /// in order for every revision via a separate channel.
    state_broadcast_tx: mpsc::Sender<StateResponse>,
    /// Lazily created shared Codex app-server reused across Codex sessions.
    shared_codex_runtime: Arc<Mutex<Option<SharedCodexRuntime>>>,
    /// Cached app-level agent readiness lives outside `self.inner` so full
    /// snapshots can clone the latest value without filesystem work under the
    /// main state mutex.
    agent_readiness_cache: Arc<RwLock<AgentReadinessCache>>,
    /// Serializes cache refreshes so concurrent snapshot requests do not all
    /// repeat the same readiness filesystem probes.
    agent_readiness_refresh_lock: Arc<Mutex<()>>,
    /// Owns SSH-backed remote connections and their event bridges.
    remote_registry: Arc<RemoteRegistry>,
    /// Tracks the newest `_sseFallback` revision already recovered per remote
    /// so duplicate or older fallback events do not trigger redundant
    /// blocking `/api/state` fetches.
    remote_sse_fallback_resynced_revision: Arc<Mutex<HashMap<String, u64>>>,
    terminal_local_command_semaphore: Arc<tokio::sync::Semaphore>,
    terminal_remote_command_semaphore: Arc<tokio::sync::Semaphore>,
    stopping_orchestrator_ids: Arc<Mutex<HashSet<String>>>,
    stopping_orchestrator_session_ids: Arc<Mutex<HashMap<String, HashSet<String>>>>,
    inner: Arc<Mutex<StateInner>>,
}

const SESSION_NOT_RUNNING_CONFLICT_MESSAGE: &str = "session is not currently running";
const TERMINAL_LOCAL_COMMAND_CONCURRENCY_LIMIT: usize = 4;
const TERMINAL_REMOTE_COMMAND_CONCURRENCY_LIMIT: usize = 4;
const AGENT_READINESS_CACHE_TTL: Duration = Duration::from_secs(5);
const ACTIVE_TURN_FILE_CHANGE_GRACE: Duration = Duration::from_millis(750);

#[derive(Clone)]
struct AgentReadinessCache {
    snapshot: Vec<AgentReadiness>,
    expires_at: std::time::Instant,
    invalidated: bool,
}

impl AgentReadinessCache {
    fn fresh(snapshot: Vec<AgentReadiness>) -> Self {
        Self {
            snapshot,
            expires_at: std::time::Instant::now() + AGENT_READINESS_CACHE_TTL,
            invalidated: false,
        }
    }

    fn needs_refresh(&self, now: std::time::Instant) -> bool {
        self.invalidated || now >= self.expires_at
    }
}

fn fresh_agent_readiness_cache(default_workdir: &str) -> AgentReadinessCache {
    AgentReadinessCache::fresh(collect_agent_readiness(default_workdir))
}


/// Holds stop session options.
#[derive(Clone)]
struct StopSessionOptions {
    dispatch_queued_prompts_on_success: bool,
    orchestrator_stop_instance_id: Option<String>,
}
impl Default for StopSessionOptions {
    /// Builds the default value.
    fn default() -> Self {
        Self {
            dispatch_queued_prompts_on_success: true,
            orchestrator_stop_instance_id: None,
        }
    }
}

/// Handles bootstrap default local state.
fn bootstrap_default_local_state(default_workdir: &str) -> StateInner {
    let mut inner = StateInner::new();
    let default_project =
        inner.create_project(None, default_workdir.to_owned(), default_local_remote_id());
    inner.create_session(
        Agent::Codex,
        Some("Codex Live".to_owned()),
        default_workdir.to_owned(),
        Some(default_project.id.clone()),
        None,
    );
    inner.create_session(
        Agent::Claude,
        Some("Claude Live".to_owned()),
        default_workdir.to_owned(),
        Some(default_project.id.clone()),
        None,
    );
    inner
}

/// Describes whether a runtime-gated mutation actually applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
enum RuntimeMatchOutcome {
    Applied,
    SessionMissing,
    RuntimeMismatch,
}

impl AppState {
    /// Creates a new instance.
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

    /// Handles new with paths.
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

    /// Builds a full state snapshot with guaranteed-fresh agent readiness.
    ///
    /// The cache is refreshed (filesystem I/O) *before* locking `inner`, then
    /// the snapshot reads `cached_agent_readiness()` *under* the `inner` lock —
    /// the same path used by `commit_locked` / `publish_state_locked`.  This
    /// ensures that a `snapshot()` call at revision N uses the same cached
    /// readiness value that was published in the SSE event for revision N.
    fn snapshot(&self) -> StateResponse {
        let _ = self.agent_readiness_snapshot();
        let inner = self.inner.lock().expect("state mutex poisoned");
        self.snapshot_from_inner(&inner)
    }

    fn agent_readiness_snapshot(&self) -> Vec<AgentReadiness> {
        if let Some(snapshot) = self.cached_agent_readiness_if_fresh() {
            return snapshot;
        }

        let _refresh_lock = self
            .agent_readiness_refresh_lock
            .lock()
            .expect("agent readiness refresh mutex poisoned");
        if let Some(snapshot) = self.cached_agent_readiness_if_fresh() {
            return snapshot;
        }

        let snapshot = collect_agent_readiness(&self.default_workdir);
        let mut cache = self
            .agent_readiness_cache
            .write()
            .expect("agent readiness cache poisoned");
        *cache = AgentReadinessCache::fresh(snapshot);
        cache.snapshot.clone()
    }

    fn cached_agent_readiness_if_fresh(&self) -> Option<Vec<AgentReadiness>> {
        let cache = self
            .agent_readiness_cache
            .read()
            .expect("agent readiness cache poisoned");
        let now = std::time::Instant::now();
        (!cache.needs_refresh(now)).then(|| cache.snapshot.clone())
    }

    fn cached_agent_readiness(&self) -> Vec<AgentReadiness> {
        self.agent_readiness_cache
            .read()
            .expect("agent readiness cache poisoned")
            .snapshot
            .clone()
    }

    /// Returns one visible session with its state revision.
    fn get_session(&self, session_id: &str) -> Result<SessionResponse, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        Ok(SessionResponse {
            revision: inner.revision,
            session: inner.sessions[index].session.clone(),
        })
    }

    fn invalidate_agent_readiness_cache(&self) {
        let _refresh_lock = self
            .agent_readiness_refresh_lock
            .lock()
            .expect("agent readiness refresh mutex poisoned");
        self.agent_readiness_cache
            .write()
            .expect("agent readiness cache poisoned")
            .invalidated = true;
    }

    /// Returns whether a remote fallback-driven /api/state resync can be
    /// skipped because the same or a newer fallback revision was already
    /// recovered for that remote within the current event-stream lifetime.
    fn should_skip_remote_sse_fallback_resync(
        &self,
        remote_id: &str,
        fallback_revision: u64,
    ) -> bool {
        self.remote_sse_fallback_resynced_revision
            .lock()
            .expect("remote fallback resync mutex poisoned")
            .get(remote_id)
            .is_some_and(|last_revision| *last_revision >= fallback_revision)
    }

    /// Records that a remote fallback-driven /api/state resync recovered the
    /// given fallback revision.
    fn note_remote_sse_fallback_resync(&self, remote_id: &str, fallback_revision: u64) {
        self.remote_sse_fallback_resynced_revision
            .lock()
            .expect("remote fallback resync mutex poisoned")
            .entry(remote_id.to_owned())
            .and_modify(|last_revision| {
                *last_revision = (*last_revision).max(fallback_revision);
            })
            .or_insert(fallback_revision);
    }

    /// Clears the latest applied remote revision when event-stream continuity
    /// is lost, such as after a disconnect or restart.
    fn clear_remote_applied_revision(&self, remote_id: &str) {
        self.inner
            .lock()
            .expect("state mutex poisoned")
            .remote_applied_revisions
            .remove(remote_id);
    }

    /// Clears remote fallback resync tracking when event-stream continuity is
    /// lost, such as after a disconnect or restart.
    fn clear_remote_sse_fallback_resync(&self, remote_id: &str) {
        self.remote_sse_fallback_resynced_revision
            .lock()
            .expect("remote fallback resync mutex poisoned")
            .remove(remote_id);
    }

    /// Lists workspace layouts.
    fn list_workspace_layouts(&self) -> Result<WorkspaceLayoutsResponse, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        Ok(WorkspaceLayoutsResponse {
            workspaces: collect_workspace_layout_summaries(inner.workspace_layouts.values()),
        })
    }

    /// Gets workspace layout.
    fn get_workspace_layout(
        &self,
        workspace_id: &str,
    ) -> Result<WorkspaceLayoutResponse, ApiError> {
        let workspace_id = normalize_optional_identifier(Some(workspace_id))
            .ok_or_else(|| ApiError::bad_request("workspace id is required"))?;
        let inner = self.inner.lock().expect("state mutex poisoned");
        let layout = inner
            .workspace_layouts
            .get(workspace_id)
            .cloned()
            .ok_or_else(|| ApiError::not_found("workspace layout not found"))?;
        Ok(WorkspaceLayoutResponse { layout })
    }

    /// Stores workspace layout.
    fn put_workspace_layout(
        &self,
        workspace_id: &str,
        request: PutWorkspaceLayoutRequest,
    ) -> Result<WorkspaceLayoutResponse, ApiError> {
        let workspace_id = normalize_optional_identifier(Some(workspace_id))
            .ok_or_else(|| ApiError::bad_request("workspace id is required"))?
            .to_owned();
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let existing_layout = inner.workspace_layouts.get(&workspace_id).cloned();
        let next_revision = existing_layout
            .as_ref()
            .map(|layout| layout.revision.saturating_add(1))
            .unwrap_or(1);
        let layout = WorkspaceLayoutDocument {
            id: workspace_id.clone(),
            revision: next_revision,
            updated_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            control_panel_side: request.control_panel_side,
            theme_id: request.theme_id,
            style_id: request.style_id,
            font_size_px: request.font_size_px,
            editor_font_size_px: request.editor_font_size_px,
            density_percent: request.density_percent,
            workspace: request.workspace,
        };
        inner.workspace_layouts.insert(workspace_id, layout.clone());
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!(
                "failed to persist workspace layout update: {err:#}"
            ))
        })?;
        Ok(WorkspaceLayoutResponse { layout })
    }

    /// Deletes workspace layout.
    fn delete_workspace_layout(
        &self,
        workspace_id: &str,
    ) -> Result<WorkspaceLayoutsResponse, ApiError> {
        let workspace_id = normalize_optional_identifier(Some(workspace_id))
            .ok_or_else(|| ApiError::bad_request("workspace id is required"))?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if inner.workspace_layouts.remove(workspace_id).is_none() {
            return Err(ApiError::not_found("workspace layout not found"));
        }
        let workspaces = collect_workspace_layout_summaries(inner.workspace_layouts.values());
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!(
                "failed to persist workspace layout deletion: {err:#}"
            ))
        })?;
        Ok(WorkspaceLayoutsResponse { workspaces })
    }

    /// Lists agent commands.
    fn list_agent_commands(
        &self,
        session_id: &str,
    ) -> std::result::Result<AgentCommandsResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_list_agent_commands(session_id);
        }

        let (session, cached_agent_commands) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            (
                inner.sessions[index].session.clone(),
                inner.sessions[index].agent_commands.clone(),
            )
        };

        let filesystem_commands = read_claude_agent_commands(FsPath::new(&session.workdir))?;
        let commands = if session.agent == Agent::Claude {
            merge_agent_commands(&cached_agent_commands, &filesystem_commands)
        } else {
            filesystem_commands
        };

        Ok(AgentCommandsResponse { commands })
    }

    /// Searches instructions.
    fn search_instructions(
        &self,
        session_id: &str,
        query: &str,
    ) -> std::result::Result<InstructionSearchResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_search_instructions(session_id, query);
        }

        let session = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            inner.sessions[index].session.clone()
        };

        search_instruction_phrase(FsPath::new(&session.workdir), query)
    }

    /// Creates session.
    fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<CreateSessionResponse, ApiError> {
        let agent = request.agent.unwrap_or(Agent::Codex);
        let requested_workdir = request
            .workdir
            .as_deref()
            .map(resolve_session_workdir)
            .transpose()?;
        let requested_model = request
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let requested_name = request
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let project = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if let Some(project_id) = request.project_id.as_deref() {
                Some(inner.find_project(project_id).cloned().ok_or_else(|| {
                    ApiError::bad_request(format!("unknown project `{project_id}`"))
                })?)
            } else {
                requested_workdir
                    .as_deref()
                    .and_then(|workdir| inner.find_project_for_workdir(workdir).cloned())
            }
        };
        let workdir = requested_workdir.unwrap_or_else(|| {
            project
                .as_ref()
                .map(|entry| entry.root_path.clone())
                .unwrap_or_else(|| self.default_workdir.clone())
        });
        if let Some(project) = project.as_ref() {
            if project.remote_id != LOCAL_REMOTE_ID {
                return self.create_remote_session_proxy(request, project.clone());
            }
            if !path_contains(&project.root_path, FsPath::new(&workdir)) {
                return Err(ApiError::bad_request(format!(
                    "session workdir `{workdir}` must stay inside project `{}`",
                    project.name
                )));
            }
        }
        validate_agent_session_setup(agent, &workdir).map_err(ApiError::bad_request)?;
        // Refresh the agent readiness cache before the critical section so that
        // commit_locked's SSE publish and the API response snapshot both carry
        // up-to-date readiness without filesystem I/O under the inner mutex.
        self.invalidate_agent_readiness_cache();
        let _ = self.agent_readiness_snapshot();
        match agent {
            agent if agent.supports_codex_prompt_settings() => {
                if request.claude_approval_mode.is_some()
                    || request.claude_effort.is_some()
                    || request.cursor_mode.is_some()
                    || request.gemini_approval_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Codex sessions only support model, sandbox, approval policy, and reasoning effort settings",
                    ));
                }
            }
            agent if agent.supports_claude_approval_mode() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.cursor_mode.is_some()
                    || request.gemini_approval_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Claude sessions only support model, mode, and effort settings",
                    ));
                }
            }
            agent if agent.supports_cursor_mode() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.claude_approval_mode.is_some()
                    || request.claude_effort.is_some()
                    || request.gemini_approval_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Cursor sessions only support mode settings",
                    ));
                }
            }
            agent if agent.supports_gemini_approval_mode() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.claude_approval_mode.is_some()
                    || request.claude_effort.is_some()
                    || request.cursor_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Gemini sessions only support approval mode settings",
                    ));
                }
            }
            _ => {}
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let project_id = project.as_ref().map(|entry| entry.id.clone());
        let mut hidden_claude_spare_to_spawn = None;
        let mut record = if agent == Agent::Claude {
            let final_model = requested_model
                .clone()
                .unwrap_or_else(|| agent.default_model().to_owned());
            let final_approval_mode = request
                .claude_approval_mode
                .unwrap_or(inner.preferences.default_claude_approval_mode);
            let final_effort = request
                .claude_effort
                .unwrap_or(inner.preferences.default_claude_effort);
            if let Some(index) = inner.find_matching_hidden_claude_spare(
                &workdir,
                project_id.as_deref(),
                &final_model,
                final_approval_mode,
                final_effort,
            ) {
                let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
                // Hidden Claude spares intentionally keep their warmed runtime alive when claimed.
                // Only the visible conversation state is reset here before the session is unhidden.
                reset_hidden_claude_spare_record(record);
                record.hidden = false;
                if let Some(name) = requested_name.clone() {
                    record.session.name = name;
                }
                record.clone()
            } else {
                inner.create_session(
                    agent,
                    requested_name.clone(),
                    workdir.clone(),
                    project_id.clone(),
                    requested_model.clone(),
                )
            }
        } else {
            inner.create_session(
                agent,
                requested_name.clone(),
                workdir.clone(),
                project_id.clone(),
                requested_model.clone(),
            )
        };
        if record.session.agent.supports_codex_prompt_settings() {
            if let Some(sandbox_mode) = request.sandbox_mode {
                record.codex_sandbox_mode = sandbox_mode;
                record.session.sandbox_mode = Some(sandbox_mode);
            }
            if let Some(approval_policy) = request.approval_policy {
                record.codex_approval_policy = approval_policy;
                record.session.approval_policy = Some(approval_policy);
            }
            if let Some(reasoning_effort) = request.reasoning_effort {
                record.codex_reasoning_effort = reasoning_effort;
                record.session.reasoning_effort = Some(reasoning_effort);
            }
        } else if record.session.agent.supports_claude_approval_mode() {
            if let Some(claude_approval_mode) = request.claude_approval_mode {
                record.session.claude_approval_mode = Some(claude_approval_mode);
            }
            if let Some(claude_effort) = request.claude_effort {
                record.session.claude_effort = Some(claude_effort);
            }
        } else if record.session.agent.supports_cursor_mode() {
            if let Some(cursor_mode) = request.cursor_mode {
                record.session.cursor_mode = Some(cursor_mode);
            }
        } else if record.session.agent.supports_gemini_approval_mode() {
            if let Some(gemini_approval_mode) = request.gemini_approval_mode {
                record.session.gemini_approval_mode = Some(gemini_approval_mode);
            }
        }
        if agent == Agent::Claude {
            hidden_claude_spare_to_spawn = inner.ensure_hidden_claude_spare(
                workdir.clone(),
                project_id.clone(),
                record.session.model.clone(),
                record
                    .session
                    .claude_approval_mode
                    .unwrap_or_else(default_claude_approval_mode),
                record
                    .session
                    .claude_effort
                    .unwrap_or_else(default_claude_effort),
            );
        }
        if let Some(index) = inner.find_session_index(&record.session.id) {
            if let Some(slot) = inner.sessions.get_mut(index) {
                *slot = record.clone();
            }
            // The whole-struct replace above clobbered the stamp that
            // `push_session` assigned; re-stamp via `session_mut_by_index`
            // so `collect_persist_delta` picks up this rewrite on the
            // next persist tick. The local `record` carries
            // `mutation_stamp: 0` from construction, so skipping this
            // call would leave the row below the persist watermark.
            let _ = inner.session_mut_by_index(index);
        }
        let revision = self.commit_session_created_locked(&mut inner, &record)
            .map_err(|err| ApiError::internal(format!("failed to persist session: {err:#}")))?;
        let session = record.session.clone();
        drop(inner);
        self.publish_delta(&DeltaEvent::SessionCreated {
            revision,
            session_id: session.id.clone(),
            session: session.clone(),
        });
        if let Some(session_id) = hidden_claude_spare_to_spawn {
            self.try_start_hidden_claude_spare(&session_id);
        }
        Ok(CreateSessionResponse {
            session_id: session.id.clone(),
            session: Some(session),
            revision,
            state: None,
        })
    }

    /// Updates app settings.
    fn update_app_settings(
        &self,
        request: UpdateAppSettingsRequest,
    ) -> Result<StateResponse, ApiError> {
        // Normalize remotes outside the lock — pure validation on request data.
        let normalized_remotes = request.remotes.map(normalize_remote_configs).transpose()?;

        // Refresh the agent readiness cache before the critical section so that
        // commit_locked's SSE publish and the API response snapshot both carry
        // up-to-date readiness without filesystem I/O under the inner mutex.
        self.invalidate_agent_readiness_cache();
        let _ = self.agent_readiness_snapshot();

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let mut changed = false;

        if let Some(default_codex_reasoning_effort) = request.default_codex_reasoning_effort {
            if inner.preferences.default_codex_reasoning_effort != default_codex_reasoning_effort {
                inner.preferences.default_codex_reasoning_effort = default_codex_reasoning_effort;
                changed = true;
            }
        }

        if let Some(default_claude_approval_mode) = request.default_claude_approval_mode {
            if inner.preferences.default_claude_approval_mode != default_claude_approval_mode {
                inner.preferences.default_claude_approval_mode = default_claude_approval_mode;
                changed = true;
            }
        }

        if let Some(default_claude_effort) = request.default_claude_effort {
            if inner.preferences.default_claude_effort != default_claude_effort {
                inner.preferences.default_claude_effort = default_claude_effort;
                changed = true;
            }
        }

        let mut next_remotes: Option<Vec<RemoteConfig>> = None;
        if let Some(normalized_remotes) = normalized_remotes {
            let next_remote_ids: HashSet<&str> = normalized_remotes
                .iter()
                .map(|remote| remote.id.as_str())
                .collect();
            if let Some(project) = inner
                .projects
                .iter()
                .find(|project| !next_remote_ids.contains(project.remote_id.as_str()))
            {
                return Err(ApiError::bad_request(format!(
                    "cannot remove remote `{}` because project `{}` still uses it",
                    project.remote_id, project.name
                )));
            }
            if inner.preferences.remotes != normalized_remotes {
                inner.preferences.remotes = normalized_remotes.clone();
                next_remotes = Some(normalized_remotes);
                changed = true;
            }
        }

        if changed {
            self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist app settings: {err:#}"))
            })?;
        }

        let snapshot = self.snapshot_from_inner(&inner);
        drop(inner);
        if let Some(remotes) = next_remotes {
            let changed_ids = self.remote_registry.reconcile(&remotes);
            // Clear revision watermarks synchronously so the first response
            // from a newly pointed/restarted remote is not dropped as stale.
            for remote_id in &changed_ids {
                self.clear_remote_applied_revision(remote_id);
                self.clear_remote_sse_fallback_resync(remote_id);
            }
        }
        Ok(snapshot)
    }

    /// Creates project.
    fn create_project(
        &self,
        request: CreateProjectRequest,
    ) -> Result<CreateProjectResponse, ApiError> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let remote_id = if request.remote_id.trim().is_empty() {
            default_local_remote_id()
        } else {
            request.remote_id.trim().to_owned()
        };
        let remote = inner
            .find_remote(&remote_id)
            .cloned()
            .ok_or_else(|| ApiError::bad_request(format!("unknown remote `{remote_id}`")))?;
        let trimmed_root_path = request.root_path.trim();
        if trimmed_root_path.is_empty() {
            return Err(ApiError::bad_request("project root path cannot be empty"));
        }
        let root_path = if matches!(remote.transport, RemoteTransport::Local) {
            resolve_project_root_path(trimmed_root_path)?
        } else {
            trimmed_root_path.to_owned()
        };
        if !remote.enabled {
            return Err(ApiError::bad_request(format!(
                "remote `{}` is disabled",
                remote.name
            )));
        }
        if remote_id != LOCAL_REMOTE_ID {
            drop(inner);
            return self.create_remote_project_proxy(request, remote, root_path);
        }
        let existing_len = inner.projects.len();
        let project = inner.create_project(request.name, root_path, remote_id);
        if inner.projects.len() != existing_len {
            self.commit_locked(&mut inner)
                .map_err(|err| ApiError::internal(format!("failed to persist project: {err:#}")))?;
        }
        Ok(CreateProjectResponse {
            project_id: project.id,
            state: self.snapshot_from_inner(&inner),
        })
    }

    /// Deletes the local project reference and keeps its sessions visible
    /// outside project scope. Remote-backed projects are intentionally removed
    /// only from this local state; TermAl does not delete remote project data
    /// from a local project-list action.
    fn delete_project(&self, project_id: &str) -> Result<StateResponse, ApiError> {
        let project_id = normalize_optional_identifier(Some(project_id))
            .ok_or_else(|| ApiError::bad_request("project id is required"))?;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(project_index) = inner
            .projects
            .iter()
            .position(|project| project.id == project_id)
        else {
            return Err(ApiError::not_found("project not found"));
        };

        inner.projects.remove(project_index);
        // Collect affected indices first so the mutating pass can go
        // through `session_mut_by_index` (which bumps `mutation_stamp`).
        // Iterating `&mut inner.sessions` directly would clear the
        // `project_id` in memory but skip the stamp, causing
        // `collect_persist_delta` to drop these changes — the deleted
        // project would reappear attached to those sessions on restart.
        let affected_session_indices: Vec<usize> = inner
            .sessions
            .iter()
            .enumerate()
            .filter_map(|(idx, record)| {
                if record.session.project_id.as_deref() == Some(project_id) {
                    Some(idx)
                } else {
                    None
                }
            })
            .collect();
        for idx in affected_session_indices {
            if let Some(record) = inner.session_mut_by_index(idx) {
                record.session.project_id = None;
            }
        }
        for instance in &mut inner.orchestrator_instances {
            if instance.project_id == project_id {
                instance.project_id.clear();
            }
        }

        self.commit_locked(&mut inner)
            .map_err(|err| ApiError::internal(format!("failed to remove project: {err:#}")))?;
        Ok(self.snapshot_from_inner(&inner))
    }

    /// Handles commit locked.
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
    /// Handles commit delta locked.
    fn commit_delta_locked(&self, inner: &mut StateInner) -> Result<u64> {
        inner.revision += 1;
        Ok(inner.revision)
    }

    // Some live-update paths still need durable persistence, but should not force a full-state
    // SSE snapshot when a small targeted delta is enough for the UI.
    /// Handles commit persisted delta locked.
    fn commit_persisted_delta_locked(&self, inner: &mut StateInner) -> Result<u64> {
        self.bump_revision_and_persist_locked(inner)
    }

    /// Handles bump revision and persist locked.
    fn bump_revision_and_persist_locked(&self, inner: &mut StateInner) -> Result<u64> {
        inner.revision += 1;
        self.persist_internal_locked(inner)?;
        Ok(inner.revision)
    }

    /// Handles subscribe events.
    fn subscribe_events(&self) -> broadcast::Receiver<String> {
        self.state_events.subscribe()
    }

    /// Handles subscribe delta events.
    fn subscribe_delta_events(&self) -> broadcast::Receiver<String> {
        self.delta_events.subscribe()
    }

    /// Handles subscribe file events.
    fn subscribe_file_events(&self) -> broadcast::Receiver<String> {
        self.file_events.subscribe()
    }

    /// Publishes delta.
    fn publish_delta(&self, event: &DeltaEvent) {
        if let Ok(payload) = serde_json::to_string(event) {
            let _ = self.delta_events.send(payload);
        }
    }

    /// Publishes workspace file changes.
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

    /// Records watcher changes against currently active local turns.
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

    /// Publishes state locked.  Fire-and-forget like [`publish_delta`](Self::publish_delta);
    /// serialization errors are logged but do not propagate.
    ///
    /// The snapshot is built from `inner` while the caller still holds the
    /// state mutex (required — `inner` fields are read here), but JSON
    /// serialization is offloaded to a dedicated broadcaster thread via
    /// [`publish_snapshot`](Self::publish_snapshot). This keeps the state
    /// mutex off the serialization critical path for requests (e.g.,
    /// `put_workspace_layout`) that commit under the lock.
    fn publish_state_locked(&self, inner: &StateInner) {
        let snapshot = self.snapshot_from_inner(inner);
        self.publish_snapshot(snapshot);
    }

    /// Publishes a pre-built snapshot as an SSE state event.
    ///
    /// Sends the owned snapshot to the background broadcaster thread, which
    /// serializes to JSON and forwards to `state_events` off the critical
    /// path. Falls back to synchronous serialize + broadcast if the channel
    /// is disconnected (test builds that construct `AppState` manually
    /// without a broadcaster thread).
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

    #[cfg(not(test))]
    fn spawn_workspace_file_watcher(&self) {
        let state = self.clone();
        std::thread::Builder::new()
            .name("termal-file-watch".to_owned())
            .spawn(move || run_workspace_file_watcher(state))
            .expect("failed to spawn file watcher thread");
    }

    /// Handles shared Codex runtime.
    fn shared_codex_runtime(&self) -> Result<SharedCodexRuntime> {
        let mut shared_runtime = self
            .shared_codex_runtime
            .lock()
            .expect("shared Codex runtime mutex poisoned");
        if let Some(runtime) = shared_runtime.clone() {
            return Ok(runtime);
        }

        let runtime = spawn_shared_codex_runtime(self.clone())?;
        *shared_runtime = Some(runtime.clone());
        Ok(runtime)
    }

    /// Handles perform Codex JSON RPC request.
    fn perform_codex_json_rpc_request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, ApiError> {
        let runtime = self.shared_codex_runtime().map_err(|err| {
            ApiError::internal(format!("failed to start shared Codex runtime: {err:#}"))
        })?;
        let (response_tx, response_rx) = mpsc::channel::<std::result::Result<Value, String>>();
        runtime
            .input_tx
            .send(CodexRuntimeCommand::JsonRpcRequest {
                method: method.to_owned(),
                params,
                timeout,
                response_tx,
            })
            .map_err(|err| {
                ApiError::internal(format!("failed to queue Codex request `{method}`: {err}"))
            })?;

        match response_rx.recv_timeout(timeout + Duration::from_secs(1)) {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(detail)) => Err(ApiError::bad_request(format!(
                "Codex request `{method}` failed: {detail}"
            ))),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ApiError::internal(format!(
                "timed out waiting for Codex request `{method}`"
            ))),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ApiError::internal(format!(
                "Codex request `{method}` did not return a result"
            ))),
        }
    }

    /// Resolves Codex thread action context.
    fn resolve_codex_thread_action_context(
        &self,
        session_id: &str,
    ) -> Result<CodexThreadActionContext, ApiError> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = &inner.sessions[index];

        if record.session.agent != Agent::Codex {
            return Err(ApiError::bad_request(
                "Codex thread actions are only available for Codex sessions",
            ));
        }
        if matches!(
            record.session.status,
            SessionStatus::Active | SessionStatus::Approval
        ) {
            return Err(ApiError::conflict(
                "wait for the current Codex turn to finish before using thread actions",
            ));
        }
        if !record.queued_prompts.is_empty() {
            return Err(ApiError::conflict(
                "wait for queued Codex prompts to finish before using thread actions",
            ));
        }

        let thread_id = record.external_session_id.clone().ok_or_else(|| {
            ApiError::bad_request(
                "Codex thread actions are only available after the session has started a thread",
            )
        })?;

        Ok(CodexThreadActionContext {
            approval_policy: record
                .session
                .approval_policy
                .unwrap_or(record.codex_approval_policy),
            model: record.session.model.clone(),
            model_options: record.session.model_options.clone(),
            name: record.session.name.clone(),
            project_id: record.session.project_id.clone(),
            reasoning_effort: record
                .session
                .reasoning_effort
                .unwrap_or(record.codex_reasoning_effort),
            sandbox_mode: record
                .session
                .sandbox_mode
                .unwrap_or(record.codex_sandbox_mode),
            thread_id,
            thread_state: normalized_codex_thread_state(
                record.session.agent,
                record.external_session_id.as_deref(),
                record.session.codex_thread_state,
            ),
            workdir: record.session.workdir.clone(),
        })
    }

    /// Clears shared Codex runtime if matches.
    fn clear_shared_codex_runtime_if_matches(&self, runtime_id: &str) -> Result<()> {
        let removed_runtime = {
            let mut shared_runtime = self
                .shared_codex_runtime
                .lock()
                .expect("shared Codex runtime mutex poisoned");
            if shared_runtime
                .as_ref()
                .is_some_and(|runtime| runtime.runtime_id == runtime_id)
            {
                shared_runtime.take()
            } else {
                None
            }
        };

        if let Some(runtime) = removed_runtime {
            runtime.kill().with_context(|| {
                format!("failed to terminate shared Codex runtime `{runtime_id}`")
            })?;
        }

        Ok(())
    }

    /// Handles shared Codex runtime exit.
    fn handle_shared_codex_runtime_exit(
        &self,
        runtime_id: &str,
        error_message: Option<&str>,
    ) -> Result<()> {
        let session_ids = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .sessions
                .iter()
                .filter_map(|record| match &record.runtime {
                    SessionRuntime::Codex(handle) if handle.runtime_id == runtime_id => {
                        Some(record.session.id.clone())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };

        let token = RuntimeToken::Codex(runtime_id.to_owned());
        for session_id in session_ids {
            self.handle_runtime_exit_if_matches(&session_id, &token, error_message)?;
        }
        self.clear_shared_codex_runtime_if_matches(runtime_id)?;
        Ok(())
    }

    /// Builds a snapshot using the latest cached agent readiness **without refreshing**.
    ///
    /// This is the hot-path builder used inside `commit_locked` / `publish_state_locked`
    /// where the `inner` mutex is held and filesystem I/O is not safe.  Callers that
    /// need guaranteed-fresh readiness (e.g. after an explicit cache invalidation) should
    /// drop the `inner` lock and use [`snapshot()`](Self::snapshot) instead.
    ///
    /// **Design tradeoff:** after the cache TTL expires, mutation paths through
    /// `commit_locked` will publish SSE events with stale readiness until a
    /// [`snapshot()`](Self::snapshot) call (e.g. `GET /api/state`, SSE reconnect)
    /// refreshes the cache.  This staleness can span multiple revisions — it is
    /// not bounded to a single mutation cycle.  This is acceptable because agent
    /// readiness changes only when CLI tools are installed or removed (extremely
    /// rare during an active session), and any `snapshot()` call refreshes the
    /// cache as a side effect even when the frontend drops the response, so the
    /// following mutation carries the fresh value.  Paths where freshness matters
    /// (`create_session`, `update_app_settings`) pre-refresh the cache before
    /// entering the critical section.
    fn snapshot_from_inner(&self, inner: &StateInner) -> StateResponse {
        self.snapshot_from_inner_with_agent_readiness(inner, self.cached_agent_readiness())
    }

    fn snapshot_from_inner_with_agent_readiness(
        &self,
        inner: &StateInner,
        agent_readiness: Vec<AgentReadiness>,
    ) -> StateResponse {
        StateResponse {
            revision: inner.revision,
            codex: inner.codex.clone(),
            agent_readiness,
            preferences: inner.preferences.clone(),
            projects: inner.projects.clone(),
            orchestrators: inner.orchestrator_instances.clone(),
            workspaces: collect_workspace_layout_summaries(inner.workspace_layouts.values()),
            sessions: inner
                .sessions
                .iter()
                .filter(|record| !record.hidden)
                .map(|record| record.session.clone())
                .collect(),
        }
    }

    /// Seeds hidden Claude spares.
    fn seed_hidden_claude_spares(&self) {
        let spare_ids = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let contexts = inner
                .sessions
                .iter()
                .filter(|record| {
                    !record.hidden
                        && !record.is_remote_proxy()
                        && record.session.agent == Agent::Claude
                })
                .map(|record| {
                    (
                        record.session.workdir.clone(),
                        record.session.project_id.clone(),
                        record.session.model.clone(),
                        record
                            .session
                            .claude_approval_mode
                            .unwrap_or_else(default_claude_approval_mode),
                        record
                            .session
                            .claude_effort
                            .unwrap_or_else(default_claude_effort),
                    )
                })
                .collect::<Vec<_>>();
            let mut spare_ids = Vec::new();
            for (workdir, project_id, model, approval_mode, effort) in contexts {
                if let Some(session_id) = inner.ensure_hidden_claude_spare(
                    workdir,
                    project_id,
                    model,
                    approval_mode,
                    effort,
                ) {
                    spare_ids.push(session_id);
                }
            }
            spare_ids
        };

        for session_id in spare_ids {
            self.try_start_hidden_claude_spare(&session_id);
        }
    }

    /// Handles try start hidden Claude spare.
    fn try_start_hidden_claude_spare(&self, session_id: &str) {
        let spawn_request = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return;
            };
            let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
            if !record.hidden
                || record.is_remote_proxy()
                || record.session.agent != Agent::Claude
                || !matches!(record.runtime, SessionRuntime::None)
            {
                return;
            }

            reset_hidden_claude_spare_record(record);
            Some((
                record.session.id.clone(),
                record.session.workdir.clone(),
                record.session.model.clone(),
                record
                    .session
                    .claude_approval_mode
                    .unwrap_or_else(default_claude_approval_mode),
                record
                    .session
                    .claude_effort
                    .unwrap_or_else(default_claude_effort),
                record.external_session_id.clone(),
            ))
        };

        let Some((session_id, cwd, model, approval_mode, effort, resume_session_id)) =
            spawn_request
        else {
            return;
        };

        let handle = match spawn_claude_runtime(
            self.clone(),
            session_id.clone(),
            cwd,
            model,
            approval_mode,
            effort,
            resume_session_id,
            None,
        ) {
            Ok(handle) => handle,
            Err(err) => {
                eprintln!("claude hidden pool> failed to warm spare `{session_id}`: {err:#}");
                return;
            }
        };

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(&session_id) else {
            let _ = handle.kill();
            return;
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if record.session.agent != Agent::Claude || !matches!(record.runtime, SessionRuntime::None)
        {
            let _ = handle.kill();
            return;
        }
        record.runtime = SessionRuntime::Claude(handle);
    }

    /// Starts turn on record.
    fn start_turn_on_record(
        &self,
        record: &mut SessionRecord,
        message_id: String,
        prompt: String,
        attachments: Vec<PromptImageAttachment>,
        expanded_prompt: Option<String>,
    ) -> std::result::Result<TurnDispatch, ApiError> {
        if record.is_remote_proxy() {
            return Err(ApiError::internal(
                "remote proxy sessions must dispatch through the remote backend",
            ));
        }

        let message_attachments = attachments
            .iter()
            .map(|attachment| attachment.metadata.clone())
            .collect::<Vec<_>>();
        record.active_turn_start_message_count = Some(record.session.messages.len());
        let expanded_prompt = expanded_prompt
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != prompt)
            .map(str::to_owned);
        let runtime_prompt = expanded_prompt.clone().unwrap_or_else(|| prompt.clone());

        let dispatch = match record.session.agent {
            Agent::Claude => {
                if record.runtime_reset_required {
                    if let SessionRuntime::Claude(handle) = &record.runtime {
                        handle.kill().map_err(|err| {
                            ApiError::internal(format!(
                                "failed to restart Claude session runtime: {err:#}"
                            ))
                        })?;
                    }
                    record.runtime = SessionRuntime::None;
                    record.pending_claude_approvals.clear();
                    record.runtime_reset_required = false;
                }

                let handle = match &record.runtime {
                    SessionRuntime::Claude(handle) => handle.clone(),
                    SessionRuntime::Codex(_) => {
                        return Err(ApiError::internal(
                            "unexpected Codex runtime attached to Claude session",
                        ));
                    }
                    SessionRuntime::Acp(_) => {
                        return Err(ApiError::internal(
                            "unexpected ACP runtime attached to Claude session",
                        ));
                    }
                    SessionRuntime::None => {
                        let handle = spawn_claude_runtime(
                            self.clone(),
                            record.session.id.clone(),
                            record.session.workdir.clone(),
                            record.session.model.clone(),
                            record
                                .session
                                .claude_approval_mode
                                .unwrap_or_else(default_claude_approval_mode),
                            record
                                .session
                                .claude_effort
                                .unwrap_or_else(default_claude_effort),
                            record.external_session_id.clone(),
                            None,
                        )
                        .map_err(|err| {
                            ApiError::internal(format!(
                                "failed to start persistent Claude session: {err:#}"
                            ))
                        })?;
                        record.runtime = SessionRuntime::Claude(handle.clone());
                        handle
                    }
                };

                TurnDispatch::PersistentClaude {
                    command: ClaudePromptCommand {
                        attachments: attachments.clone(),
                        text: runtime_prompt.clone(),
                    },
                    sender: handle.input_tx,
                    session_id: record.session.id.clone(),
                }
            }
            Agent::Codex => {
                if record.runtime_reset_required {
                    if let SessionRuntime::Codex(handle) = &record.runtime {
                        if let Some(shared_session) = &handle.shared_session {
                            shared_session.detach();
                        } else {
                            handle.kill().map_err(|err| {
                                ApiError::internal(format!(
                                    "failed to restart Codex session runtime: {err:#}"
                                ))
                            })?;
                        }
                    }
                    record.runtime = SessionRuntime::None;
                    record.pending_codex_approvals.clear();
                    record.pending_codex_user_inputs.clear();
                    record.pending_codex_mcp_elicitations.clear();
                    record.pending_codex_app_requests.clear();
                    record.runtime_reset_required = false;
                }

                let handle = match &record.runtime {
                    SessionRuntime::Codex(handle) => handle.clone(),
                    SessionRuntime::Claude(_) => {
                        return Err(ApiError::internal(
                            "unexpected Claude runtime attached to Codex session",
                        ));
                    }
                    SessionRuntime::Acp(_) => {
                        return Err(ApiError::internal(
                            "unexpected ACP runtime attached to Codex session",
                        ));
                    }
                    SessionRuntime::None => {
                        let handle = spawn_codex_runtime(
                            self.clone(),
                            record.session.id.clone(),
                            record.session.workdir.clone(),
                        )
                        .map_err(|err| {
                            ApiError::internal(format!(
                                "failed to start persistent Codex session: {err:#}"
                            ))
                        })?;
                        record.runtime = SessionRuntime::Codex(handle.clone());
                        handle
                    }
                };

                TurnDispatch::PersistentCodex {
                    command: CodexPromptCommand {
                        approval_policy: record.codex_approval_policy,
                        attachments,
                        cwd: record.session.workdir.clone(),
                        model: record.session.model.clone(),
                        prompt: runtime_prompt.to_owned(),
                        reasoning_effort: record.codex_reasoning_effort,
                        resume_thread_id: record.external_session_id.clone(),
                        sandbox_mode: record.codex_sandbox_mode,
                    },
                    sender: handle.input_tx,
                    session_id: record.session.id.clone(),
                }
            }
            agent @ (Agent::Cursor | Agent::Gemini) => {
                if !attachments.is_empty() {
                    return Err(ApiError::bad_request(format!(
                        "{} sessions do not support image attachments yet",
                        agent.name()
                    )));
                }

                if record.runtime_reset_required {
                    if let SessionRuntime::Acp(handle) = &record.runtime {
                        handle.kill().map_err(|err| {
                            ApiError::internal(format!(
                                "failed to restart {} session runtime: {err:#}",
                                agent.name()
                            ))
                        })?;
                    }
                    record.runtime = SessionRuntime::None;
                    record.pending_acp_approvals.clear();
                    record.runtime_reset_required = false;
                }

                let expected_acp_agent = agent
                    .acp_runtime()
                    .ok_or_else(|| ApiError::internal("missing ACP runtime config"))?;
                let handle = match &record.runtime {
                    SessionRuntime::Acp(handle) if handle.agent == expected_acp_agent => {
                        handle.clone()
                    }
                    SessionRuntime::Acp(_) => {
                        return Err(ApiError::internal(
                            "unexpected ACP runtime attached to session",
                        ));
                    }
                    SessionRuntime::Claude(_) => {
                        return Err(ApiError::internal(
                            "unexpected Claude runtime attached to ACP session",
                        ));
                    }
                    SessionRuntime::Codex(_) => {
                        return Err(ApiError::internal(
                            "unexpected Codex runtime attached to ACP session",
                        ));
                    }
                    SessionRuntime::None => {
                        let handle = spawn_acp_runtime(
                            self.clone(),
                            record.session.id.clone(),
                            record.session.workdir.clone(),
                            expected_acp_agent,
                            record.session.gemini_approval_mode,
                        )
                        .map_err(|err| {
                            ApiError::internal(format!(
                                "failed to start persistent {} session: {err:#}",
                                agent.name()
                            ))
                        })?;
                        record.runtime = SessionRuntime::Acp(handle.clone());
                        handle
                    }
                };

                TurnDispatch::PersistentAcp {
                    command: AcpPromptCommand {
                        cwd: record.session.workdir.clone(),
                        cursor_mode: record.session.cursor_mode,
                        model: record.session.model.clone(),
                        prompt: runtime_prompt.to_owned(),
                        resume_session_id: record.external_session_id.clone(),
                    },
                    sender: handle.input_tx,
                    session_id: record.session.id.clone(),
                }
            }
        };

        record.orchestrator_auto_dispatch_blocked = false;
        record.active_turn_file_changes.clear();
        record.active_turn_file_change_grace_deadline = None;
        push_message_on_record(
            record,
            Message::Text {
                attachments: message_attachments.clone(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::You,
                text: prompt.clone(),
                expanded_text: expanded_prompt,
            },
        );
        record.session.status = SessionStatus::Active;
        record.session.preview = prompt_preview_text(&prompt, &message_attachments);

        Ok(dispatch)
    }

    /// Dispatches orphaned queued prompts.
    fn dispatch_orphaned_queued_prompts(&self) {
        let session_ids: Vec<String> = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            inner
                .sessions
                .iter()
                .filter(|record| {
                    !record.is_remote_proxy()
                        && record.session.status == SessionStatus::Idle
                        && !record.queued_prompts.is_empty()
                        && !record.orchestrator_auto_dispatch_blocked
                        && matches!(record.runtime, SessionRuntime::None)
                })
                .map(|record| record.session.id.clone())
                .collect()
        };

        for session_id in session_ids {
            match self.dispatch_next_queued_turn(&session_id, false) {
                Ok(Some(dispatch)) => {
                    if let Err(err) = deliver_turn_dispatch(self, dispatch) {
                        eprintln!(
                            "startup> failed dispatching orphaned queued prompt for `{session_id}`: {}",
                            err.message
                        );
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    eprintln!(
                        "startup> failed dispatching orphaned queued prompt for `{session_id}`: {err:#}"
                    );
                }
            }
        }
    }

    /// Dispatches next queued turn.
    fn dispatch_next_queued_turn(
        &self,
        session_id: &str,
        allow_blocked_dispatch: bool,
    ) -> Result<Option<TurnDispatch>> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;

        if inner.sessions[index].orchestrator_auto_dispatch_blocked && !allow_blocked_dispatch {
            return Ok(None);
        }

        let queued = inner.sessions[index].queued_prompts.front().cloned();

        let Some(queued) = queued else {
            return Ok(None);
        };

        let dispatch = self
            .start_turn_on_record(
                inner
            .session_mut_by_index(index)
            .expect("session index should be valid"),
                queued.pending_prompt.id.clone(),
                queued.pending_prompt.text.clone(),
                queued.attachments.clone(),
                queued.pending_prompt.expanded_text.clone(),
            )
            .map_err(|err| anyhow!("failed to dispatch queued prompt: {}", err.message))?;
        inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .queued_prompts
            .pop_front();
        sync_pending_prompts(inner
            .session_mut_by_index(index)
            .expect("session index should be valid"));
        self.commit_locked(&mut inner)?;
        Ok(Some(dispatch))
    }

    /// Dispatches turn.
    fn dispatch_turn(
        &self,
        session_id: &str,
        request: SendMessageRequest,
    ) -> std::result::Result<DispatchTurnResult, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            self.proxy_remote_turn_dispatch(session_id, request)?;
            return Ok(DispatchTurnResult::Queued);
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;

        let mut prompt = request.text.trim().to_owned();
        let mut expanded_prompt = request
            .expanded_text
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != prompt)
            .map(str::to_owned);
        let attachments = parse_prompt_image_attachments(&request.attachments)?;
        if record_has_archived_codex_thread(&inner.sessions[index]) {
            return Err(ApiError::conflict(
                "the current Codex thread is archived; unarchive it before sending another prompt",
            ));
        }
        if let Some(template_session) =
            orchestrator_template_session_for_runtime_session(&inner, session_id)
        {
            let rendered_prompt = build_orchestrator_destination_prompt(
                &inner.sessions[index],
                &template_session.instructions,
                expanded_prompt.as_deref().unwrap_or(&prompt),
            );
            if expanded_prompt.is_some() {
                prompt = rendered_prompt.clone();
                expanded_prompt = Some(rendered_prompt);
            } else {
                prompt = rendered_prompt;
            }
        }
        if prompt.is_empty() && attachments.is_empty() {
            return Err(ApiError::bad_request("prompt cannot be empty"));
        }

        let session_is_busy = matches!(
            inner.sessions[index].session.status,
            SessionStatus::Active | SessionStatus::Approval
        );
        let has_queued_prompts = !inner.sessions[index].queued_prompts.is_empty();
        let blocked_queue_contains_user_prompt = inner.sessions[index]
            .queued_prompts
            .iter()
            .any(|queued| queued.source == QueuedPromptSource::User);
        let recover_blocked_queue_with_existing_user_prompt = !session_is_busy
            && has_queued_prompts
            && inner.sessions[index].orchestrator_auto_dispatch_blocked
            && blocked_queue_contains_user_prompt;
        let prioritize_manual_dispatch_over_blocked_queue = !session_is_busy
            && has_queued_prompts
            && inner.sessions[index].orchestrator_auto_dispatch_blocked
            && !blocked_queue_contains_user_prompt;

        if recover_blocked_queue_with_existing_user_prompt {
            let message_id = inner.next_message_id();
            queue_prompt_on_record(
                inner
            .session_mut_by_index(index)
            .expect("session index should be valid"),
                PendingPrompt {
                    attachments: attachments
                        .iter()
                        .map(|attachment| attachment.metadata.clone())
                        .collect(),
                    id: message_id,
                    timestamp: stamp_now(),
                    text: prompt,
                    expanded_text: expanded_prompt.clone(),
                },
                attachments,
            );
            prioritize_user_queued_prompts(inner
            .session_mut_by_index(index)
            .expect("session index should be valid"));
            self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist session state: {err:#}"))
            })?;

            drop(inner);
            let dispatch = self
                .dispatch_next_queued_turn(session_id, true)
                .map_err(|err| {
                    ApiError::internal(format!("failed to dispatch queued turn: {err:#}"))
                })?
                .ok_or_else(|| ApiError::internal("queued prompt disappeared before dispatch"))?;
            return Ok(DispatchTurnResult::Dispatched(dispatch));
        }

        if prioritize_manual_dispatch_over_blocked_queue {
            let message_id = inner.next_message_id();
            let dispatch = self.start_turn_on_record(
                inner
            .session_mut_by_index(index)
            .expect("session index should be valid"),
                message_id,
                prompt,
                attachments,
                expanded_prompt,
            )?;

            self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist session state: {err:#}"))
            })?;
            return Ok(DispatchTurnResult::Dispatched(dispatch));
        }

        if session_is_busy || has_queued_prompts {
            let message_id = inner.next_message_id();
            queue_prompt_on_record(
                inner
            .session_mut_by_index(index)
            .expect("session index should be valid"),
                PendingPrompt {
                    attachments: attachments
                        .iter()
                        .map(|attachment| attachment.metadata.clone())
                        .collect(),
                    id: message_id,
                    timestamp: stamp_now(),
                    text: prompt,
                    expanded_text: expanded_prompt.clone(),
                },
                attachments,
            );
            self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist session state: {err:#}"))
            })?;
            if session_is_busy {
                return Ok(DispatchTurnResult::Queued);
            }

            drop(inner);
            let dispatch = self
                .dispatch_next_queued_turn(session_id, true)
                .map_err(|err| {
                    ApiError::internal(format!("failed to dispatch queued turn: {err:#}"))
                })?
                .ok_or_else(|| ApiError::internal("queued prompt disappeared before dispatch"))?;
            return Ok(DispatchTurnResult::Dispatched(dispatch));
        }

        let message_id = inner.next_message_id();
        let dispatch = self.start_turn_on_record(
            inner
            .session_mut_by_index(index)
            .expect("session index should be valid"),
            message_id,
            prompt,
            attachments,
            expanded_prompt,
        )?;

        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;

        Ok(DispatchTurnResult::Dispatched(dispatch))
    }

    /// Updates session settings.
    fn update_session_settings(
        &self,
        session_id: &str,
        request: UpdateSessionSettingsRequest,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_session_settings(session_id, request);
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let mut claude_model_update: Option<(ClaudeRuntimeHandle, String)> = None;
        let mut claude_permission_mode_update: Option<(ClaudeRuntimeHandle, String)> = None;
        let mut acp_config_updates: Vec<(AcpRuntimeHandle, Value)> = Vec::new();

        match record.session.agent {
            agent if agent.supports_codex_prompt_settings() => {
                if request.claude_approval_mode.is_some() || request.claude_effort.is_some() {
                    return Err(ApiError::bad_request(
                        "Claude mode and effort can only be changed for Claude sessions",
                    ));
                }
                if request.cursor_mode.is_some() || request.gemini_approval_mode.is_some() {
                    return Err(ApiError::bad_request(
                        "Codex sessions do not support Cursor or Gemini settings",
                    ));
                }
            }
            agent if agent.supports_claude_approval_mode() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.cursor_mode.is_some()
                    || request.gemini_approval_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Claude sessions only support model, mode, and effort settings",
                    ));
                }
            }
            agent if agent.supports_cursor_mode() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.claude_approval_mode.is_some()
                    || request.claude_effort.is_some()
                    || request.gemini_approval_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Cursor sessions only support model and mode settings",
                    ));
                }
            }
            agent if agent.supports_gemini_approval_mode() => {
                if request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.claude_approval_mode.is_some()
                    || request.claude_effort.is_some()
                    || request.cursor_mode.is_some()
                {
                    return Err(ApiError::bad_request(
                        "Gemini sessions only support model and approval mode settings",
                    ));
                }
            }
            agent => {
                if request.model.is_some()
                    || request.sandbox_mode.is_some()
                    || request.approval_policy.is_some()
                    || request.reasoning_effort.is_some()
                    || request.claude_approval_mode.is_some()
                    || request.claude_effort.is_some()
                    || request.cursor_mode.is_some()
                    || request.gemini_approval_mode.is_some()
                {
                    return Err(ApiError::bad_request(format!(
                        "{} sessions do not support prompt settings yet",
                        agent.name()
                    )));
                }
            }
        }

        if let Some(name) = request.name.as_deref() {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                return Err(ApiError::bad_request("session name cannot be empty"));
            }
            record.session.name = trimmed.to_owned();
        }

        if let Some(model) = request.model.as_deref() {
            let trimmed = model.trim();
            if trimmed.is_empty() {
                return Err(ApiError::bad_request("session model cannot be empty"));
            }
        }
        let requested_model = request
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| {
                matching_session_model_option_value(value, &record.session.model_options)
                    .unwrap_or_else(|| value.to_owned())
            });

        match record.session.agent {
            agent if agent.supports_codex_prompt_settings() => {
                let next_model = requested_model
                    .clone()
                    .unwrap_or_else(|| record.session.model.clone());
                let next_reasoning_effort = request
                    .reasoning_effort
                    .unwrap_or(record.codex_reasoning_effort);
                let normalized_reasoning_effort = normalized_codex_reasoning_effort(
                    &next_model,
                    next_reasoning_effort,
                    &record.session.model_options,
                );
                if request.reasoning_effort.is_some() {
                    if let Some(normalized_reasoning_effort) = normalized_reasoning_effort {
                        if normalized_reasoning_effort != next_reasoning_effort {
                            if let Some(option) =
                                codex_model_option(&next_model, &record.session.model_options)
                            {
                                return Err(ApiError::bad_request(format!(
                                    "model `{}` does not support `{}` reasoning effort; choose {}",
                                    option.label,
                                    next_reasoning_effort.as_api_value(),
                                    format_codex_reasoning_efforts(
                                        &option.supported_reasoning_efforts
                                    )
                                )));
                            }
                        }
                    }
                }
                if let Some(model) = requested_model.as_deref() {
                    if record.session.model != model {
                        record.session.model = model.to_owned();
                    }
                }
                if let Some(sandbox_mode) = request.sandbox_mode {
                    record.codex_sandbox_mode = sandbox_mode;
                    record.session.sandbox_mode = Some(sandbox_mode);
                }
                if let Some(approval_policy) = request.approval_policy {
                    record.codex_approval_policy = approval_policy;
                    record.session.approval_policy = Some(approval_policy);
                }
                if let Some(reasoning_effort) = request.reasoning_effort {
                    record.codex_reasoning_effort = reasoning_effort;
                    record.session.reasoning_effort = Some(reasoning_effort);
                } else if let Some(normalized_reasoning_effort) = normalized_reasoning_effort {
                    if record.codex_reasoning_effort != normalized_reasoning_effort {
                        record.codex_reasoning_effort = normalized_reasoning_effort;
                        record.session.reasoning_effort = Some(normalized_reasoning_effort);
                    }
                }
            }
            agent if agent.supports_claude_approval_mode() => {
                let should_restart_for_effort =
                    request.claude_effort.is_some_and(|claude_effort| {
                        record.session.claude_effort != Some(claude_effort)
                    });
                if should_restart_for_effort {
                    record.runtime_reset_required = true;
                }
                if let Some(model) = requested_model.as_deref() {
                    if record.session.model != model {
                        record.session.model = model.to_owned();
                        if should_restart_for_effort {
                            record.runtime_reset_required = true;
                        } else if let SessionRuntime::Claude(handle) = &record.runtime {
                            if claude_cli_model_arg(model).is_some() {
                                claude_model_update = Some((handle.clone(), model.to_owned()));
                            } else {
                                record.runtime_reset_required = true;
                            }
                        }
                    }
                }
                if let Some(claude_approval_mode) = request.claude_approval_mode {
                    record.session.claude_approval_mode = Some(claude_approval_mode);
                    if let SessionRuntime::Claude(handle) = &record.runtime {
                        claude_permission_mode_update = Some((
                            handle.clone(),
                            claude_approval_mode
                                .session_cli_permission_mode()
                                .to_owned(),
                        ));
                    }
                }
                if let Some(claude_effort) = request.claude_effort {
                    record.session.claude_effort = Some(claude_effort);
                }
            }
            agent if agent.supports_cursor_mode() => {
                if let Some(model) = requested_model.as_deref() {
                    if record.session.model != model {
                        record.session.model = model.to_owned();
                        if let (SessionRuntime::Acp(handle), Some(external_session_id)) =
                            (&record.runtime, record.external_session_id.as_deref())
                        {
                            acp_config_updates.push((
                                handle.clone(),
                                json_rpc_request_message(
                                    Uuid::new_v4().to_string(),
                                    "session/set_config_option",
                                    json!({
                                        "sessionId": external_session_id,
                                        "optionId": "model",
                                        "value": model,
                                    }),
                                ),
                            ));
                        }
                    }
                }
                if let Some(cursor_mode) = request.cursor_mode {
                    if record.session.cursor_mode != Some(cursor_mode) {
                        record.session.cursor_mode = Some(cursor_mode);
                        if let (SessionRuntime::Acp(handle), Some(external_session_id)) =
                            (&record.runtime, record.external_session_id.as_deref())
                        {
                            acp_config_updates.push((
                                handle.clone(),
                                json_rpc_request_message(
                                    Uuid::new_v4().to_string(),
                                    "session/set_config_option",
                                    json!({
                                        "sessionId": external_session_id,
                                        "optionId": "mode",
                                        "value": cursor_mode.as_acp_value(),
                                    }),
                                ),
                            ));
                        }
                    }
                }
            }
            agent if agent.supports_gemini_approval_mode() => {
                if let Some(model) = requested_model.as_deref() {
                    record.session.model = model.to_owned();
                }
                if let Some(gemini_approval_mode) = request.gemini_approval_mode {
                    if record.session.gemini_approval_mode != Some(gemini_approval_mode) {
                        record.runtime_reset_required = true;
                    }
                    record.session.gemini_approval_mode = Some(gemini_approval_mode);
                }
            }
            _ => {}
        }

        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;
        let snapshot = self.snapshot_from_inner(&inner);
        drop(inner);

        if let Some((handle, model)) = claude_model_update {
            let _ = handle.input_tx.send(ClaudeRuntimeCommand::SetModel(model));
        }
        if let Some((handle, permission_mode)) = claude_permission_mode_update {
            let _ = handle
                .input_tx
                .send(ClaudeRuntimeCommand::SetPermissionMode(permission_mode));
        }
        for (handle, request) in acp_config_updates {
            let _ = handle
                .input_tx
                .send(AcpRuntimeCommand::JsonRpcMessage(request));
        }

        Ok(snapshot)
    }

    /// Refreshes session model options.
    fn refresh_session_model_options(
        &self,
        session_id: &str,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_refresh_session_model_options(session_id);
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let agent = record.session.agent;
        if agent == Agent::Claude {
            if record.runtime_reset_required {
                if let SessionRuntime::Claude(handle) = &record.runtime {
                    handle.kill().map_err(|err| {
                        ApiError::internal(format!(
                            "failed to restart Claude session runtime: {err:#}"
                        ))
                    })?;
                }
                record.runtime = SessionRuntime::None;
                record.pending_claude_approvals.clear();
                record.runtime_reset_required = false;
            }

            match &record.runtime {
                SessionRuntime::Acp(_) => {
                    return Err(ApiError::internal(
                        "unexpected ACP runtime attached to Claude session",
                    ));
                }
                SessionRuntime::Codex(_) => {
                    return Err(ApiError::internal(
                        "unexpected Codex runtime attached to Claude session",
                    ));
                }
                SessionRuntime::Claude(handle) => {
                    handle.kill().map_err(|err| {
                        ApiError::internal(format!(
                            "failed to restart Claude session runtime: {err:#}"
                        ))
                    })?;
                    record.runtime = SessionRuntime::None;
                    record.pending_claude_approvals.clear();
                }
                SessionRuntime::None => {}
            }

            let (response_tx, response_rx) =
                mpsc::channel::<std::result::Result<Vec<SessionModelOption>, String>>();
            let handle = spawn_claude_runtime(
                self.clone(),
                record.session.id.clone(),
                record.session.workdir.clone(),
                record.session.model.clone(),
                record
                    .session
                    .claude_approval_mode
                    .unwrap_or_else(default_claude_approval_mode),
                record
                    .session
                    .claude_effort
                    .unwrap_or_else(default_claude_effort),
                record.external_session_id.clone(),
                Some(response_tx),
            )
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to start persistent Claude session: {err:#}"
                ))
            })?;
            record.runtime = SessionRuntime::Claude(handle);
            drop(inner);

            let model_options = match response_rx.recv_timeout(Duration::from_secs(30)) {
                Ok(Ok(model_options)) => model_options,
                Ok(Err(detail)) => {
                    return Err(ApiError::internal(format!(
                        "failed to refresh Claude model options: {detail}"
                    )));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(ApiError::internal(
                        "timed out refreshing Claude model options".to_owned(),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(ApiError::internal(
                        "Claude model refresh did not return a result".to_owned(),
                    ));
                }
            };

            self.sync_session_model_options(session_id, None, model_options)
                .map_err(|err| {
                    ApiError::internal(format!("failed to sync Claude model options: {err:#}"))
                })?;
            return Ok(self.snapshot());
        }

        if agent == Agent::Codex {
            if record.runtime_reset_required {
                if let SessionRuntime::Codex(handle) = &record.runtime {
                    if let Some(shared_session) = &handle.shared_session {
                        shared_session.detach();
                    } else {
                        handle.kill().map_err(|err| {
                            ApiError::internal(format!(
                                "failed to restart Codex session runtime: {err:#}"
                            ))
                        })?;
                    }
                }
                record.runtime = SessionRuntime::None;
                record.pending_codex_approvals.clear();
                record.pending_codex_user_inputs.clear();
                record.pending_codex_mcp_elicitations.clear();
                record.pending_codex_app_requests.clear();
                record.runtime_reset_required = false;
            }

            let handle = match &record.runtime {
                SessionRuntime::Codex(handle) => handle.clone(),
                SessionRuntime::Acp(_) => {
                    return Err(ApiError::internal(
                        "unexpected ACP runtime attached to Codex session",
                    ));
                }
                SessionRuntime::Claude(_) => {
                    return Err(ApiError::internal(
                        "unexpected Claude runtime attached to Codex session",
                    ));
                }
                SessionRuntime::None => {
                    let handle = spawn_codex_runtime(
                        self.clone(),
                        record.session.id.clone(),
                        record.session.workdir.clone(),
                    )
                    .map_err(|err| {
                        ApiError::internal(format!(
                            "failed to start persistent Codex session: {err:#}"
                        ))
                    })?;
                    record.runtime = SessionRuntime::Codex(handle.clone());
                    handle
                }
            };
            drop(inner);

            let (response_tx, response_rx) =
                mpsc::channel::<std::result::Result<Vec<SessionModelOption>, String>>();
            handle
                .input_tx
                .send(CodexRuntimeCommand::RefreshModelList { response_tx })
                .map_err(|err| {
                    ApiError::internal(format!("failed to queue Codex model refresh: {err}"))
                })?;

            let model_options = match response_rx.recv_timeout(Duration::from_secs(30)) {
                Ok(Ok(model_options)) => model_options,
                Ok(Err(detail)) => {
                    return Err(ApiError::internal(format!(
                        "failed to refresh Codex model options: {detail}"
                    )));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(ApiError::internal(
                        "timed out refreshing Codex model options".to_owned(),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(ApiError::internal(
                        "Codex model refresh did not return a result".to_owned(),
                    ));
                }
            };

            self.sync_session_model_options(session_id, None, model_options)
                .map_err(|err| {
                    ApiError::internal(format!("failed to sync Codex model options: {err:#}"))
                })?;
            return Ok(self.snapshot());
        }

        let expected_acp_agent = agent.acp_runtime().ok_or_else(|| {
            ApiError::bad_request(format!(
                "{} sessions do not expose live model options",
                agent.name()
            ))
        })?;

        if record.runtime_reset_required {
            if let SessionRuntime::Acp(handle) = &record.runtime {
                handle.kill().map_err(|err| {
                    ApiError::internal(format!(
                        "failed to restart {} session runtime: {err:#}",
                        agent.name()
                    ))
                })?;
            }
            record.runtime = SessionRuntime::None;
            record.pending_acp_approvals.clear();
            record.runtime_reset_required = false;
        }

        let handle = match &record.runtime {
            SessionRuntime::Acp(handle) if handle.agent == expected_acp_agent => handle.clone(),
            SessionRuntime::Acp(_) => {
                return Err(ApiError::internal(
                    "unexpected ACP runtime attached to session",
                ));
            }
            SessionRuntime::Claude(_) => {
                return Err(ApiError::internal(
                    "unexpected Claude runtime attached to ACP session",
                ));
            }
            SessionRuntime::Codex(_) => {
                return Err(ApiError::internal(
                    "unexpected Codex runtime attached to ACP session",
                ));
            }
            SessionRuntime::None => {
                let handle = spawn_acp_runtime(
                    self.clone(),
                    record.session.id.clone(),
                    record.session.workdir.clone(),
                    expected_acp_agent,
                    record.session.gemini_approval_mode,
                )
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to start persistent {} session: {err:#}",
                        agent.name()
                    ))
                })?;
                record.runtime = SessionRuntime::Acp(handle.clone());
                handle
            }
        };

        let command = AcpPromptCommand {
            cwd: record.session.workdir.clone(),
            cursor_mode: record.session.cursor_mode,
            model: record.session.model.clone(),
            prompt: String::new(),
            resume_session_id: record.external_session_id.clone(),
        };
        drop(inner);

        let (response_tx, response_rx) = mpsc::channel::<std::result::Result<(), String>>();
        handle
            .input_tx
            .send(AcpRuntimeCommand::RefreshSessionConfig {
                command,
                response_tx,
            })
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to queue {} model refresh: {err}",
                    agent.name()
                ))
            })?;

        match response_rx.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(())) => Ok(self.snapshot()),
            Ok(Err(detail)) => Err(ApiError::internal(format!(
                "failed to refresh {} model options: {detail}",
                agent.name()
            ))),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ApiError::internal(format!(
                "timed out refreshing {} model options",
                agent.name()
            ))),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ApiError::internal(format!(
                "{} model refresh did not return a result",
                agent.name()
            ))),
        }
    }

    /// Forks Codex thread.
    fn fork_codex_thread(
        &self,
        session_id: &str,
    ) -> std::result::Result<CreateSessionResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_fork_codex_thread(session_id);
        }

        let context = self.resolve_codex_thread_action_context(session_id)?;
        let fork_result = self.perform_codex_json_rpc_request(
            "thread/fork",
            json!({
                "threadId": context.thread_id,
            }),
            Duration::from_secs(30),
        )?;

        let fork_thread_id = fork_result
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::internal("Codex thread fork did not return a thread id"))?
            .to_owned();
        let fork_name = default_forked_codex_session_name(
            &context.name,
            fork_result.pointer("/thread/name").and_then(Value::as_str),
        );
        let fork_model = fork_result
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(&context.model)
            .to_owned();
        let fork_workdir = resolve_forked_codex_workdir(
            fork_result.get("cwd").and_then(Value::as_str),
            &context.workdir,
            context.project_id.as_deref(),
            self,
        )?;
        let approval_policy = fork_result
            .get("approvalPolicy")
            .and_then(codex_approval_policy_from_json_value)
            .unwrap_or(context.approval_policy);
        let sandbox_mode = fork_result
            .get("sandbox")
            .and_then(codex_sandbox_mode_from_json_value)
            .unwrap_or(context.sandbox_mode);
        let reasoning_effort = fork_result
            .get("reasoningEffort")
            .and_then(codex_reasoning_effort_from_json_value)
            .unwrap_or(context.reasoning_effort);
        let fork_preview = fork_result
            .pointer("/thread/preview")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(make_preview);
        let fork_thread = fork_result
            .get("thread")
            .ok_or_else(|| ApiError::internal("Codex thread fork did not return a thread"))?;

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let fork_messages = codex_thread_messages_from_json(&mut inner, fork_thread);
        let mut record = inner.create_session(
            Agent::Codex,
            Some(fork_name),
            fork_workdir,
            context.project_id.clone(),
            Some(fork_model),
        );
        record.session.model_options = context.model_options.clone();
        record.codex_approval_policy = approval_policy;
        record.session.approval_policy = Some(approval_policy);
        record.codex_sandbox_mode = sandbox_mode;
        record.session.sandbox_mode = Some(sandbox_mode);
        record.codex_reasoning_effort = reasoning_effort;
        record.session.reasoning_effort = Some(reasoning_effort);
        set_record_external_session_id(&mut record, Some(fork_thread_id.clone()));
        if let Some(fork_messages) = fork_messages {
            replace_session_messages_on_record(&mut record, fork_messages, fork_preview);
        } else {
            let note_message_id = inner.next_message_id();
            push_session_markdown_note_on_record(
                &mut record,
                note_message_id,
                "Forked Codex thread",
                format!(
                    "Forked from `{}` into live Codex thread `{}`.\n\nPreview: {}\n\nCodex did not return the earlier thread history for this fork, so TermAl could not backfill the transcript. New prompts here continue on the forked thread from this point forward.",
                    context.name,
                    fork_thread_id,
                    fork_preview
                        .as_deref()
                        .unwrap_or("No thread preview was returned.")
                ),
            );
        }

        if let Some(index) = inner.find_session_index(&record.session.id) {
            if let Some(slot) = inner.sessions.get_mut(index) {
                *slot = record.clone();
            }
            // See `create_session`: re-stamp the record after the
            // whole-struct replace so the persist thread picks up the
            // rewrite instead of skipping it at the delta watermark.
            let _ = inner.session_mut_by_index(index);
        }
        let revision = self.commit_session_created_locked(&mut inner, &record).map_err(|err| {
            ApiError::internal(format!("failed to persist forked Codex session: {err:#}"))
        })?;
        let session = record.session.clone();
        drop(inner);
        self.publish_delta(&DeltaEvent::SessionCreated {
            revision,
            session_id: session.id.clone(),
            session: session.clone(),
        });

        Ok(CreateSessionResponse {
            session_id: session.id.clone(),
            session: Some(session),
            revision,
            state: None,
        })
    }

    /// Archives Codex thread.
    fn archive_codex_thread(
        &self,
        session_id: &str,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_archive_codex_thread(session_id);
        }

        let context = self.resolve_codex_thread_action_context(session_id)?;
        if context.thread_state == Some(CodexThreadState::Archived) {
            return Err(ApiError::conflict(
                "the current Codex thread is already archived",
            ));
        }
        self.perform_codex_json_rpc_request(
            "thread/archive",
            json!({
                "threadId": context.thread_id,
            }),
            Duration::from_secs(30),
        )?;

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let note_message_id = inner.next_message_id();
        set_record_codex_thread_state(inner
            .session_mut_by_index(index)
            .expect("session index should be valid"), CodexThreadState::Archived);
        push_session_markdown_note_on_record(
            inner
            .session_mut_by_index(index)
            .expect("session index should be valid"),
            note_message_id,
            "Archived Codex thread",
            format!(
                "Archived the live Codex thread `{}`.\n\nUse **Unarchive** to restore it later before sending more prompts.",
                context.thread_id
            ),
        );
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!(
                "failed to persist archived Codex thread note: {err:#}"
            ))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }

    /// Unarchives Codex thread.
    fn unarchive_codex_thread(
        &self,
        session_id: &str,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_unarchive_codex_thread(session_id);
        }

        let context = self.resolve_codex_thread_action_context(session_id)?;
        if context.thread_state != Some(CodexThreadState::Archived) {
            return Err(ApiError::conflict(
                "the current Codex thread is not archived",
            ));
        }
        self.perform_codex_json_rpc_request(
            "thread/unarchive",
            json!({
                "threadId": context.thread_id,
            }),
            Duration::from_secs(30),
        )?;

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let note_message_id = inner.next_message_id();
        set_record_codex_thread_state(inner
            .session_mut_by_index(index)
            .expect("session index should be valid"), CodexThreadState::Active);
        push_session_markdown_note_on_record(
            inner
            .session_mut_by_index(index)
            .expect("session index should be valid"),
            note_message_id,
            "Restored Codex thread",
            format!(
                "Restored the archived Codex thread `{}` so the session can continue using it.",
                context.thread_id
            ),
        );
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!(
                "failed to persist restored Codex thread note: {err:#}"
            ))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }

    /// Compacts Codex thread.
    fn compact_codex_thread(
        &self,
        session_id: &str,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_compact_codex_thread(session_id);
        }

        let context = self.resolve_codex_thread_action_context(session_id)?;
        self.perform_codex_json_rpc_request(
            "thread/compact/start",
            json!({
                "threadId": context.thread_id,
            }),
            Duration::from_secs(30),
        )?;

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let note_message_id = inner.next_message_id();
        push_session_markdown_note_on_record(
            inner
            .session_mut_by_index(index)
            .expect("session index should be valid"),
            note_message_id,
            "Started Codex compaction",
            format!(
                "Started Codex context compaction for live thread `{}`.\n\nThe TermAl transcript stays intact, but the live Codex thread may now rely on a compacted summary internally.",
                context.thread_id
            ),
        );
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist Codex compaction note: {err:#}"))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }

    /// Rolls back Codex thread.
    fn rollback_codex_thread(
        &self,
        session_id: &str,
        num_turns: usize,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_rollback_codex_thread(session_id, num_turns);
        }
        if num_turns == 0 {
            return Err(ApiError::bad_request("rollback requires at least one turn"));
        }

        let context = self.resolve_codex_thread_action_context(session_id)?;
        let rollback_result = self.perform_codex_json_rpc_request(
            "thread/rollback",
            json!({
                "threadId": context.thread_id,
                "numTurns": num_turns,
            }),
            Duration::from_secs(30),
        )?;
        let rollback_thread = rollback_result
            .get("thread")
            .ok_or_else(|| ApiError::internal("Codex thread rollback did not return a thread"))?;
        let rollback_preview = rollback_thread
            .get("preview")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(make_preview);

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let rollback_messages = codex_thread_messages_from_json(&mut inner, rollback_thread);
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        if let Some(rollback_messages) = rollback_messages {
            replace_session_messages_on_record(
                inner
            .session_mut_by_index(index)
            .expect("session index should be valid"),
                rollback_messages,
                rollback_preview,
            );
        } else {
            let turn_label = if num_turns == 1 { "turn" } else { "turns" };
            let note_message_id = inner.next_message_id();
            push_session_markdown_note_on_record(
                inner
            .session_mut_by_index(index)
            .expect("session index should be valid"),
                note_message_id,
                "Rolled back Codex thread",
                format!(
                    "Rolled back the live Codex thread `{}` by {} {}.\n\nCodex did not return the updated thread history for this rollback, so TermAl kept the earlier local transcript above. It may not exactly match the live Codex thread after this point.",
                    context.thread_id, num_turns, turn_label
                ),
            );
        }
        inner
            .session_mut_by_index(index)
            .expect("session index should be valid")
            .session
            .status = SessionStatus::Idle;
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist Codex rollback state: {err:#}"))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }

    /// Allocates message ID.
    fn allocate_message_id(&self) -> String {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        inner.next_message_id()
    }

    /// Sets external session ID.
    fn set_external_session_id(&self, session_id: &str, external_session_id: String) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        set_record_external_session_id(inner
            .session_mut_by_index(index)
            .expect("session index should be valid"), Some(external_session_id));
        if inner.sessions[index]
            .session
            .agent
            .supports_codex_prompt_settings()
        {
            let external_session_id = inner.sessions[index].external_session_id.clone();
            inner.allow_discovered_codex_thread(external_session_id.as_deref());
        }
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    /// Sets external session ID only if the session still belongs to the expected runtime.
    ///
    /// Returns the applied/skipped outcome, or `Err` when commit/persistence
    /// fails after the in-memory mutation has begun. This avoids the TOCTOU gap of a separate
    /// `session_matches_runtime_token` check followed by `set_external_session_id`.
    fn set_external_session_id_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        external_session_id: String,
    ) -> Result<RuntimeMatchOutcome> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return Ok(RuntimeMatchOutcome::SessionMissing);
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if !record.runtime.matches_runtime_token(token) {
            return Ok(RuntimeMatchOutcome::RuntimeMismatch);
        }
        set_record_external_session_id(record, Some(external_session_id));
        if inner.sessions[index]
            .session
            .agent
            .supports_codex_prompt_settings()
        {
            let external_session_id = inner.sessions[index].external_session_id.clone();
            inner.allow_discovered_codex_thread(external_session_id.as_deref());
        }
        self.commit_locked(&mut inner)?;
        Ok(RuntimeMatchOutcome::Applied)
    }

    /// Clears external session ID when the expected runtime still owns the session.
    ///
    /// When `suppress_rediscovery` is `true` and the session agent supports Codex
    /// prompt settings, the cleared thread ID is added to the ignored-discovery
    /// set so it does not resurface as a new imported session. Pass `true` for
    /// newly created threads (`thread/start`) that would otherwise be orphaned,
    /// and `false` for resumed pre-existing threads (`thread/resume`) whose
    /// discovery state should be preserved.
    fn clear_external_session_id_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        external_session_id: &str,
        suppress_rediscovery: bool,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if !record.runtime.matches_runtime_token(token) {
            return Ok(());
        }
        if record.external_session_id.as_deref() != Some(external_session_id) {
            return Ok(());
        }

        let should_ignore_thread =
            suppress_rediscovery && record.session.agent.supports_codex_prompt_settings();
        set_record_external_session_id(record, None);
        if should_ignore_thread {
            inner.ignore_discovered_codex_thread(Some(external_session_id));
        }
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    /// Sets Codex thread state if runtime matches.
    fn set_codex_thread_state_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        thread_state: CodexThreadState,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if !record.runtime.matches_runtime_token(token) {
            return Ok(());
        }
        if record.runtime_stop_in_progress {
            return Ok(());
        }

        let next_state = normalized_codex_thread_state(
            record.session.agent,
            record.external_session_id.as_deref(),
            Some(thread_state),
        );
        if record.session.codex_thread_state == next_state {
            return Ok(());
        }

        record.session.codex_thread_state = next_state;
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    /// Syncs session model options.
    fn sync_session_model_options(
        &self,
        session_id: &str,
        current_model: Option<String>,
        model_options: Vec<SessionModelOption>,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");

        let mut changed = false;
        if let Some(current_model) = current_model
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
        {
            if record.session.model != current_model {
                record.session.model = current_model;
                changed = true;
            }
        }
        if record.session.model_options != model_options {
            record.session.model_options = model_options;
            changed = true;
        }
        if record.session.agent.supports_codex_prompt_settings() {
            if let Some(normalized_effort) = normalized_codex_reasoning_effort(
                &record.session.model,
                record.codex_reasoning_effort,
                &record.session.model_options,
            ) {
                if record.codex_reasoning_effort != normalized_effort {
                    record.codex_reasoning_effort = normalized_effort;
                    record.session.reasoning_effort = Some(normalized_effort);
                    changed = true;
                }
            }
        }

        if changed {
            self.commit_locked(&mut inner)?;
        }
        Ok(())
    }

    /// Syncs session agent commands.
    fn sync_session_agent_commands(
        &self,
        session_id: &str,
        agent_commands: Vec<AgentCommand>,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let next_commands = dedupe_agent_commands(agent_commands);
        let should_publish = {
            let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
            if record.agent_commands == next_commands {
                return Ok(());
            }
            record.agent_commands = next_commands;
            if record.hidden {
                false
            } else {
                record.session.agent_commands_revision =
                    record.session.agent_commands_revision.saturating_add(1);
                true
            }
        };
        if should_publish {
            self.commit_locked(&mut inner)?;
        }
        Ok(())
    }

    /// Syncs session cursor mode.
    fn sync_session_cursor_mode(
        &self,
        session_id: &str,
        cursor_mode: Option<CursorMode>,
    ) -> Result<()> {
        let Some(cursor_mode) = cursor_mode else {
            return Ok(());
        };

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if !record.session.agent.supports_cursor_mode()
            || record.session.cursor_mode == Some(cursor_mode)
        {
            return Ok(());
        }

        record.session.cursor_mode = Some(cursor_mode);
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    /// Records Codex rate limits.
    fn note_codex_rate_limits(&self, rate_limits: CodexRateLimits) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if inner.codex.rate_limits.as_ref() == Some(&rate_limits) {
            return Ok(());
        }

        inner.codex.rate_limits = Some(rate_limits);
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    /// Records Codex notice.
    fn note_codex_notice(&self, notice: CodexNotice) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if inner
            .codex
            .notices
            .first()
            .is_some_and(|existing| same_codex_notice_identity(existing, &notice))
        {
            return Ok(());
        }

        if let Some(index) = inner
            .codex
            .notices
            .iter()
            .position(|existing| same_codex_notice_identity(existing, &notice))
        {
            inner.codex.notices.remove(index);
        }

        inner.codex.notices.insert(0, notice);
        inner.codex.notices.truncate(5);
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    /// Records Codex runtime config when the expected runtime still owns the session.
    fn record_codex_runtime_config_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        sandbox_mode: CodexSandboxMode,
        approval_policy: CodexApprovalPolicy,
        reasoning_effort: CodexReasoningEffort,
    ) -> Result<RuntimeMatchOutcome> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return Ok(RuntimeMatchOutcome::SessionMissing);
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if !record.runtime.matches_runtime_token(token) {
            return Ok(RuntimeMatchOutcome::RuntimeMismatch);
        }
        record.active_codex_sandbox_mode = Some(sandbox_mode);
        record.active_codex_approval_policy = Some(approval_policy);
        record.active_codex_reasoning_effort = Some(reasoning_effort);
        self.persist_internal_locked(&inner)?;
        Ok(RuntimeMatchOutcome::Applied)
    }

    /// Handles Claude approval mode.
    fn claude_approval_mode(&self, session_id: &str) -> Result<ClaudeApprovalMode> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        Ok(inner.sessions[index]
            .session
            .claude_approval_mode
            .unwrap_or_else(default_claude_approval_mode))
    }

    /// Handles cursor mode.
    fn cursor_mode(&self, session_id: &str) -> Result<CursorMode> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        Ok(inner.sessions[index]
            .session
            .cursor_mode
            .unwrap_or_else(default_cursor_mode))
    }

    /// Handles session matches runtime token.
    fn session_matches_runtime_token(&self, session_id: &str, token: &RuntimeToken) -> bool {
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner
            .find_session_index(session_id)
            .and_then(|index| inner.sessions.get(index))
            .is_some_and(|record| record.runtime.matches_runtime_token(token))
    }

    /// Clears runtime.
    fn clear_runtime(&self, session_id: &str) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let had_changes = !matches!(record.runtime, SessionRuntime::None)
            || record.runtime_reset_required
            || record.runtime_stop_in_progress
            || has_pending_requests(record);
        if !had_changes {
            return Ok(());
        }

        record.runtime = SessionRuntime::None;
        record.runtime_reset_required = false;
        record.orchestrator_auto_dispatch_blocked = false;
        record.runtime_stop_in_progress = false;
        record.deferred_stop_callbacks.clear();
        clear_active_turn_file_change_tracking(record);
        clear_all_pending_requests(record);
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    /// Marks turn if runtime matches as failed.
    fn fail_turn_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        error_message: &str,
    ) -> Result<()> {
        let cleaned = error_message.trim();
        let should_dispatch_next = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let message_id = (!cleaned.is_empty()).then(|| inner.next_message_id());
            let file_change_message_id = (!inner.sessions[index].active_turn_file_changes.is_empty())
                .then(|| inner.next_message_id());
            let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
            if !record.runtime.matches_runtime_token(token) {
                return Ok(());
            }
            if record.runtime_stop_in_progress {
                record
                    .deferred_stop_callbacks
                    .push(DeferredStopCallback::TurnFailed(cleaned.to_owned()));
                return Ok(());
            }

            if let Some(message_id) = message_id {
                record.session.messages.push(Message::Text {
                    attachments: Vec::new(),
                    id: message_id,
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: format!("Turn failed: {cleaned}"),
                    expanded_text: None,
                });
            }
            if let Some(message_id) = file_change_message_id {
                push_active_turn_file_changes_on_record(record, message_id);
            }

            record.session.status = SessionStatus::Error;
            record.session.preview = make_preview(cleaned);
            finish_active_turn_file_change_tracking(record);
            let has_queued_prompts = !record.queued_prompts.is_empty();
            match self.commit_locked(&mut inner) {
                Ok(_) => {}
                Err(err) => {
                    // Persistence failed but the in-memory state is already
                    // updated. Publish anyway so the frontend sees the error
                    // state instead of being stuck on an active turn.
                    eprintln!(
                        "state warning> failed to persist turn failure for session `{session_id}`, \
                         publishing in-memory state: {err:#}"
                    );
                    self.publish_state_locked(&inner);
                }
            }
            has_queued_prompts
        };

        if should_dispatch_next {
            self.resume_pending_orchestrator_transitions()?;
            if let Some(dispatch) = self.dispatch_next_queued_turn(session_id, false)? {
                deliver_turn_dispatch(self, dispatch).map_err(|err| {
                    anyhow!("failed to deliver queued turn dispatch: {}", err.message)
                })?;
            }
        } else {
            self.resume_pending_orchestrator_transitions()?;
        }

        Ok(())
    }
    /// Records turn retry if runtime matches.
    fn note_turn_retry_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        detail: &str,
    ) -> Result<()> {
        let cleaned = detail.trim();
        if cleaned.is_empty() {
            return Ok(());
        }

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;

        let duplicate_last_message = {
            let record = &inner.sessions[index];
            if !record.runtime.matches_runtime_token(token) {
                return Ok(());
            }
            if record.runtime_stop_in_progress {
                return Ok(());
            }

            matches!(
                record.session.messages.last(),
                Some(Message::Text {
                    author: Author::Assistant,
                    text,
                    ..
                }) if text.trim() == cleaned
            )
        };

        let message_id = (!duplicate_last_message).then(|| inner.next_message_id());
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");

        if let Some(message_id) = message_id {
            record.session.messages.push(Message::Text {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::Assistant,
                text: cleaned.to_owned(),
                expanded_text: None,
            });
        }

        if record.session.status != SessionStatus::Approval {
            record.session.status = SessionStatus::Active;
        }
        record.session.preview = make_preview(cleaned);
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    /// Marks turn error if runtime matches.
    fn mark_turn_error_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        error_message: &str,
    ) -> Result<()> {
        let cleaned = error_message.trim();
        let should_dispatch_next = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let file_change_message_id = (!inner.sessions[index].active_turn_file_changes.is_empty())
                .then(|| inner.next_message_id());
            let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
            if !record.runtime.matches_runtime_token(token) {
                return Ok(());
            }
            if record.runtime_stop_in_progress {
                record
                    .deferred_stop_callbacks
                    .push(DeferredStopCallback::TurnError(cleaned.to_owned()));
                return Ok(());
            }

            record.session.status = SessionStatus::Error;
            if !cleaned.is_empty() {
                record.session.preview = make_preview(cleaned);
            }
            if let Some(message_id) = file_change_message_id {
                push_active_turn_file_changes_on_record(record, message_id);
            }
            finish_active_turn_file_change_tracking(inner
            .session_mut_by_index(index)
            .expect("session index should be valid"));
            let has_queued_prompts = !inner.sessions[index].queued_prompts.is_empty();
            self.commit_locked(&mut inner)?;
            has_queued_prompts
        };
        if should_dispatch_next {
            self.resume_pending_orchestrator_transitions()?;
            if let Some(dispatch) = self.dispatch_next_queued_turn(session_id, false)? {
                deliver_turn_dispatch(self, dispatch).map_err(|err| {
                    anyhow!("failed to deliver queued turn dispatch: {}", err.message)
                })?;
            }
        } else {
            self.resume_pending_orchestrator_transitions()?;
        }

        Ok(())
    }
    /// Finishes turn ok if runtime matches.
    fn finish_turn_ok_if_runtime_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
    ) -> Result<()> {
        let stopping_orchestrator_session_ids = self.stopping_orchestrator_session_ids_snapshot();
        let (should_dispatch_next, orchestrator_delta) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let file_change_message_id = (!inner.sessions[index].active_turn_file_changes.is_empty())
                .then(|| inner.next_message_id());
            let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
            if !record.runtime.matches_runtime_token(token) {
                return Ok(());
            }
            if record.runtime_stop_in_progress {
                record
                    .deferred_stop_callbacks
                    .push(DeferredStopCallback::TurnCompleted);
                return Ok(());
            }

            if record.session.status == SessionStatus::Active {
                record.session.status = SessionStatus::Idle;
            }
            if record.session.preview.trim().is_empty() {
                record.session.preview = "Turn completed.".to_owned();
            }
            let completion_revision = inner.revision.saturating_add(1);
            let orchestrator_changed = schedule_orchestrator_transitions_for_completed_session(
                &mut inner,
                &stopping_orchestrator_session_ids,
                session_id,
                completion_revision,
            );
            if let Some(message_id) = file_change_message_id {
                push_active_turn_file_changes_on_record(inner
            .session_mut_by_index(index)
            .expect("session index should be valid"), message_id);
            }
            finish_active_turn_file_change_tracking(inner
            .session_mut_by_index(index)
            .expect("session index should be valid"));
            self.commit_locked(&mut inner)?;
            let orchestrator_delta = orchestrator_changed
                .then(|| (inner.revision, inner.orchestrator_instances.clone()));
            (true, orchestrator_delta)
        };

        if let Some((revision, orchestrators)) = orchestrator_delta {
            self.publish_orchestrators_updated(revision, orchestrators);
        }

        if should_dispatch_next {
            self.resume_pending_orchestrator_transitions()?;
            if let Some(dispatch) = self.dispatch_next_queued_turn(session_id, false)? {
                deliver_turn_dispatch(self, dispatch).map_err(|err| {
                    anyhow!("failed to deliver queued turn dispatch: {}", err.message)
                })?;
            }
        }

        Ok(())
    }
    /// Handles runtime exit if matches.
    fn handle_runtime_exit_if_matches(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        error_message: Option<&str>,
    ) -> Result<()> {
        let cleaned = error_message.map(str::trim).unwrap_or("");
        let should_dispatch_next = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let matches_runtime = inner.sessions[index].runtime.matches_runtime_token(token);
            if !matches_runtime {
                return Ok(());
            }
            if inner.sessions[index].runtime_stop_in_progress {
                inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid")
                    .deferred_stop_callbacks
                    .push(DeferredStopCallback::RuntimeExited(
                        error_message.map(str::to_owned),
                    ));
                return Ok(());
            }
            let was_busy = matches!(
                inner.sessions[index].session.status,
                SessionStatus::Active | SessionStatus::Approval
            );
            let message_id = (was_busy || !cleaned.is_empty()).then(|| inner.next_message_id());
            let detail = if !cleaned.is_empty() || was_busy {
                Some(if !cleaned.is_empty() {
                    cleaned.to_owned()
                } else {
                    match token {
                        RuntimeToken::Claude(_) => {
                            "Claude session exited before the active turn completed".to_owned()
                        }
                        RuntimeToken::Codex(_) => {
                            "Codex session exited before the active turn completed".to_owned()
                        }
                        RuntimeToken::Acp(_) => {
                            "Agent session exited before the active turn completed".to_owned()
                        }
                    }
                })
            } else {
                None
            };
            let file_change_message_id = (!inner.sessions[index].active_turn_file_changes.is_empty())
                .then(|| inner.next_message_id());
            let has_queued_prompts = {
                let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
                record.runtime = SessionRuntime::None;
                record.runtime_reset_required = false;
                record.orchestrator_auto_dispatch_blocked = false;
                record.runtime_stop_in_progress = false;
                record.deferred_stop_callbacks.clear();
                cancel_pending_interaction_messages(&mut record.session.messages);
                clear_all_pending_requests(record);
                if let Some(detail) = detail.as_ref() {
                    if let Some(message_id) = message_id {
                        record.session.messages.push(Message::Text {
                            attachments: Vec::new(),
                            id: message_id,
                            timestamp: stamp_now(),
                            author: Author::Assistant,
                            text: format!("Turn failed: {detail}"),
                            expanded_text: None,
                        });
                    }
                    record.session.status = SessionStatus::Error;
                    record.session.preview = make_preview(detail);
                }
                if let Some(message_id) = file_change_message_id {
                    push_active_turn_file_changes_on_record(record, message_id);
                }
                !record.queued_prompts.is_empty()
            };
            finish_active_turn_file_change_tracking(inner
            .session_mut_by_index(index)
            .expect("session index should be valid"));
            self.commit_locked(&mut inner)?;
            has_queued_prompts
        };

        if should_dispatch_next {
            self.resume_pending_orchestrator_transitions()?;
            if let Some(dispatch) = self.dispatch_next_queued_turn(session_id, false)? {
                deliver_turn_dispatch(self, dispatch).map_err(|err| {
                    anyhow!("failed to deliver queued turn dispatch: {}", err.message)
                })?;
            }
        } else {
            self.resume_pending_orchestrator_transitions()?;
        }

        Ok(())
    }

    /// Registers Claude pending approval.
    fn register_claude_pending_approval(
        &self,
        session_id: &str,
        message_id: String,
        approval: ClaudePendingApproval,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_claude_approvals
            .insert(message_id, approval);
        Ok(())
    }

    /// Registers Codex pending approval.
    fn register_codex_pending_approval(
        &self,
        session_id: &str,
        message_id: String,
        approval: CodexPendingApproval,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_codex_approvals
            .insert(message_id, approval);
        Ok(())
    }

    /// Registers Codex pending user input.
    fn register_codex_pending_user_input(
        &self,
        session_id: &str,
        message_id: String,
        request: CodexPendingUserInput,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_codex_user_inputs
            .insert(message_id, request);
        Ok(())
    }

    /// Registers Codex pending MCP elicitation.
    fn register_codex_pending_mcp_elicitation(
        &self,
        session_id: &str,
        message_id: String,
        request: CodexPendingMcpElicitation,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_codex_mcp_elicitations
            .insert(message_id, request);
        Ok(())
    }

    /// Registers Codex pending app request.
    fn register_codex_pending_app_request(
        &self,
        session_id: &str,
        message_id: String,
        request: CodexPendingAppRequest,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_codex_app_requests
            .insert(message_id, request);
        Ok(())
    }

    /// Registers ACP pending approval.
    fn register_acp_pending_approval(
        &self,
        session_id: &str,
        message_id: String,
        approval: AcpPendingApproval,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        inner.sessions[index]
            .pending_acp_approvals
            .insert(message_id, approval);
        Ok(())
    }

    /// Clears Claude pending approval by request.
    fn clear_claude_pending_approval_by_request(
        &self,
        session_id: &str,
        request_id: &str,
    ) -> Result<()> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let message_ids: Vec<String> = record
            .pending_claude_approvals
            .iter()
            .filter(|(_, approval)| approval.request_id == request_id)
            .map(|(message_id, _)| message_id.clone())
            .collect();

        if message_ids.is_empty() {
            return Ok(());
        }

        for message_id in &message_ids {
            set_approval_decision_on_record(record, message_id, ApprovalDecision::Canceled)?;
            record.pending_claude_approvals.remove(message_id);
        }

        sync_session_interaction_state(
            record,
            approval_preview_text(record.session.agent.name(), ApprovalDecision::Canceled),
        );
        self.commit_locked(&mut inner)?;
        Ok(())
    }

    /// Kills session.
    fn kill_session(&self, session_id: &str) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_kill_session(session_id);
        }
        let (runtime_to_kill, hidden_runtimes_to_kill) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let workdir = inner.sessions[index].session.workdir.clone();
            let project_id = inner.sessions[index].session.project_id.clone();
            let agent = inner.sessions[index].session.agent;
            let external_session_id = inner.sessions[index].external_session_id.clone();
            let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");

            let runtime = match &record.runtime {
                SessionRuntime::Claude(handle) => Some(KillableRuntime::Claude(handle.clone())),
                SessionRuntime::Codex(handle) => Some(KillableRuntime::Codex(handle.clone())),
                SessionRuntime::Acp(handle) => Some(KillableRuntime::Acp(handle.clone())),
                SessionRuntime::None => None,
            };
            inner.remove_session_at(index);

            let mut hidden_runtimes = Vec::new();
            if agent == Agent::Claude {
                let visible_profiles = inner
                    .sessions
                    .iter()
                    .filter(|session_record| {
                        !session_record.hidden
                            && !session_record.is_remote_proxy()
                            && session_record.session.agent == Agent::Claude
                            && session_record.session.workdir == workdir
                            && session_record.session.project_id == project_id
                    })
                    .map(claude_spare_profile)
                    .collect::<Vec<_>>();
                inner.retain_sessions(|session_record| {
                    let should_consider = session_record.hidden
                        && !session_record.is_remote_proxy()
                        && session_record.session.agent == Agent::Claude
                        && session_record.session.workdir == workdir
                        && session_record.session.project_id == project_id;
                    if !should_consider {
                        return true;
                    }

                    let keep = visible_profiles
                        .iter()
                        .any(|profile| *profile == claude_spare_profile(session_record));
                    if !keep {
                        if let SessionRuntime::Claude(handle) = &session_record.runtime {
                            hidden_runtimes.push(KillableRuntime::Claude(handle.clone()));
                        }
                    }
                    keep
                });
            }

            if agent.supports_codex_prompt_settings() {
                inner.ignore_discovered_codex_thread(external_session_id.as_deref());
            }
            inner.normalize_orchestrator_instances();

            self.commit_locked(&mut inner).map_err(|err| {
                ApiError::internal(format!("failed to persist session state: {err:#}"))
            })?;
            (runtime, hidden_runtimes)
        };

        if let Some(runtime) = runtime_to_kill {
            if let Err(err) = shutdown_removed_runtime(runtime, &format!("session `{session_id}`"))
            {
                eprintln!("session cleanup warning> {err:#}");
            }
        }
        for runtime in hidden_runtimes_to_kill {
            if let Err(err) = shutdown_removed_runtime(runtime, "a hidden Claude spare") {
                eprintln!("session cleanup warning> {err:#}");
            }
        }

        self.resume_pending_orchestrator_transitions()
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to reconcile orchestrator transitions: {err:#}"
                ))
            })?;
        Ok(self.snapshot())
    }

    /// Cancels queued prompt.
    fn cancel_queued_prompt(
        &self,
        session_id: &str,
        prompt_id: &str,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_cancel_queued_prompt(session_id, prompt_id);
        }
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let original_len = record.queued_prompts.len();
        record
            .queued_prompts
            .retain(|queued| queued.pending_prompt.id != prompt_id);
        if record.queued_prompts.len() == original_len {
            return Err(ApiError::not_found("queued prompt not found"));
        }
        sync_pending_prompts(record);

        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;
        drop(inner);
        self.resume_pending_orchestrator_transitions()
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to reconcile orchestrator transitions: {err:#}"
                ))
            })?;
        Ok(self.snapshot())
    }

    /// Stops session.
    fn stop_session(&self, session_id: &str) -> std::result::Result<StateResponse, ApiError> {
        self.stop_session_with_options(session_id, StopSessionOptions::default())
    }

    /// Stops session with options.
    fn stop_session_with_options(
        &self,
        session_id: &str,
        options: StopSessionOptions,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_stop_session(session_id);
        }
        let (runtime_to_stop, stop_failure_is_best_effort) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");

            if record.runtime_stop_in_progress {
                return Err(ApiError::conflict("session is already stopping"));
            }

            if !matches!(
                record.session.status,
                SessionStatus::Active | SessionStatus::Approval
            ) {
                return Err(ApiError::conflict(SESSION_NOT_RUNNING_CONFLICT_MESSAGE));
            }

            let runtime = match &record.runtime {
                SessionRuntime::Claude(handle) => KillableRuntime::Claude(handle.clone()),
                SessionRuntime::Codex(handle) => KillableRuntime::Codex(handle.clone()),
                SessionRuntime::Acp(handle) => KillableRuntime::Acp(handle.clone()),
                SessionRuntime::None => {
                    return Err(ApiError::conflict(SESSION_NOT_RUNNING_CONFLICT_MESSAGE));
                }
            };
            let stop_failure_is_best_effort = runtime.stop_failure_is_best_effort();

            // Preserve the public session status until the stop succeeds so borrowed state reads
            // never observe a contradictory transient Idle snapshot while shutdown is still pending.
            // `deferred_stop_callbacks` is guaranteed to be empty here because the guard above
            // already returned if `runtime_stop_in_progress` was true (and callbacks can only
            // defer when that flag is set).
            record.runtime_stop_in_progress = true;

            (runtime, stop_failure_is_best_effort)
        };

        let mut clear_external_session_id = false;
        if let Err(err) =
            shutdown_removed_runtime(runtime_to_stop, &format!("session `{session_id}`"))
        {
            if stop_failure_is_best_effort {
                eprintln!(
                    "session cleanup warning> failed to stop session `{session_id}` cleanly: {err:#}"
                );
                clear_external_session_id = true;
            } else {
                let (mut deferred_callbacks, token) = {
                    let mut inner = self.inner.lock().expect("state mutex poisoned");
                    let index = inner
                        .find_visible_session_index(session_id)
                        .ok_or_else(|| ApiError::not_found("session not found"))?;
                    let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
                    record.runtime_stop_in_progress = false;
                    let deferred_callbacks = std::mem::take(&mut record.deferred_stop_callbacks);
                    let token = record.runtime.runtime_token();
                    (deferred_callbacks, token)
                };

                // Replay any terminal callbacks that arrived during the failed shutdown window.
                // The flag is now cleared so the callback methods will proceed normally.
                if let Some(token) = token {
                    deferred_callbacks.sort_by_key(|deferred| {
                        matches!(deferred, DeferredStopCallback::RuntimeExited(_))
                    });
                    for deferred in deferred_callbacks {
                        let replay_result = match deferred {
                            DeferredStopCallback::TurnFailed(msg) => {
                                self.fail_turn_if_runtime_matches(session_id, &token, &msg)
                            }
                            DeferredStopCallback::TurnError(msg) => {
                                self.mark_turn_error_if_runtime_matches(session_id, &token, &msg)
                            }
                            DeferredStopCallback::TurnCompleted => {
                                self.finish_turn_ok_if_runtime_matches(session_id, &token)
                            }
                            DeferredStopCallback::RuntimeExited(msg) => self
                                .handle_runtime_exit_if_matches(session_id, &token, msg.as_deref()),
                        };
                        if let Err(replay_err) = replay_result {
                            eprintln!(
                                "session cleanup warning> failed to replay deferred stop callback \
                                 for session `{session_id}`: {replay_err:#}"
                            );
                        }
                    }
                }

                return Err(ApiError::internal(format!(
                    "failed to stop session `{session_id}` cleanly: {err:#}"
                )));
            }
        }
        let orchestrator_stop_instance_id = options.orchestrator_stop_instance_id.clone();
        let should_dispatch_next = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let message_id = inner.next_message_id();
            let file_change_message_id = (!inner.sessions[index].active_turn_file_changes.is_empty())
                .then(|| inner.next_message_id());
            let mut thread_id_to_suppress = None;
            {
                let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
                record.runtime = SessionRuntime::None;
                record.runtime_reset_required = false;
                record.runtime_stop_in_progress = false;
                record.deferred_stop_callbacks.clear();
                cancel_pending_interaction_messages(&mut record.session.messages);
                clear_all_pending_requests(record);
                if clear_external_session_id {
                    // Interrupt failures can leave the detached Codex thread running, so any
                    // queued or future prompt must start a fresh thread instead of resuming it.
                    // Capture the thread id before clearing so we can suppress its rediscovery
                    // after the record borrow is released.
                    if record.session.agent.supports_codex_prompt_settings() {
                        thread_id_to_suppress = record.external_session_id.clone();
                    }
                    set_record_external_session_id(record, None);
                }
                record.session.status = SessionStatus::Idle;
                record.session.preview = "Turn stopped by user.".to_owned();
                record.session.messages.push(Message::Text {
                    attachments: Vec::new(),
                    id: message_id,
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: "Turn stopped by user.".to_owned(),
                    expanded_text: None,
                });
                if let Some(message_id) = file_change_message_id {
                    push_active_turn_file_changes_on_record(record, message_id);
                }
            }

            // Suppress rediscovery of the detached thread after the record
            // borrow is released. Without this, the still-running thread
            // would resurface as a new imported session on the next
            // import_discovered_codex_threads pass.
            if let Some(ref thread_id) = thread_id_to_suppress {
                inner.ignore_discovered_codex_thread(Some(thread_id));
            }

            finish_active_turn_file_change_tracking(inner
            .session_mut_by_index(index)
            .expect("session index should be valid"));
            let mut stopped_orchestrator_instance_index = None;
            let mut added_stopped_session_id = false;
            if let Some(orchestrator_instance_id) = orchestrator_stop_instance_id.as_deref() {
                if let Some(instance_index) = inner
                    .orchestrator_instances
                    .iter()
                    .position(|instance| instance.id == orchestrator_instance_id)
                {
                    stopped_orchestrator_instance_index = Some(instance_index);
                    let stopped_session_ids = &mut inner.orchestrator_instances[instance_index]
                        .stopped_session_ids_during_stop;
                    if !stopped_session_ids
                        .iter()
                        .any(|candidate| candidate == session_id)
                    {
                        stopped_session_ids.push(session_id.to_owned());
                        stopped_session_ids.sort();
                        added_stopped_session_id = true;
                    }
                }
            }
            let has_queued_prompts = options.dispatch_queued_prompts_on_success
                && !inner.sessions[index].queued_prompts.is_empty();
            if let Err(err) = self.commit_locked(&mut inner) {
                if added_stopped_session_id {
                    if let Some(instance_index) = stopped_orchestrator_instance_index {
                        inner.orchestrator_instances[instance_index]
                            .stopped_session_ids_during_stop
                            .retain(|candidate| candidate != session_id);
                    }
                }
                inner
                    .session_mut_by_index(index)
                    .expect("session index should be valid")
                    .orchestrator_auto_dispatch_blocked = true;
                return Err(ApiError::internal(format!(
                    "failed to persist session state: {err:#}"
                )));
            }
            has_queued_prompts
        };

        if let Some(orchestrator_instance_id) = orchestrator_stop_instance_id.as_deref() {
            self.note_stopped_orchestrator_session(orchestrator_instance_id, session_id);
        }

        if should_dispatch_next {
            if let Some(dispatch) =
                self.dispatch_next_queued_turn(session_id, false)
                    .map_err(|err| {
                        ApiError::internal(format!("failed to dispatch queued prompt: {err:#}"))
                    })?
            {
                deliver_turn_dispatch(self, dispatch)?;
            }
        }

        Ok(self.snapshot())
    }

    /// Pushes message.
    fn push_message(&self, session_id: &str, message: Message) -> Result<()> {
        let (revision, message, message_index, preview, status) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let (message_index, preview, status) = {
                let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
                if let Some(next_preview) = message.preview_text() {
                    record.session.preview = next_preview;
                }
                if matches!(
                    message,
                    Message::Approval { .. }
                        | Message::UserInputRequest { .. }
                        | Message::McpElicitationRequest { .. }
                        | Message::CodexAppRequest { .. }
                ) {
                    record.session.status = SessionStatus::Approval;
                }
                let message_index = push_message_on_record(record, message.clone());
                (
                    message_index,
                    record.session.preview.clone(),
                    record.session.status,
                )
            };
            let revision = self.commit_persisted_delta_locked(&mut inner)?;
            (revision, message, message_index, preview, status)
        };

        self.publish_delta(&DeltaEvent::MessageCreated {
            revision,
            session_id: session_id.to_owned(),
            message_id: message.id().to_owned(),
            message_index,
            message,
            preview,
            status,
        });
        Ok(())
    }

    /// Returns the last message ID.
    pub(crate) fn last_message_id(&self, session_id: &str) -> Result<Option<String>> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        Ok(inner.sessions[index]
            .session
            .messages
            .last()
            .map(|message| message.id().to_owned()))
    }

    /// Handles insert message before.
    pub(crate) fn insert_message_before(
        &self,
        session_id: &str,
        anchor_message_id: &str,
        message: Message,
    ) -> Result<()> {
        let (revision, message, message_index, preview, status) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let (message_index, preview, status) = {
                let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
                let anchor_index =
                    message_index_on_record(record, anchor_message_id).ok_or_else(|| {
                        anyhow!(
                            "session `{session_id}` anchor message `{anchor_message_id}` not found"
                        )
                    })?;
                // This insertion path is currently reserved for subagent-result messages that do
                // not contribute preview/status text. Keep the existing session preview/status in
                // the emitted delta unless a future caller explicitly broadens that contract.
                let message_index = insert_message_on_record(record, anchor_index, message.clone());
                (
                    message_index,
                    record.session.preview.clone(),
                    record.session.status,
                )
            };
            let revision = self.commit_persisted_delta_locked(&mut inner)?;
            (revision, message, message_index, preview, status)
        };

        self.publish_delta(&DeltaEvent::MessageCreated {
            revision,
            session_id: session_id.to_owned(),
            message_id: message.id().to_owned(),
            message_index,
            message,
            preview,
            status,
        });
        Ok(())
    }

    /// Appends text delta.
    fn append_text_delta(&self, session_id: &str, message_id: &str, delta: &str) -> Result<()> {
        let (preview, revision, message_index) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
            let message_index = message_index_on_record(record, message_id).ok_or_else(|| {
                anyhow!("session `{session_id}` message `{message_id}` not found")
            })?;
            let session = &mut record.session;

            let mut preview = None;
            let Some(message) = session.messages.get_mut(message_index) else {
                return Err(anyhow!(
                    "session `{session_id}` message index `{message_index}` is out of bounds"
                ));
            };
            match message {
                Message::Text { id, text, .. } if id == message_id => {
                    text.push_str(delta);
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        preview = Some(make_preview(trimmed));
                    }
                }
                _ => {
                    return Err(anyhow!(
                        "session `{session_id}` message `{message_id}` is not a text message"
                    ));
                }
            }

            if let Some(next_preview) = preview.as_ref() {
                session.preview = next_preview.clone();
            }
            let revision = self.commit_delta_locked(&mut inner)?;
            (preview, revision, message_index)
        };

        self.publish_delta(&DeltaEvent::TextDelta {
            revision,
            session_id: session_id.to_owned(),
            message_id: message_id.to_owned(),
            message_index,
            delta: delta.to_owned(),
            preview,
        });

        Ok(())
    }

    /// Replaces text message.
    fn replace_text_message(&self, session_id: &str, message_id: &str, text: &str) -> Result<()> {
        let (preview, revision, message_index, replacement_text) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
            let message_index = message_index_on_record(record, message_id).ok_or_else(|| {
                anyhow!("session `{session_id}` message `{message_id}` not found")
            })?;
            let session = &mut record.session;

            let mut preview = None;
            let Some(message) = session.messages.get_mut(message_index) else {
                return Err(anyhow!(
                    "session `{session_id}` message index `{message_index}` is out of bounds"
                ));
            };
            match message {
                Message::Text {
                    id,
                    text: current_text,
                    ..
                } if id == message_id => {
                    current_text.clear();
                    current_text.push_str(text);
                    let trimmed = current_text.trim();
                    if !trimmed.is_empty() {
                        preview = Some(make_preview(trimmed));
                    }
                }
                _ => {
                    return Err(anyhow!(
                        "session `{session_id}` message `{message_id}` is not a text message"
                    ));
                }
            }

            if let Some(next_preview) = preview.as_ref() {
                session.preview = next_preview.clone();
            }
            let revision = self.commit_delta_locked(&mut inner)?;
            (preview, revision, message_index, text.to_owned())
        };

        self.publish_delta(&DeltaEvent::TextReplace {
            revision,
            session_id: session_id.to_owned(),
            message_id: message_id.to_owned(),
            message_index,
            text: replacement_text,
            preview,
        });

        Ok(())
    }

    /// Upserts command message.
    fn upsert_command_message(
        &self,
        session_id: &str,
        message_id: &str,
        command: &str,
        output: &str,
        status: CommandStatus,
    ) -> Result<()> {
        let command_language = Some(shell_language().to_owned());
        let output_language = infer_command_output_language(command).map(str::to_owned);

        let (preview, revision, message_index, created_message, session_status) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let (message_index, created_message, preview, session_status) = {
                let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
                let (message_index, created_message) = if let Some(message_index) =
                    message_index_on_record(record, message_id)
                {
                    let Some(message) = record.session.messages.get_mut(message_index) else {
                        return Err(anyhow!(
                            "session `{session_id}` message index `{message_index}` is out of bounds"
                        ));
                    };
                    match message {
                        Message::Command {
                            id,
                            command: existing_command,
                            command_language: existing_command_language,
                            output: existing_output,
                            output_language: existing_output_language,
                            status: existing_status,
                            ..
                        } if id == message_id => {
                            *existing_command = command.to_owned();
                            *existing_command_language = command_language.clone();
                            *existing_output = output.to_owned();
                            *existing_output_language = output_language.clone();
                            *existing_status = status;
                            (message_index, None)
                        }
                        _ => {
                            return Err(anyhow!(
                                "session `{session_id}` message `{message_id}` is not a command message"
                            ));
                        }
                    }
                } else {
                    let message = Message::Command {
                        id: message_id.to_owned(),
                        timestamp: stamp_now(),
                        author: Author::Assistant,
                        command: command.to_owned(),
                        command_language: command_language.clone(),
                        output: output.to_owned(),
                        output_language: output_language.clone(),
                        status,
                    };
                    let message_index = push_message_on_record(record, message.clone());
                    (message_index, Some(message))
                };

                let preview = match status {
                    CommandStatus::Running => make_preview(&format!("Running {command}")),
                    CommandStatus::Success => {
                        if output.trim().is_empty() {
                            make_preview(&format!("Completed {command}"))
                        } else {
                            make_preview(output.trim())
                        }
                    }
                    CommandStatus::Error => {
                        if output.trim().is_empty() {
                            make_preview(&format!("Command failed: {command}"))
                        } else {
                            make_preview(output.trim())
                        }
                    }
                };
                record.session.preview = preview.clone();
                (
                    message_index,
                    created_message,
                    preview,
                    record.session.status,
                )
            };
            let revision = if created_message.is_some() {
                self.commit_persisted_delta_locked(&mut inner)?
            } else {
                self.commit_delta_locked(&mut inner)?
            };
            (
                preview,
                revision,
                message_index,
                created_message,
                session_status,
            )
        };

        if let Some(message) = created_message {
            self.publish_delta(&DeltaEvent::MessageCreated {
                revision,
                session_id: session_id.to_owned(),
                message_id: message_id.to_owned(),
                message_index,
                message,
                preview,
                status: session_status,
            });
        } else {
            self.publish_delta(&DeltaEvent::CommandUpdate {
                revision,
                session_id: session_id.to_owned(),
                message_id: message_id.to_owned(),
                message_index,
                command: command.to_owned(),
                command_language,
                output: output.to_owned(),
                output_language,
                status,
                preview,
            });
        }

        Ok(())
    }

    /// Upserts parallel agents message.
    fn upsert_parallel_agents_message(
        &self,
        session_id: &str,
        message_id: &str,
        agents: Vec<ParallelAgentProgress>,
    ) -> Result<()> {
        let (preview, revision, message_index, created_message, session_status) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let (message_index, created_message, preview, session_status) = {
                let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");

                let (message_index, created_message) = if let Some(message_index) =
                    message_index_on_record(record, message_id)
                {
                    let Some(message) = record.session.messages.get_mut(message_index) else {
                        return Err(anyhow!(
                            "session `{session_id}` message index `{message_index}` is out of bounds"
                        ));
                    };
                    match message {
                        Message::ParallelAgents {
                            id,
                            agents: existing_agents,
                            ..
                        } if id == message_id => {
                            *existing_agents = agents.clone();
                            (message_index, None)
                        }
                        _ => {
                            return Err(anyhow!(
                                "session `{session_id}` message `{message_id}` is not a parallel-agents message"
                            ));
                        }
                    }
                } else {
                    let message = Message::ParallelAgents {
                        id: message_id.to_owned(),
                        timestamp: stamp_now(),
                        author: Author::Assistant,
                        agents: agents.clone(),
                    };
                    let message_index = push_message_on_record(record, message.clone());
                    (message_index, Some(message))
                };

                let preview = parallel_agents_preview_text(&agents);
                record.session.preview = preview.clone();
                (
                    message_index,
                    created_message,
                    preview,
                    record.session.status,
                )
            };
            let revision = if created_message.is_some() {
                self.commit_persisted_delta_locked(&mut inner)?
            } else {
                self.commit_delta_locked(&mut inner)?
            };
            (
                preview,
                revision,
                message_index,
                created_message,
                session_status,
            )
        };

        if let Some(message) = created_message {
            self.publish_delta(&DeltaEvent::MessageCreated {
                revision,
                session_id: session_id.to_owned(),
                message_id: message_id.to_owned(),
                message_index,
                message,
                preview,
                status: session_status,
            });
        } else {
            self.publish_delta(&DeltaEvent::ParallelAgentsUpdate {
                revision,
                session_id: session_id.to_owned(),
                message_id: message_id.to_owned(),
                message_index,
                agents,
                preview,
            });
        }

        Ok(())
    }

    /// Updates approval.
    fn update_approval(
        &self,
        session_id: &str,
        message_id: &str,
        decision: ApprovalDecision,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_update_approval(session_id, message_id, decision);
        }
        if matches!(
            decision,
            ApprovalDecision::Pending | ApprovalDecision::Interrupted | ApprovalDecision::Canceled
        ) {
            return Err(ApiError::bad_request(
                "approval decisions cannot be marked pending, interrupted, or canceled manually",
            ));
        }

        let mut claude_runtime_action: Option<(ClaudeRuntimeHandle, ClaudePendingApproval)> = None;
        let mut codex_runtime_action: Option<(CodexRuntimeHandle, CodexPendingApproval)> = None;
        let mut acp_runtime_action: Option<(AcpRuntimeHandle, AcpPendingApproval)> = None;
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_visible_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if record.session.status != SessionStatus::Approval {
            return Err(ApiError::conflict(
                "session is not currently awaiting approval",
            ));
        }

        if record.session.agent == Agent::Claude
            && matches!(
                decision,
                ApprovalDecision::Accepted
                    | ApprovalDecision::AcceptedForSession
                    | ApprovalDecision::Rejected
            )
        {
            let pending = record
                .pending_claude_approvals
                .get(message_id)
                .cloned()
                .ok_or_else(|| ApiError::conflict("approval request is no longer live"))?;
            let handle = match &record.runtime {
                SessionRuntime::Claude(handle) => handle.clone(),
                SessionRuntime::Codex(_) => {
                    return Err(ApiError::conflict(
                        "Claude session is not currently running",
                    ));
                }
                SessionRuntime::None => {
                    return Err(ApiError::conflict(
                        "Claude session is not currently running",
                    ));
                }
                SessionRuntime::Acp(_) => {
                    return Err(ApiError::conflict(
                        "Claude session is not currently running",
                    ));
                }
            };
            claude_runtime_action = Some((handle, pending));
        } else if record.session.agent == Agent::Codex
            && matches!(
                decision,
                ApprovalDecision::Accepted
                    | ApprovalDecision::AcceptedForSession
                    | ApprovalDecision::Rejected
            )
        {
            let pending = record
                .pending_codex_approvals
                .get(message_id)
                .cloned()
                .ok_or_else(|| ApiError::conflict("approval request is no longer live"))?;
            let handle = match &record.runtime {
                SessionRuntime::Codex(handle) => handle.clone(),
                SessionRuntime::Claude(_) | SessionRuntime::None | SessionRuntime::Acp(_) => {
                    return Err(ApiError::conflict("Codex session is not currently running"));
                }
            };
            codex_runtime_action = Some((handle, pending));
        } else if matches!(record.session.agent, Agent::Cursor | Agent::Gemini)
            && matches!(
                decision,
                ApprovalDecision::Accepted
                    | ApprovalDecision::AcceptedForSession
                    | ApprovalDecision::Rejected
            )
        {
            let pending = record
                .pending_acp_approvals
                .get(message_id)
                .cloned()
                .ok_or_else(|| ApiError::conflict("approval request is no longer live"))?;
            let handle = match &record.runtime {
                SessionRuntime::Acp(handle) => handle.clone(),
                SessionRuntime::Claude(_) | SessionRuntime::Codex(_) | SessionRuntime::None => {
                    return Err(ApiError::conflict("agent session is not currently running"));
                }
            };
            acp_runtime_action = Some((handle, pending));
        }

        drop(inner);

        if let Some((handle, pending)) = claude_runtime_action {
            if decision == ApprovalDecision::AcceptedForSession {
                if let Some(mode) = pending.permission_mode_for_session.clone() {
                    handle
                        .input_tx
                        .send(ClaudeRuntimeCommand::SetPermissionMode(mode))
                        .map_err(|err| {
                            ApiError::internal(format!(
                                "failed to update Claude permission mode: {err}"
                            ))
                        })?;
                }
            }

            let response = match decision {
                ApprovalDecision::Accepted | ApprovalDecision::AcceptedForSession => {
                    ClaudePermissionDecision::Allow {
                        request_id: pending.request_id.clone(),
                        updated_input: pending.tool_input.clone(),
                    }
                }
                ApprovalDecision::Rejected => ClaudePermissionDecision::Deny {
                    request_id: pending.request_id.clone(),
                    message: "User rejected this action in TermAl.".to_owned(),
                },
                ApprovalDecision::Pending
                | ApprovalDecision::Interrupted
                | ApprovalDecision::Canceled => {
                    unreachable!("non-deliverable approval decisions are not sent")
                }
            };

            handle
                .input_tx
                .send(ClaudeRuntimeCommand::PermissionResponse(response))
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to deliver approval response to Claude: {err}"
                    ))
                })?;
        }
        if let Some((handle, pending)) = codex_runtime_action {
            handle
                .input_tx
                .send(CodexRuntimeCommand::JsonRpcResponse {
                    response: CodexJsonRpcResponseCommand {
                        request_id: pending.request_id.clone(),
                        payload: CodexJsonRpcResponsePayload::Result(codex_approval_result(
                            &pending.kind,
                            decision,
                        )),
                    },
                })
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to deliver approval response to Codex: {err}"
                    ))
                })?;
        }
        if let Some((handle, pending)) = acp_runtime_action {
            let option_id = match decision {
                ApprovalDecision::Accepted => pending
                    .allow_once_option_id
                    .clone()
                    .or_else(|| pending.allow_always_option_id.clone())
                    .or_else(|| pending.reject_option_id.clone()),
                ApprovalDecision::AcceptedForSession => pending
                    .allow_always_option_id
                    .clone()
                    .or_else(|| pending.allow_once_option_id.clone())
                    .or_else(|| pending.reject_option_id.clone()),
                ApprovalDecision::Rejected => pending
                    .reject_option_id
                    .clone()
                    .or_else(|| pending.allow_once_option_id.clone())
                    .or_else(|| pending.allow_always_option_id.clone()),
                ApprovalDecision::Pending
                | ApprovalDecision::Interrupted
                | ApprovalDecision::Canceled => None,
            }
            .ok_or_else(|| {
                ApiError::conflict("no approval option is available for this request")
            })?;

            handle
                .input_tx
                .send(AcpRuntimeCommand::JsonRpcMessage(
                    json_rpc_result_response_message(
                        pending.request_id.clone(),
                        json!({
                            "outcome": {
                                "outcome": "selected",
                                "optionId": option_id,
                            }
                        }),
                    ),
                ))
                .map_err(|err| {
                    ApiError::internal(format!(
                        "failed to deliver approval response to agent session: {err}"
                    ))
                })?;
        }

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if record.session.status != SessionStatus::Approval && decision == ApprovalDecision::Pending
        {
            return Err(ApiError::conflict(
                "session is not currently awaiting approval",
            ));
        }
        set_approval_decision_on_record(record, message_id, decision)
            .map_err(|_| ApiError::not_found("approval message not found"))?;

        if decision != ApprovalDecision::Pending {
            record.pending_claude_approvals.remove(message_id);
            record.pending_codex_approvals.remove(message_id);
            record.pending_acp_approvals.remove(message_id);
        }
        sync_session_interaction_state(
            record,
            approval_preview_text(record.session.agent.name(), decision),
        );
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }

    /// Submits Codex user input.
    fn submit_codex_user_input(
        &self,
        session_id: &str,
        message_id: &str,
        answers: BTreeMap<String, Vec<String>>,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_submit_codex_user_input(session_id, message_id, answers);
        }

        let (handle, pending, response_answers, display_answers) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
            if record.session.status != SessionStatus::Approval {
                return Err(ApiError::conflict(
                    "session is not currently waiting for input",
                ));
            }
            if record.session.agent != Agent::Codex {
                return Err(ApiError::conflict(
                    "only Codex sessions currently support structured user input",
                ));
            }

            let pending = record
                .pending_codex_user_inputs
                .get(message_id)
                .cloned()
                .ok_or_else(|| ApiError::conflict("user input request is no longer live"))?;
            let handle = match &record.runtime {
                SessionRuntime::Codex(handle) => handle.clone(),
                SessionRuntime::Claude(_) | SessionRuntime::None | SessionRuntime::Acp(_) => {
                    return Err(ApiError::conflict("Codex session is not currently running"));
                }
            };
            let (response_answers, display_answers) =
                validate_codex_user_input_answers(&pending.questions, answers)?;
            (handle, pending, response_answers, display_answers)
        };

        handle
            .input_tx
            .send(CodexRuntimeCommand::JsonRpcResponse {
                response: CodexJsonRpcResponseCommand {
                    request_id: pending.request_id.clone(),
                    payload: CodexJsonRpcResponsePayload::Result(
                        json!({ "answers": response_answers }),
                    ),
                },
            })
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to deliver user input response to Codex: {err}"
                ))
            })?;

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        set_user_input_request_state_on_record(
            record,
            message_id,
            InteractionRequestState::Submitted,
            Some(display_answers),
        )
        .map_err(|_| ApiError::not_found("user input request not found"))?;
        record.pending_codex_user_inputs.remove(message_id);
        sync_session_interaction_state(
            record,
            user_input_request_preview_text(
                record.session.agent.name(),
                InteractionRequestState::Submitted,
            ),
        );
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }

    /// Submits Codex MCP elicitation.
    fn submit_codex_mcp_elicitation(
        &self,
        session_id: &str,
        message_id: &str,
        action: McpElicitationAction,
        content: Option<Value>,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_submit_codex_mcp_elicitation(
                session_id, message_id, action, content,
            );
        }

        let (handle, pending, normalized_content) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
            if record.session.status != SessionStatus::Approval {
                return Err(ApiError::conflict(
                    "session is not currently waiting for input",
                ));
            }
            if record.session.agent != Agent::Codex {
                return Err(ApiError::conflict(
                    "only Codex sessions currently support MCP elicitation input",
                ));
            }

            let pending = record
                .pending_codex_mcp_elicitations
                .get(message_id)
                .cloned()
                .ok_or_else(|| ApiError::conflict("MCP elicitation request is no longer live"))?;
            let handle = match &record.runtime {
                SessionRuntime::Codex(handle) => handle.clone(),
                SessionRuntime::Claude(_) | SessionRuntime::None | SessionRuntime::Acp(_) => {
                    return Err(ApiError::conflict("Codex session is not currently running"));
                }
            };
            let normalized_content =
                validate_codex_mcp_elicitation_submission(&pending.request, action, content)?;
            (handle, pending, normalized_content)
        };

        handle
            .input_tx
            .send(CodexRuntimeCommand::JsonRpcResponse {
                response: CodexJsonRpcResponseCommand {
                    request_id: pending.request_id.clone(),
                    payload: CodexJsonRpcResponsePayload::Result(json!({
                        "action": action,
                        "content": normalized_content
                    })),
                },
            })
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to deliver MCP elicitation response to Codex: {err}"
                ))
            })?;

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        set_mcp_elicitation_request_state_on_record(
            record,
            message_id,
            InteractionRequestState::Submitted,
            Some(action),
            normalized_content.clone(),
        )
        .map_err(|_| ApiError::not_found("MCP elicitation request not found"))?;
        record.pending_codex_mcp_elicitations.remove(message_id);
        sync_session_interaction_state(
            record,
            mcp_elicitation_request_preview_text(
                record.session.agent.name(),
                InteractionRequestState::Submitted,
                Some(action),
            ),
        );
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }

    /// Submits Codex app request.
    fn submit_codex_app_request(
        &self,
        session_id: &str,
        message_id: &str,
        result: Value,
    ) -> std::result::Result<StateResponse, ApiError> {
        if self.remote_session_target(session_id)?.is_some() {
            return self.proxy_remote_submit_codex_app_request(session_id, message_id, result);
        }
        let result = validate_codex_app_request_result(result)?;

        let (handle, pending) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_visible_session_index(session_id)
                .ok_or_else(|| ApiError::not_found("session not found"))?;
            let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
            if record.session.status != SessionStatus::Approval {
                return Err(ApiError::conflict(
                    "session is not currently waiting for a Codex request response",
                ));
            }
            if record.session.agent != Agent::Codex {
                return Err(ApiError::conflict(
                    "only Codex sessions currently support generic app-server requests",
                ));
            }

            let pending = record
                .pending_codex_app_requests
                .get(message_id)
                .cloned()
                .ok_or_else(|| ApiError::conflict("Codex app request is no longer live"))?;
            let handle = match &record.runtime {
                SessionRuntime::Codex(handle) => handle.clone(),
                SessionRuntime::Claude(_) | SessionRuntime::None | SessionRuntime::Acp(_) => {
                    return Err(ApiError::conflict("Codex session is not currently running"));
                }
            };
            (handle, pending)
        };

        handle
            .input_tx
            .send(CodexRuntimeCommand::JsonRpcResponse {
                response: CodexJsonRpcResponseCommand {
                    request_id: pending.request_id.clone(),
                    payload: CodexJsonRpcResponsePayload::Result(result.clone()),
                },
            })
            .map_err(|err| {
                ApiError::internal(format!(
                    "failed to deliver generic Codex app request response: {err}"
                ))
            })?;

        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| ApiError::not_found("session not found"))?;
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        set_codex_app_request_state_on_record(
            record,
            message_id,
            InteractionRequestState::Submitted,
            Some(result),
        )
        .map_err(|_| ApiError::not_found("Codex app request not found"))?;
        record.pending_codex_app_requests.remove(message_id);
        sync_session_interaction_state(
            record,
            codex_app_request_preview_text(
                record.session.agent.name(),
                InteractionRequestState::Submitted,
            ),
        );
        self.commit_locked(&mut inner).map_err(|err| {
            ApiError::internal(format!("failed to persist session state: {err:#}"))
        })?;
        Ok(self.snapshot_from_inner(&inner))
    }

    /// Marks turn as failed.
    fn fail_turn(&self, session_id: &str, error_message: &str) -> Result<()> {
        let cleaned = error_message.trim();
        if !cleaned.is_empty() {
            self.push_message(
                session_id,
                Message::Text {
                    attachments: Vec::new(),
                    id: self.allocate_message_id(),
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: format!("Turn failed: {cleaned}"),
                    expanded_text: None,
                },
            )?;
        }

        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let index = inner
                .find_session_index(session_id)
                .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
            let file_change_message_id = (!inner.sessions[index].active_turn_file_changes.is_empty())
                .then(|| inner.next_message_id());
            let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
            record.session.status = SessionStatus::Error;
            record.session.preview = make_preview(cleaned);
            if let Some(message_id) = file_change_message_id {
                push_active_turn_file_changes_on_record(record, message_id);
            }
            finish_active_turn_file_change_tracking(record);
            self.commit_locked(&mut inner)?;
        }

        if let Some(dispatch) = self.dispatch_next_queued_turn(session_id, false)? {
            deliver_turn_dispatch(self, dispatch).map_err(|err| {
                anyhow!("failed to deliver queued turn dispatch: {}", err.message)
            })?;
        }
        Ok(())
    }
}

/// Handles Codex approval result.
fn codex_approval_result(kind: &CodexApprovalKind, decision: ApprovalDecision) -> Value {
    match kind {
        CodexApprovalKind::CommandExecution => match decision {
            ApprovalDecision::Accepted => json!({ "decision": "accept" }),
            ApprovalDecision::AcceptedForSession => json!({ "decision": "acceptForSession" }),
            ApprovalDecision::Rejected => json!({ "decision": "decline" }),
            ApprovalDecision::Pending
            | ApprovalDecision::Interrupted
            | ApprovalDecision::Canceled => {
                unreachable!("non-deliverable approval decisions are not sent to Codex")
            }
        },
        CodexApprovalKind::FileChange => match decision {
            ApprovalDecision::Accepted => json!({ "decision": "accept" }),
            ApprovalDecision::AcceptedForSession => json!({ "decision": "acceptForSession" }),
            ApprovalDecision::Rejected => json!({ "decision": "decline" }),
            ApprovalDecision::Pending
            | ApprovalDecision::Interrupted
            | ApprovalDecision::Canceled => {
                unreachable!("non-deliverable approval decisions are not sent to Codex")
            }
        },
        CodexApprovalKind::Permissions {
            requested_permissions,
        } => {
            let permissions = match decision {
                ApprovalDecision::Accepted | ApprovalDecision::AcceptedForSession => {
                    requested_permissions.clone()
                }
                ApprovalDecision::Rejected => json!({}),
                ApprovalDecision::Pending
                | ApprovalDecision::Interrupted
                | ApprovalDecision::Canceled => {
                    unreachable!("non-deliverable approval decisions are not sent to Codex")
                }
            };
            let scope = match decision {
                ApprovalDecision::AcceptedForSession => "session",
                ApprovalDecision::Accepted | ApprovalDecision::Rejected => "turn",
                ApprovalDecision::Pending
                | ApprovalDecision::Interrupted
                | ApprovalDecision::Canceled => {
                    unreachable!("non-deliverable approval decisions are not sent to Codex")
                }
            };
            json!({
                "permissions": permissions,
                "scope": scope,
            })
        }
    }
}


/// Normalizes remote configs.
fn normalize_remote_configs(remotes: Vec<RemoteConfig>) -> Result<Vec<RemoteConfig>, ApiError> {
    let mut normalized = vec![RemoteConfig::local()];
    let mut seen_ids = HashSet::from([default_local_remote_id()]);

    for remote in remotes {
        let id = remote.id.trim();
        validate_remote_id_value(id)?;
        if id.eq_ignore_ascii_case(LOCAL_REMOTE_ID) {
            continue;
        }
        if !seen_ids.insert(id.to_owned()) {
            return Err(ApiError::bad_request(format!("duplicate remote id `{id}`")));
        }

        let name = remote.name.trim();
        if name.is_empty() {
            return Err(ApiError::bad_request(format!(
                "remote `{id}` must have a name"
            )));
        }

        match remote.transport {
            RemoteTransport::Local => {
                return Err(ApiError::bad_request(format!(
                    "remote `{id}` cannot use local transport"
                )));
            }
            RemoteTransport::Ssh => {
                let host = normalized_remote_ssh_host(&remote)?;
                normalized.push(RemoteConfig {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    transport: RemoteTransport::Ssh,
                    enabled: remote.enabled,
                    host: Some(host),
                    port: Some(remote.port.unwrap_or(DEFAULT_SSH_REMOTE_PORT)),
                    user: normalized_remote_ssh_user(&remote)?,
                });
            }
        }
    }

    Ok(normalized)
}

/// Validates persisted remote configs.
fn validate_persisted_remote_configs(remotes: Vec<RemoteConfig>) -> Result<Vec<RemoteConfig>> {
    normalize_remote_configs(remotes).map_err(|err| anyhow!(err.message))
}

/// Normalizes local user facing path.
fn normalize_local_user_facing_path(path: &str) -> String {
    normalize_user_facing_path(FsPath::new(path))
        .to_string_lossy()
        .into_owned()
}

/// Normalizes workspace layout paths.
fn normalize_workspace_layout_paths(layout: &mut WorkspaceLayoutDocument) {
    let Some(panes) = layout
        .workspace
        .get_mut("panes")
        .and_then(Value::as_array_mut)
    else {
        return;
    };

    for pane in panes {
        normalize_workspace_layout_path_field(pane, "sourcePath");

        let Some(tabs) = pane.get_mut("tabs").and_then(Value::as_array_mut) else {
            continue;
        };

        for tab in tabs {
            match tab.get("kind").and_then(Value::as_str) {
                Some("source") => normalize_workspace_layout_path_field(tab, "path"),
                Some("filesystem") => normalize_workspace_layout_path_field(tab, "rootPath"),
                Some("gitStatus") | Some("instructionDebugger") => {
                    normalize_workspace_layout_path_field(tab, "workdir")
                }
                Some("diffPreview") => normalize_workspace_layout_path_field(tab, "filePath"),
                _ => {}
            }
        }
    }
}

/// Normalizes workspace layout path field.
fn normalize_workspace_layout_path_field(object: &mut Value, key: &str) {
    let Some(field) = object.get_mut(key) else {
        return;
    };
    let Some(path) = field.as_str() else {
        return;
    };
    *field = Value::String(normalize_local_user_facing_path(path));
}

/// Builds sorted workspace layout summaries.
fn collect_workspace_layout_summaries<'a>(
    layouts: impl Iterator<Item = &'a WorkspaceLayoutDocument>,
) -> Vec<WorkspaceLayoutSummary> {
    let mut workspaces = layouts
        .map(|layout| WorkspaceLayoutSummary {
            id: layout.id.clone(),
            revision: layout.revision,
            updated_at: layout.updated_at.clone(),
            control_panel_side: layout.control_panel_side,
            theme_id: layout.theme_id.clone(),
            style_id: layout.style_id.clone(),
            font_size_px: layout.font_size_px,
            editor_font_size_px: layout.editor_font_size_px,
            density_percent: layout.density_percent,
        })
        .collect::<Vec<_>>();
    workspaces.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    workspaces
}

/// Represents state inner.
struct StateInner {
    codex: CodexState,
    preferences: AppPreferences,
    revision: u64,
    next_project_number: usize,
    next_session_number: usize,
    next_message_number: u64,
    /// Stable project records used by local and remote session routing.
    projects: Vec<Project>,
    ignored_discovered_codex_thread_ids: BTreeSet<String>,
    /// Tracks the latest remote revision applied for each mirrored remote so
    /// stale snapshots and deltas cannot roll local proxy state backward.
    remote_applied_revisions: HashMap<String, u64>,
    /// Session records carry the serializable session plus runtime-only state.
    sessions: Vec<SessionRecord>,
    /// Runtime instances for orchestrator templates live beside ordinary sessions.
    orchestrator_instances: Vec<OrchestratorInstance>,
    /// Server-backed workspace documents keyed by workspace id.
    workspace_layouts: BTreeMap<String, WorkspaceLayoutDocument>,
    /// Monotonic counter used to stamp mutated session records. The persist
    /// thread uses this plus its own watermark to write only the sessions
    /// that changed since the last successful persist, so each
    /// `commit_locked` pays only for the sessions it actually touched
    /// rather than rewriting every session row. Starts at 0; advanced by
    /// [`StateInner::next_mutation_stamp`] on every `session_mut*` call.
    last_mutation_stamp: u64,
    /// Session ids removed from `sessions` since the last persist tick.
    /// Drained by the persist thread and applied as targeted `DELETE`
    /// statements so removed rows do not linger after the move to
    /// delta persistence.
    removed_session_ids: Vec<String>,
}

impl StateInner {
    /// Creates a new instance.
    fn new() -> Self {
        Self {
            codex: CodexState::default(),
            preferences: AppPreferences::default(),
            revision: 0,
            next_project_number: 1,
            next_session_number: 1,
            next_message_number: 1,
            projects: Vec::new(),
            ignored_discovered_codex_thread_ids: BTreeSet::new(),
            remote_applied_revisions: HashMap::new(),
            sessions: Vec::new(),
            orchestrator_instances: Vec::new(),
            workspace_layouts: BTreeMap::new(),
            last_mutation_stamp: 0,
            removed_session_ids: Vec::new(),
        }
    }

    /// Returns whether the supplied remote snapshot revision is stale for this remote.
    fn should_skip_remote_applied_revision(&self, remote_id: &str, remote_revision: u64) -> bool {
        self.remote_applied_revisions
            .get(remote_id)
            .is_some_and(|latest_revision| *latest_revision >= remote_revision)
    }

    /// Returns whether the supplied remote delta revision is stale for this remote.
    fn should_skip_remote_applied_delta_revision(
        &self,
        remote_id: &str,
        remote_revision: u64,
    ) -> bool {
        self.remote_applied_revisions
            .get(remote_id)
            .is_some_and(|latest_revision| *latest_revision > remote_revision)
    }

    /// Records the latest applied remote revision for a mirrored remote.
    fn note_remote_applied_revision(&mut self, remote_id: &str, remote_revision: u64) {
        self.remote_applied_revisions
            .entry(remote_id.to_owned())
            .and_modify(|latest_revision| {
                *latest_revision = (*latest_revision).max(remote_revision);
            })
            .or_insert(remote_revision);
    }

    /// Creates project.
    fn create_project(
        &mut self,
        name: Option<String>,
        root_path: String,
        remote_id: String,
    ) -> Project {
        if let Some(existing) = self
            .projects
            .iter()
            .find(|project| project.remote_id == remote_id && project.root_path == root_path)
            .cloned()
        {
            return existing;
        }

        let number = self.next_project_number;
        self.next_project_number += 1;
        let base_name = name.unwrap_or_else(|| default_project_name(&root_path));
        let project = Project {
            id: format!("project-{number}"),
            name: dedupe_project_name(&self.projects, &base_name),
            root_path,
            remote_id,
            remote_project_id: None,
        };
        self.projects.push(project.clone());
        project
    }

    /// Creates session.
    fn create_session(
        &mut self,
        agent: Agent,
        name: Option<String>,
        workdir: String,
        project_id: Option<String>,
        model: Option<String>,
    ) -> SessionRecord {
        let number = self.next_session_number;
        self.next_session_number += 1;

        let record = SessionRecord {
            active_codex_approval_policy: None,
            active_codex_reasoning_effort: None,
            active_codex_sandbox_mode: None,
            active_turn_start_message_count: None,
            active_turn_file_changes: BTreeMap::new(),
            active_turn_file_change_grace_deadline: None,
            agent_commands: Vec::new(),
            codex_approval_policy: default_codex_approval_policy(),
            codex_reasoning_effort: self.preferences.default_codex_reasoning_effort,
            codex_sandbox_mode: default_codex_sandbox_mode(),
            external_session_id: None,
            pending_claude_approvals: HashMap::new(),
            pending_codex_approvals: HashMap::new(),
            pending_codex_user_inputs: HashMap::new(),
            pending_codex_mcp_elicitations: HashMap::new(),
            pending_codex_app_requests: HashMap::new(),
            pending_acp_approvals: HashMap::new(),
            queued_prompts: VecDeque::new(),
            message_positions: HashMap::new(),
            remote_id: None,
            remote_session_id: None,
            runtime: SessionRuntime::None,
            runtime_reset_required: false,
            orchestrator_auto_dispatch_blocked: false,
            runtime_stop_in_progress: false,
            deferred_stop_callbacks: Vec::new(),
            hidden: false,
            // Freshly created records start unstamped; the call path
            // immediately inserts this record and then the caller routes
            // subsequent edits through `session_mut*`, which bumps the
            // stamp as soon as a mutation happens.
            mutation_stamp: 0,
            session: Session {
                id: format!("session-{number}"),
                name: name.unwrap_or_else(|| format!("{} {}", agent.name(), number)),
                emoji: agent.avatar().to_owned(),
                agent,
                workdir,
                project_id,
                model: model.unwrap_or_else(|| agent.default_model().to_owned()),
                model_options: Vec::new(),
                approval_policy: None,
                reasoning_effort: None,
                sandbox_mode: None,
                cursor_mode: agent
                    .supports_cursor_mode()
                    .then_some(default_cursor_mode()),
                claude_approval_mode: agent
                    .supports_claude_approval_mode()
                    .then_some(self.preferences.default_claude_approval_mode),
                claude_effort: agent
                    .supports_claude_approval_mode()
                    .then_some(self.preferences.default_claude_effort),
                gemini_approval_mode: agent
                    .supports_gemini_approval_mode()
                    .then_some(default_gemini_approval_mode()),
                external_session_id: None,
                agent_commands_revision: 0,
                codex_thread_state: None,
                status: SessionStatus::Idle,
                preview: "Ready for a prompt.".to_owned(),
                messages: Vec::new(),
                pending_prompts: Vec::new(),
            },
        };

        let mut record = record;
        if record.session.agent.supports_codex_prompt_settings() {
            record.session.approval_policy = Some(record.codex_approval_policy);
            record.session.reasoning_effort = Some(record.codex_reasoning_effort);
            record.session.sandbox_mode = Some(record.codex_sandbox_mode);
        } else if record.session.agent.supports_claude_approval_mode() {
            record.session.claude_approval_mode = Some(self.preferences.default_claude_approval_mode);
            record.session.claude_effort = Some(self.preferences.default_claude_effort);
        }

        self.push_session(record.clone());
        record
    }

    /// Ignores discovered Codex thread.
    fn ignore_discovered_codex_thread(&mut self, thread_id: Option<&str>) {
        if let Some(thread_id) = normalize_optional_identifier(thread_id) {
            self.ignored_discovered_codex_thread_ids
                .insert(thread_id.to_owned());
        }
    }

    /// Allows discovered Codex thread.
    fn allow_discovered_codex_thread(&mut self, thread_id: Option<&str>) {
        if let Some(thread_id) = normalize_optional_identifier(thread_id) {
            self.ignored_discovered_codex_thread_ids.remove(thread_id);
        }
    }

    /// Finds matching hidden Claude spare.
    fn find_matching_hidden_claude_spare(
        &self,
        workdir: &str,
        project_id: Option<&str>,
        model: &str,
        approval_mode: ClaudeApprovalMode,
        effort: ClaudeEffortLevel,
    ) -> Option<usize> {
        self.sessions.iter().position(|record| {
            record.hidden
                && !record.is_remote_proxy()
                && record.session.agent == Agent::Claude
                && record.session.workdir == workdir
                && record.session.project_id.as_deref() == project_id
                && record.session.model == model
                && record.session.claude_approval_mode == Some(approval_mode)
                && record.session.claude_effort == Some(effort)
        })
    }

    /// Ensures hidden Claude spare.
    fn ensure_hidden_claude_spare(
        &mut self,
        workdir: String,
        project_id: Option<String>,
        model: String,
        approval_mode: ClaudeApprovalMode,
        effort: ClaudeEffortLevel,
    ) -> Option<String> {
        if let Some(index) = self.find_matching_hidden_claude_spare(
            &workdir,
            project_id.as_deref(),
            &model,
            approval_mode,
            effort,
        ) {
            let record = self
                .session_mut_by_index(index)
                .expect("session index should be valid");
            reset_hidden_claude_spare_record(record);
            return matches!(record.runtime, SessionRuntime::None)
                .then(|| record.session.id.clone());
        }

        self.create_session(Agent::Claude, None, workdir, project_id, Some(model));
        let record = self
            .sessions
            .last_mut()
            .expect("create_session should append a session record");
        record.hidden = true;
        record.session.claude_approval_mode = Some(approval_mode);
        record.session.claude_effort = Some(effort);
        reset_hidden_claude_spare_record(record);
        Some(record.session.id.clone())
    }

    /// Returns the next message ID.
    fn next_message_id(&mut self) -> String {
        let id = format!("message-{}", self.next_message_number);
        self.next_message_number += 1;
        id
    }

    /// Returns a fresh monotonic mutation stamp.
    ///
    /// Every `session_mut*` helper calls this before handing out mutable
    /// access to a `SessionRecord`. The persist thread compares each
    /// record's `mutation_stamp` against its watermark to identify the
    /// exact subset of sessions that changed since the last successful
    /// persist, so a `commit_locked` on one session no longer re-
    /// serializes every other session row.
    fn next_mutation_stamp(&mut self) -> u64 {
        self.last_mutation_stamp = self.last_mutation_stamp.saturating_add(1);
        self.last_mutation_stamp
    }

    /// Stamps the session at `index` with the next mutation stamp.
    ///
    /// Use when the caller already has the index (e.g., from a loop or a
    /// prior `find_session_index`) and intends to mutate that session
    /// without rebinding through `session_mut_by_index`. Returns the
    /// assigned stamp, or `None` if the index is out of bounds.
    fn stamp_session_at_index(&mut self, index: usize) -> Option<u64> {
        let stamp = self.next_mutation_stamp();
        let record = self.sessions.get_mut(index)?;
        record.mutation_stamp = stamp;
        Some(stamp)
    }

    /// Finds a session by id and returns mutable access, stamping the
    /// record so the persist thread picks up the mutation on its next
    /// tick. Returns `None` if no session matches.
    fn session_mut(&mut self, session_id: &str) -> Option<&mut SessionRecord> {
        let index = self.find_session_index(session_id)?;
        let stamp = self.next_mutation_stamp();
        let record = self.sessions.get_mut(index)?;
        record.mutation_stamp = stamp;
        Some(record)
    }

    /// Like [`StateInner::session_mut`] but indexed directly. Panics if
    /// the index is out of bounds; callers should obtain the index via
    /// `find_session_index` / `find_visible_session_index` first.
    fn session_mut_by_index(&mut self, index: usize) -> Option<&mut SessionRecord> {
        let stamp = self.next_mutation_stamp();
        let record = self.sessions.get_mut(index)?;
        record.mutation_stamp = stamp;
        Some(record)
    }

    /// Records that a session id has been removed from `sessions` since
    /// the last persist tick. Drained by the persist thread and applied
    /// as targeted `DELETE` statements so removed rows do not linger in
    /// SQLite after the move to delta persistence.
    fn record_removed_session(&mut self, session_id: String) {
        if !session_id.is_empty() {
            self.removed_session_ids.push(session_id);
        }
    }

    /// Inserts a new session record, stamping it so the persist thread
    /// picks it up on its next tick. Returns the index at which the
    /// record was inserted (end of the `sessions` vec).
    fn push_session(&mut self, mut record: SessionRecord) -> usize {
        let stamp = self.next_mutation_stamp();
        record.mutation_stamp = stamp;
        self.sessions.push(record);
        self.sessions.len() - 1
    }

    /// Removes the session at `index`, recording its id in
    /// `removed_session_ids` so the persist thread issues a `DELETE`
    /// on its next tick. Panics on out-of-bounds access like the
    /// underlying `Vec::remove` it wraps.
    fn remove_session_at(&mut self, index: usize) -> SessionRecord {
        let record = self.sessions.remove(index);
        let id = record.session.id.clone();
        self.record_removed_session(id);
        record
    }

    /// `Vec::retain`-style filter that records every dropped session id
    /// as a tombstone. The predicate is called once per record.
    fn retain_sessions<F>(&mut self, mut keep: F)
    where
        F: FnMut(&SessionRecord) -> bool,
    {
        let mut removed_ids: Vec<String> = Vec::new();
        self.sessions.retain(|record| {
            let retained = keep(record);
            if !retained {
                removed_ids.push(record.session.id.clone());
            }
            retained
        });
        for id in removed_ids {
            self.record_removed_session(id);
        }
    }

    /// Collects the subset of state that advanced past `watermark`.
    ///
    /// Called by the background persist thread while it briefly holds
    /// `AppState::inner`. Clones only:
    ///
    /// - App metadata (non-session fields; shallow clones, no transcripts).
    /// - Sessions whose `mutation_stamp > watermark`, filtered so
    ///   hidden sessions produce `DELETE`s instead of upserts (visible
    ///   sessions that have flipped to hidden since the last persist
    ///   need to disappear from SQLite, and hidden spares are
    ///   regenerated on startup rather than persisted).
    /// - The tombstone list of explicitly removed session ids, drained
    ///   from `removed_session_ids`.
    ///
    /// Returns the new watermark (`last_mutation_stamp` at collection
    /// time) that the caller should install after a successful write.
    ///
    /// The only call site is the background persist thread, which is
    /// `#[cfg(not(test))]`-gated — so under `cargo test` this looks
    /// unused. The `allow(dead_code)` silences that warning without
    /// hiding real dead code in release builds.
    #[cfg_attr(test, allow(dead_code))]
    fn collect_persist_delta(&mut self, watermark: u64) -> PersistDelta {
        let mut changed_sessions: Vec<PersistedSessionRecord> = Vec::new();
        let mut removed_ids = std::mem::take(&mut self.removed_session_ids);
        for record in &self.sessions {
            if record.mutation_stamp <= watermark {
                continue;
            }
            if record.hidden {
                // A session that changed and is now hidden must not
                // stay in SQLite — hidden spares are re-seeded on
                // startup, so ensure the row is removed.
                removed_ids.push(record.session.id.clone());
            } else {
                changed_sessions.push(PersistedSessionRecord::from_record(record));
            }
        }
        PersistDelta {
            metadata: PersistedState::metadata_from_inner(self),
            changed_sessions,
            removed_session_ids: removed_ids,
            watermark: self.last_mutation_stamp,
        }
    }

    /// Finds session index.
    fn find_session_index(&self, session_id: &str) -> Option<usize> {
        self.sessions
            .iter()
            .position(|record| record.session.id == session_id)
    }

    /// Finds visible session index.
    fn find_visible_session_index(&self, session_id: &str) -> Option<usize> {
        self.sessions
            .iter()
            .position(|record| !record.hidden && record.session.id == session_id)
    }

    /// Finds remote session index.
    fn find_remote_session_index(&self, remote_id: &str, remote_session_id: &str) -> Option<usize> {
        self.sessions.iter().position(|record| {
            record.remote_id.as_deref() == Some(remote_id)
                && record.remote_session_id.as_deref() == Some(remote_session_id)
        })
    }

    /// Finds remote orchestrator index.
    fn find_remote_orchestrator_index(
        &self,
        remote_id: &str,
        remote_orchestrator_id: &str,
    ) -> Option<usize> {
        self.orchestrator_instances.iter().position(|instance| {
            instance.remote_id.as_deref() == Some(remote_id)
                && instance.remote_orchestrator_id.as_deref() == Some(remote_orchestrator_id)
        })
    }

    /// Finds project.
    fn find_project(&self, project_id: &str) -> Option<&Project> {
        self.projects
            .iter()
            .find(|project| project.id == project_id)
    }

    /// Finds remote.
    fn find_remote(&self, remote_id: &str) -> Option<&RemoteConfig> {
        self.preferences
            .remotes
            .iter()
            .find(|remote| remote.id == remote_id)
    }

    /// Finds project for working directory.
    fn find_project_for_workdir(&self, workdir: &str) -> Option<&Project> {
        let target = FsPath::new(workdir);
        self.projects
            .iter()
            .filter(|project| {
                project.remote_id == LOCAL_REMOTE_ID
                    && codex_discovery_scope_contains(&project.root_path, target)
            })
            .max_by_key(|project| project.root_path.len())
    }

    /// Imports discovered Codex threads.
    fn import_discovered_codex_threads(
        &mut self,
        default_workdir: &str,
        threads: Vec<DiscoveredCodexThread>,
    ) {
        let discovered_thread_ids = threads
            .iter()
            .filter_map(|thread| normalize_optional_identifier(Some(thread.id.as_str())))
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        self.ignored_discovered_codex_thread_ids
            .retain(|thread_id| discovered_thread_ids.contains(thread_id));

        for thread in threads {
            let thread = DiscoveredCodexThread {
                cwd: normalize_local_user_facing_path(&thread.cwd),
                ..thread
            };
            let target_path = FsPath::new(&thread.cwd);
            let within_scope = codex_discovery_scope_contains(default_workdir, target_path)
                || self.projects.iter().any(|project| {
                    project.remote_id == LOCAL_REMOTE_ID
                        && codex_discovery_scope_contains(&project.root_path, target_path)
                });
            if !within_scope {
                continue;
            }

            let project_id = self
                .find_project_for_workdir(&thread.cwd)
                .map(|project| project.id.clone())
                .unwrap_or_else(|| {
                    self.create_project(None, thread.cwd.clone(), default_local_remote_id())
                        .id
                });

            let existing_index = self.sessions.iter().position(|record| {
                !record.is_remote_proxy()
                    && record.session.agent == Agent::Codex
                    && record.external_session_id.as_deref() == Some(thread.id.as_str())
            });

            if let Some(index) = existing_index {
                self.allow_discovered_codex_thread(Some(thread.id.as_str()));
                let record = self
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                if record.session.workdir != thread.cwd {
                    record.session.workdir = thread.cwd.clone();
                }
                if record.session.project_id.as_deref() != Some(project_id.as_str()) {
                    record.session.project_id = Some(project_id);
                }
                apply_discovered_codex_thread(record, &thread, false);
                continue;
            }

            if self
                .ignored_discovered_codex_thread_ids
                .contains(thread.id.as_str())
            {
                continue;
            }

            let mut record = self.create_session(
                Agent::Codex,
                Some(thread.title.clone()),
                thread.cwd.clone(),
                Some(project_id),
                thread.model.clone(),
            );
            apply_discovered_codex_thread(&mut record, &thread, true);
            if let Some(slot) = self
                .find_session_index(&record.session.id)
                .and_then(|index| self.sessions.get_mut(index))
            {
                *slot = record;
            }
        }
    }

    /// Validates projects consistent.
    fn validate_projects_consistent(&self) -> Result<()> {
        for project in &self.projects {
            let remote_id = project.remote_id.trim();
            if remote_id.is_empty() {
                return Err(anyhow!(
                    "persisted project `{}` is missing remoteId",
                    project.id
                ));
            }
            if self.find_remote(remote_id).is_none() {
                return Err(anyhow!(
                    "persisted project `{}` references unknown remote `{remote_id}`",
                    project.id
                ));
            }
        }

        if self.next_project_number < 1 {
            return Err(anyhow!("persisted nextProjectNumber must be at least 1"));
        }

        let highest_project_number = self
            .projects
            .iter()
            .filter_map(|project| {
                project
                    .id
                    .strip_prefix("project-")
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .max()
            .unwrap_or(0);
        if self.next_project_number <= highest_project_number {
            return Err(anyhow!(
                "persisted nextProjectNumber `{}` must be greater than existing project ids",
                self.next_project_number
            ));
        }

        for record in &self.sessions {
            let Some(project_id) = record.session.project_id.as_deref() else {
                continue;
            };
            if self.find_project(project_id).is_none() {
                return Err(anyhow!(
                    "persisted session `{}` references unknown project `{project_id}`",
                    record.session.id
                ));
            }
        }

        Ok(())
    }

    /// Normalizes local paths.
    fn normalize_local_paths(&mut self) {
        let local_project_ids = self
            .projects
            .iter_mut()
            .filter_map(|project| {
                if project.remote_id == LOCAL_REMOTE_ID {
                    project.root_path = normalize_local_user_facing_path(&project.root_path);
                    Some(project.id.clone())
                } else {
                    None
                }
            })
            .collect::<HashSet<_>>();

        for record in &mut self.sessions {
            let should_normalize = record.remote_id.is_none()
                && record
                    .session
                    .project_id
                    .as_deref()
                    .map(|project_id| local_project_ids.contains(project_id))
                    .unwrap_or(true);
            if should_normalize {
                record.session.workdir = normalize_local_user_facing_path(&record.session.workdir);
            }
        }
        for layout in self.workspace_layouts.values_mut() {
            normalize_workspace_layout_paths(layout);
        }
    }

    /// Recovers interrupted sessions.
    fn recover_interrupted_sessions(&mut self) {
        for index in 0..self.sessions.len() {
            if self.sessions[index].is_remote_proxy() {
                continue;
            }
            let recovery = {
                let record = self
                    .session_mut_by_index(index)
                    .expect("session index should be valid");
                recover_interrupted_session_record(record)
            };

            let Some(recovery) = recovery else {
                continue;
            };

            let message_id = self.next_message_id();
            let record = self
                .session_mut_by_index(index)
                .expect("session index should be valid");
            push_message_on_record(
                record,
                Message::Text {
                    attachments: Vec::new(),
                    id: message_id,
                    timestamp: stamp_now(),
                    author: Author::Assistant,
                    text: recovery,
                    expanded_text: None,
                },
            );
            record.session.status = SessionStatus::Error;
            if let Some(message) = record.session.messages.last() {
                if let Some(preview) = message.preview_text() {
                    record.session.preview = preview;
                }
            }
        }
    }
}


/// Represents a session record.
#[derive(Clone)]
struct SessionRecord {
    active_codex_approval_policy: Option<CodexApprovalPolicy>,
    active_codex_reasoning_effort: Option<CodexReasoningEffort>,
    active_codex_sandbox_mode: Option<CodexSandboxMode>,
    active_turn_start_message_count: Option<usize>,
    active_turn_file_changes: BTreeMap<String, WorkspaceFileChangeKind>,
    active_turn_file_change_grace_deadline: Option<std::time::Instant>,
    agent_commands: Vec<AgentCommand>,
    codex_approval_policy: CodexApprovalPolicy,
    codex_reasoning_effort: CodexReasoningEffort,
    codex_sandbox_mode: CodexSandboxMode,
    external_session_id: Option<String>,
    pending_claude_approvals: HashMap<String, ClaudePendingApproval>,
    pending_codex_approvals: HashMap<String, CodexPendingApproval>,
    pending_codex_user_inputs: HashMap<String, CodexPendingUserInput>,
    pending_codex_mcp_elicitations: HashMap<String, CodexPendingMcpElicitation>,
    pending_codex_app_requests: HashMap<String, CodexPendingAppRequest>,
    pending_acp_approvals: HashMap<String, AcpPendingApproval>,
    /// FIFO follow-up prompts collected while the runtime is busy.
    queued_prompts: VecDeque<QueuedPromptRecord>,
    message_positions: HashMap<String, usize>,
    /// Present only for proxy sessions mirrored from a remote TermAl backend.
    remote_id: Option<String>,
    remote_session_id: Option<String>,
    runtime: SessionRuntime,
    runtime_reset_required: bool,
    /// Persisted guard for sessions whose runtime was stopped but whose updated session state
    /// failed to persist cleanly; blocks auto-dispatch until an explicit user/manual turn resets it.
    orchestrator_auto_dispatch_blocked: bool,
    runtime_stop_in_progress: bool,
    /// Terminal callbacks deferred while `runtime_stop_in_progress` was true. Replayed in arrival
    /// order on dedicated stop failure so the session doesn't get stuck in a stale Active state or
    /// reconstruct the wrong terminal sequence when completion/error and runtime-exit both land
    /// during the shutdown window.
    deferred_stop_callbacks: Vec<DeferredStopCallback>,
    hidden: bool,
    session: Session,
    /// Monotonic mutation stamp assigned by [`StateInner::next_mutation_stamp`]
    /// every time this record is handed out through one of the
    /// `session_mut*` helpers. Not persisted — stamps start at `0` on each
    /// process lifetime and the persist thread's watermark advances
    /// accordingly. A stamp strictly greater than the persist watermark
    /// means this record has in-memory changes that have not yet reached
    /// SQLite.
    mutation_stamp: u64,
}

impl SessionRecord {
    /// Returns whether remote proxy.
    fn is_remote_proxy(&self) -> bool {
        self.remote_id.is_some() && self.remote_session_id.is_some()
    }
}



/// Handles Codex approval policy from JSON value.
fn codex_approval_policy_from_json_value(value: &Value) -> Option<CodexApprovalPolicy> {
    match value {
        Value::String(raw) => match raw.as_str() {
            "untrusted" => Some(CodexApprovalPolicy::Untrusted),
            "on-failure" => Some(CodexApprovalPolicy::OnFailure),
            "on-request" => Some(CodexApprovalPolicy::OnRequest),
            "never" => Some(CodexApprovalPolicy::Never),
            _ => None,
        },
        _ => None,
    }
}

/// Handles Codex reasoning effort from JSON value.
fn codex_reasoning_effort_from_json_value(value: &Value) -> Option<CodexReasoningEffort> {
    match value {
        Value::String(raw) => match raw.as_str() {
            "none" => Some(CodexReasoningEffort::None),
            "minimal" => Some(CodexReasoningEffort::Minimal),
            "low" => Some(CodexReasoningEffort::Low),
            "medium" => Some(CodexReasoningEffort::Medium),
            "high" => Some(CodexReasoningEffort::High),
            "xhigh" => Some(CodexReasoningEffort::XHigh),
            _ => None,
        },
        _ => None,
    }
}

/// Handles Codex sandbox mode from JSON value.
fn codex_sandbox_mode_from_json_value(value: &Value) -> Option<CodexSandboxMode> {
    match value {
        Value::String(raw) => match raw.as_str() {
            "danger-full-access" => Some(CodexSandboxMode::DangerFullAccess),
            "read-only" => Some(CodexSandboxMode::ReadOnly),
            "workspace-write" => Some(CodexSandboxMode::WorkspaceWrite),
            _ => None,
        },
        Value::Object(_) => match value.get("type").and_then(Value::as_str) {
            Some("dangerFullAccess") => Some(CodexSandboxMode::DangerFullAccess),
            Some("readOnly") => Some(CodexSandboxMode::ReadOnly),
            Some("workspaceWrite") => Some(CodexSandboxMode::WorkspaceWrite),
            _ => None,
        },
        _ => None,
    }
}

/// Returns the default forked Codex session name.
fn default_forked_codex_session_name(current_name: &str, thread_name: Option<&str>) -> String {
    let trimmed_thread_name = thread_name.map(str::trim).filter(|value| !value.is_empty());
    let trimmed_current_name = current_name.trim();
    let base = trimmed_thread_name.unwrap_or(trimmed_current_name);
    format!("{base} Fork")
}

/// Resolves forked Codex working directory.
fn resolve_forked_codex_workdir(
    requested_workdir: Option<&str>,
    fallback_workdir: &str,
    project_id: Option<&str>,
    state: &AppState,
) -> Result<String, ApiError> {
    let Some(requested_workdir) = requested_workdir
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(fallback_workdir.to_owned());
    };

    let project_id = match project_id {
        Some(project_id) => project_id,
        None => return Ok(requested_workdir.to_owned()),
    };
    let project_root = resolve_project_root_path_by_id(state, project_id)?;
    if path_contains(
        project_root.to_string_lossy().as_ref(),
        FsPath::new(requested_workdir),
    ) {
        Ok(requested_workdir.to_owned())
    } else {
        Ok(fallback_workdir.to_owned())
    }
}

