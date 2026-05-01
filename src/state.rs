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
                    ~/.termal/termal.sqlite
AppState owns live coordination primitives that should not be serialized.
StateInner is the durable model plus counters and indexes protected by one
mutex.
*/

/// Wake signal sent to the background persist thread.
///
/// The persist thread owns an `Arc<Mutex<StateInner>>` and collects
/// the diff itself on each tick — it locks briefly, filters sessions
/// by `mutation_stamp > watermark`, clones only that subset plus app
/// metadata, drains `removed_session_ids`, then releases the lock and
/// writes to SQLite via `persist_delta_via_cache` (see `persist.rs`).
/// `PersistRequest` therefore carries only the wake signal; the full
/// `PersistedState` snapshot that earlier versions cloned under the
/// state mutex is no longer needed.
enum PersistRequest {
    /// Incremental persist: the thread looks up the current
    /// `last_mutation_stamp` and writes only the sessions that
    /// advanced past the thread's own watermark.
    Delta,
    /// Graceful-shutdown signal: the persist worker performs one final
    /// drain-and-write tick (so any pending mutation reaches SQLite),
    /// then exits its loop. The matching `JoinHandle` lives on
    /// `AppState::persist_thread_handle` and `AppState::shutdown_persist_blocking`
    /// is the documented shutdown entry point — see also bugs.md
    /// "Server restart without browser refresh can lose the last
    /// streamed message" for the durability contract this closes.
    Shutdown,
}

const REMOTE_DELTA_REPLAY_CACHE_LIMIT: usize = 2048;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RemoteDeltaReplayKey {
    remote_id: String,
    revision: u64,
    payload: RemoteDeltaReplayPayload,
}

/// Semantic identity for one remote delta replay key.
///
/// Every state-mutating field from the corresponding `DeltaEvent` variant must
/// be represented here directly or through a stable fingerprint. The replay
/// cache uses this value to distinguish exact same-revision redeliveries from
/// valid same-revision sibling deltas. See `wire::DeltaEvent` for the source
/// variants and `AppState::apply_remote_delta_event` for the consumer; new wire
/// fields must be added here and pinned by the `remote_delta_replay_key_*`
/// tests.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum RemoteDeltaReplayPayload {
    SessionCreated {
        session_id: String,
        message_count: u32,
        session_fingerprint: String,
        session_mutation_stamp: Option<u64>,
    },
    MessageCreated {
        session_id: String,
        message_id: String,
        message_index: usize,
        message_count: u32,
        message_fingerprint: String,
        preview_fingerprint: String,
        status: u8,
        session_mutation_stamp: Option<u64>,
    },
    MessageUpdated {
        session_id: String,
        message_id: String,
        message_index: usize,
        message_count: u32,
        message_fingerprint: String,
        preview_fingerprint: String,
        status: u8,
        session_mutation_stamp: Option<u64>,
    },
    TextDelta {
        session_id: String,
        message_id: String,
        message_index: usize,
        message_count: u32,
        delta_fingerprint: String,
        preview_fingerprint: Option<String>,
        session_mutation_stamp: Option<u64>,
    },
    TextReplace {
        session_id: String,
        message_id: String,
        message_index: usize,
        message_count: u32,
        text_fingerprint: String,
        preview_fingerprint: Option<String>,
        session_mutation_stamp: Option<u64>,
    },
    CommandUpdate {
        session_id: String,
        message_id: String,
        message_index: usize,
        message_count: u32,
        command_fingerprint: String,
        command_language: Option<String>,
        output_fingerprint: String,
        output_language: Option<String>,
        status: u8,
        preview_fingerprint: String,
        session_mutation_stamp: Option<u64>,
    },
    ParallelAgentsUpdate {
        session_id: String,
        message_id: String,
        message_index: usize,
        message_count: u32,
        agents_fingerprint: String,
        preview_fingerprint: String,
        session_mutation_stamp: Option<u64>,
    },
    CodexUpdated {
        codex_fingerprint: String,
    },
    OrchestratorsUpdated {
        orchestrator_fingerprints: Vec<String>,
        session_fingerprints: Vec<String>,
    },
}

/// Bounded exact remote-delta replay suppression.
///
/// Entries are keyed by remote id, remote revision, and semantic delta payload
/// identity. The per-remote cap is intentionally small because the cache only
/// covers the short same-revision replay window around remote event delivery;
/// older revisions still fall back to the monotonic remote-applied watermark.
/// Per-remote entries are cleared when event-stream continuity is lost. Memory
/// grows linearly with active remote count (`remotes * limit`), which is the
/// fairness tradeoff versus the previous globally-FIFO cache. Insertion is
/// O(total cache size) once a remote is over cap because we scan the shared
/// order queue to evict that remote's oldest entry; this is acceptable while
/// the cache is bounded and only records successfully applied deltas. If this
/// appears in profiles, split storage into per-remote queues to restore O(1)
/// eviction without losing cross-remote isolation.
#[derive(Default)]
struct RemoteDeltaReplayCache {
    keys: HashSet<RemoteDeltaReplayKey>,
    order: VecDeque<RemoteDeltaReplayKey>,
}

impl RemoteDeltaReplayCache {
    fn contains(&self, key: &RemoteDeltaReplayKey) -> bool {
        self.keys.contains(key)
    }

    fn insert(&mut self, key: RemoteDeltaReplayKey) {
        if self.keys.contains(&key) {
            return;
        }
        let remote_id = key.remote_id.clone();
        self.order.push_back(key.clone());
        self.keys.insert(key);
        let mut remote_entry_count = self
            .order
            .iter()
            .filter(|entry| entry.remote_id == remote_id)
            .count();
        while remote_entry_count > REMOTE_DELTA_REPLAY_CACHE_LIMIT {
            let Some(expired_index) = self
                .order
                .iter()
                .position(|entry| entry.remote_id == remote_id)
            else {
                break;
            };
            if let Some(expired) = self.order.remove(expired_index) {
                self.keys.remove(&expired);
                remote_entry_count -= 1;
            }
        }
    }

    fn remove_remote(&mut self, remote_id: &str) {
        self.keys.retain(|key| key.remote_id != remote_id);
        self.order.retain(|key| key.remote_id != remote_id);
    }
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
/// sweep. `drained_explicit_tombstones` keeps only the tombstones drained from
/// state so a failed write can restore those without duplicating hidden-session
/// deletes synthesized from still-hidden records.
#[cfg_attr(test, allow(dead_code))]
struct PersistDelta {
    metadata: PersistedState,
    changed_sessions: Vec<PersistedSessionRecord>,
    removed_session_ids: Vec<String>,
    drained_explicit_tombstones: Vec<String>,
    watermark: u64,
}

#[derive(Clone)]
struct AppState {
    /// Per-process UUID generated at `AppState::new_with_paths` boot.
    /// Carried on every `StateResponse` and `HealthResponse` so clients
    /// can distinguish "revision decreased because the server just
    /// restarted" from "revision decreased because this response is
    /// stale". The frontend's `shouldAdoptSnapshotRevision` uses a
    /// mismatch between this id and its `lastSeenServerInstanceIdRef`
    /// as the signal to accept a revision downgrade.
    server_instance_id: String,
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
    /// Handle to the background persist thread. Wrapped in
    /// `Arc<Mutex<Option<_>>>` so that:
    ///   - `AppState` stays `Clone` (the handle is shared, not duplicated),
    ///   - exactly one shutdown caller can `take()` the handle and join,
    ///   - subsequent shutdown calls are a safe no-op.
    /// Populated by `AppState::new_with_paths` after spawning the thread.
    /// `None` for test-only constructors that don't spawn the thread —
    /// `shutdown_persist_blocking` then has nothing to wait on.
    persist_thread_handle: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
    /// `true` while the background persist worker may still be alive and able
    /// to drain `PersistRequest::Delta` signals; flipped to `false` by
    /// `shutdown_persist_blocking` only AFTER the worker thread has joined.
    /// Read by `commit_delta_locked` (and any other path that stamps a
    /// mutation without sending its own persist signal) to switch from
    /// the async-worker path to a synchronous full-state JSON write. Once the
    /// flag is false, the worker is demonstrably gone and cannot race the
    /// synchronous fallback with its final drain/write; no future worker drain
    /// can persist a mutation stamped after that point.
    /// See bugs.md "Post-shutdown persistence writes still leave a
    /// post-collection-pre-join window".
    persist_worker_alive: Arc<std::sync::atomic::AtomicBool>,
    /// Graceful-shutdown signal for long-lived SSE streams. Triggered by
    /// `main.rs::shutdown_signal` (Ctrl+C / SIGTERM) before the
    /// `axum::serve` graceful-shutdown future resolves; SSE handlers
    /// `subscribe()` and exit their `tokio::select!` loops when the
    /// channel value flips to `true`. Without this, `with_graceful_shutdown`
    /// would block forever waiting for SSE streams to finish — the
    /// broadcast receivers in `state_events` only return `Closed` when the
    /// last `AppState` clone drops, but `shutdown_state` keeps a clone
    /// alive for the post-serve persist drain.
    ///
    /// Uses `tokio::sync::watch` rather than `tokio::sync::Notify` because
    /// `Notify::notify_waiters()` is *not* sticky: a waiter that subscribes
    /// after the notification fires never wakes. With `watch`, the value
    /// `true` is durable — a receiver constructed *after* `send(true)` can
    /// `borrow_and_update()` and see it immediately, so `/api/events`
    /// connections accepted just before Ctrl+C are guaranteed to observe
    /// the shutdown signal regardless of registration ordering. See
    /// bugs.md "One-shot SSE shutdown notification can be missed before
    /// waiter registration" / "Graceful shutdown blocks forever waiting
    /// for SSE streams to drain".
    shutdown_signal_tx: Arc<tokio::sync::watch::Sender<bool>>,
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
    /// Exact remote-delta payload identities for every successfully applied
    /// inbound delta. Suppresses same-revision payload-identical replays from a
    /// misbehaving remote/SSE retry; sibling same-revision deltas with different
    /// payloads still apply. Per-remote entries are cleared on event-stream
    /// continuity loss.
    remote_delta_replay_cache: Arc<Mutex<RemoteDeltaReplayCache>>,
    /// Remote session transcript hydrations currently being fetched because a
    /// delta reached an unloaded proxy session. Keyed by `(remote_id,
    /// remote_session_id)` so concurrent same-session deltas do not fan out
    /// duplicate blocking `/api/sessions/{id}` requests; the first fetch repairs
    /// the transcript and the later deltas can continue through the narrow
    /// delta path.
    remote_delta_hydrations_in_flight: Arc<Mutex<HashSet<(String, String)>>>,
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
    /// Remote revision watermarks are ordered from weakest to strongest:
    /// `remote_applied_revisions`, `remote_snapshot_applied_revisions`, then
    /// `remote_transcript_snapshot_applied_revisions`. Recording a stronger
    /// broad-state watermark also records every weaker one. The skip checks use
    /// `>=` only when that watermark already materialized the same kind of data
    /// at the supplied revision; otherwise they use `>` so same-revision repair
    /// paths can still fill data omitted by a narrower event.
    ///
    /// Focused `/api/sessions/{id}` hydration is narrower than a broad
    /// transcript snapshot, so it uses
    /// `remote_session_transcript_applied_revisions` instead of bumping the
    /// broad transcript watermark. That suppresses duplicate same-revision
    /// transcript deltas for the hydrated session without dropping unrelated
    /// sessions from the same remote revision.
    /// Tracks the latest remote revision applied for each mirrored remote so
    /// stale snapshots and deltas cannot roll local proxy state backward.
    remote_applied_revisions: HashMap<String, u64>,
    /// Tracks the latest broad full-state snapshot applied for each mirrored
    /// remote. Cleared with `remote_applied_revisions` and the per-remote replay
    /// cache when event-stream continuity is lost.
    remote_snapshot_applied_revisions: HashMap<String, u64>,
    /// Tracks the latest broad snapshot that fully materialized session
    /// transcripts for a mirrored remote. Same-revision deltas are allowed after
    /// metadata-first snapshots because those snapshots may omit the transcript
    /// bytes carried by the delta.
    remote_transcript_snapshot_applied_revisions: HashMap<String, u64>,
    /// Tracks focused full-session transcript hydration per remote session.
    /// This is intentionally narrower than `remote_transcript_snapshot_applied_revisions`.
    remote_session_transcript_applied_revisions: HashMap<String, HashMap<String, u64>>,
    /// Session records carry the serializable session plus runtime-only state.
    sessions: Vec<SessionRecord>,
    /// Runtime instances for orchestrator templates live beside ordinary sessions.
    orchestrator_instances: Vec<OrchestratorInstance>,
    /// Durable parent-child delegation links for ordinary sessions.
    delegations: Vec<DelegationRecord>,
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
            remote_snapshot_applied_revisions: HashMap::new(),
            remote_transcript_snapshot_applied_revisions: HashMap::new(),
            remote_session_transcript_applied_revisions: HashMap::new(),
            sessions: Vec::new(),
            orchestrator_instances: Vec::new(),
            delegations: Vec::new(),
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

    /// Returns whether the supplied remote state snapshot revision is stale for
    /// this remote. Same-revision snapshots are allowed after ordinary deltas so
    /// repair paths can materialize state after a sibling delta at that revision.
    fn should_skip_remote_applied_snapshot_revision(
        &self,
        remote_id: &str,
        remote_revision: u64,
    ) -> bool {
        self.remote_applied_revisions
            .get(remote_id)
            .is_some_and(|latest_revision| *latest_revision > remote_revision)
            || self
                .remote_snapshot_applied_revisions
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
            || self
                .remote_snapshot_applied_revisions
                .get(remote_id)
                .is_some_and(|latest_revision| *latest_revision > remote_revision)
            || self
                .remote_transcript_snapshot_applied_revisions
                .get(remote_id)
                .is_some_and(|latest_revision| *latest_revision >= remote_revision)
    }

    /// Returns whether a session-scoped remote delta is stale for this remote
    /// session. This extends the broad remote delta rule with focused transcript
    /// hydration for one remote session only.
    fn should_skip_remote_session_applied_delta_revision(
        &self,
        remote_id: &str,
        remote_session_id: &str,
        remote_revision: u64,
    ) -> bool {
        self.should_skip_remote_applied_delta_revision(remote_id, remote_revision)
            || self
                .remote_session_transcript_applied_revisions
                .get(remote_id)
                .and_then(|sessions| sessions.get(remote_session_id))
                .is_some_and(|latest_revision| *latest_revision >= remote_revision)
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

    /// Records that a broad full-state snapshot, not just a narrow delta or
    /// focused session response, has materialized this remote revision.
    fn note_remote_applied_snapshot_revision(&mut self, remote_id: &str, remote_revision: u64) {
        self.note_remote_applied_revision(remote_id, remote_revision);
        self.remote_snapshot_applied_revisions
            .entry(remote_id.to_owned())
            .and_modify(|latest_revision| {
                *latest_revision = (*latest_revision).max(remote_revision);
            })
            .or_insert(remote_revision);
    }

    /// Records that a broad full-state snapshot included complete session
    /// transcripts for this remote revision.
    fn note_remote_applied_transcript_snapshot_revision(
        &mut self,
        remote_id: &str,
        remote_revision: u64,
    ) {
        self.note_remote_applied_snapshot_revision(remote_id, remote_revision);
        self.remote_transcript_snapshot_applied_revisions
            .entry(remote_id.to_owned())
            .and_modify(|latest_revision| {
                *latest_revision = (*latest_revision).max(remote_revision);
            })
            .or_insert(remote_revision);
    }

    /// Records that focused full-session hydration materialized a single remote
    /// session transcript at this revision.
    fn note_remote_session_transcript_applied_revision(
        &mut self,
        remote_id: &str,
        remote_session_id: &str,
        remote_revision: u64,
    ) {
        self.remote_session_transcript_applied_revisions
            .entry(remote_id.to_owned())
            .or_default()
            .entry(remote_session_id.to_owned())
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

