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

