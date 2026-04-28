# TermAl — Architecture

> A WhatsApp-style interface for controlling AI coding agents running on your machine.

---

## System Overview

```text
Browser UI
  -> /api + /api/events
  -> local TermAl server
       -> AppState / StateInner / persistence
       -> shared Codex app-server
       -> per-session Claude runtime
       -> per-session ACP runtimes (Cursor / Gemini)
       -> RemoteRegistry (SSH tunnels + remote event bridges)

Optional sidecar:
  telegram mode -> project digest/actions -> same local TermAl server
```

**Frontend:** React 18 + TypeScript, served on `:4173` in dev with a Vite proxy to the backend.
**Backend:** Rust + axum + tokio, bound to `127.0.0.1:8787` by default, overridable with `TERMAL_PORT`.
**Persistence:** `~/.termal/termal.sqlite` stores sessions, projects, preferences, remote config, workspace layouts, and orchestrator instances. `~/.termal/orchestrators.json` stores reusable orchestrator templates.
**Real-time:** Server-Sent Events with a monotonic revision counter for ordering.

**Current status:** The current implementation includes server-backed workspace layouts, project-scoped SSH remotes, orchestrator templates and runtime instances, session-scoped model controls, workspace terminal tabs, file-change awareness, and the Telegram relay.

**Remote model:** The browser connects to a single local TermAl server. That
server stores preferences, manages remote connections, and routes project work
to local or remote TermAl servers over SSH-managed tunnels.

---

## Backend

### Entry Points

The binary has three modes:

1. **Server mode** (default) - starts an axum HTTP server on `127.0.0.1:8787` by default, serves the API, and manages long-lived agent processes. `TERMAL_PORT` can override the port.
2. **REPL mode** (`repl`, `cli`, or an agent shortcut such as `codex` / `claude`) - interactive terminal loop. Reads prompts from stdin and runs one turn at a time via `run_turn_blocking()`.
3. **Telegram mode** (`telegram` or `telegram-bot`) - long-polling relay that turns project digests and project actions into a Telegram bot workflow.

### Core State

```rust
AppState {
    default_workdir: String,
    persistence_path: Arc<PathBuf>,            // ~/.termal/termal.sqlite
    orchestrator_templates_path: Arc<PathBuf>, // ~/.termal/orchestrators.json
    state_events: broadcast::Sender<String>,
    delta_events: broadcast::Sender<String>,
    file_events: broadcast::Sender<String>,    // workspace file-watcher fan-out
    shared_codex_runtime: Arc<Mutex<Option<SharedCodexRuntime>>>,
    remote_registry: Arc<RemoteRegistry>,
    persist_tx: mpsc::Sender<PersistRequest>,  // wake the background persist thread
    inner: Arc<Mutex<StateInner>>,
}

StateInner {
    codex: CodexState,
    preferences: AppPreferences,
    revision: u64,
    next_project_number: usize,
    next_session_number: usize,
    next_message_number: u64,
    projects: Vec<Project>,
    ignored_discovered_codex_thread_ids: BTreeSet<String>,
    sessions: Vec<SessionRecord>,
    orchestrator_instances: Vec<OrchestratorInstance>,
    workspace_layouts: BTreeMap<String, WorkspaceLayoutDocument>,
}
```

`AppState` is the live coordination shell: SSE broadcasters, the shared Codex app-server handle, and the SSH remote registry all live there. `StateInner` is the mutex-protected durable model that gets serialized to disk.

**SessionRecord** wraps the serializable `Session` with runtime-only fields:

```rust
SessionRecord {
    session: Session,                          // id, agent, model, messages, preview, status
    runtime: SessionRuntime,                   // None | Claude | Codex | Acp
    pending_claude_approvals: HashMap,
    pending_codex_approvals: HashMap,
    pending_codex_user_inputs: HashMap,
    pending_codex_mcp_elicitations: HashMap,
    pending_codex_app_requests: HashMap,
    pending_acp_approvals: HashMap,
    queued_prompts: VecDeque<QueuedPromptRecord>,
    remote_id: Option<String>,                 // remote owning the proxy session
    remote_session_id: Option<String>,         // remote session id when proxied
    external_session_id: Option<String>,       // Claude/Codex/ACP resume identifier
    runtime_reset_required: bool,
    hidden: bool,
}
```

### State Mutation Pattern

All client-visible state changes go through `commit_locked()`:

```
commit_locked(&mut inner)
  → inner.revision += 1
  → persist_tx.send(PersistRequest::Delta) // wake the background persist thread
  → publish_state_locked(inner)            // broadcast metadata-first StateResponse on SSE
  → Ok(revision)
```

The background `termal-persist` thread owns an `Arc<Mutex<StateInner>>`
and a `SqlitePersistConnectionCache`. On each `Delta` wake it briefly
locks `inner`, collects the diff via
`StateInner::collect_persist_delta(watermark)` (only sessions whose
`mutation_stamp` advanced past the thread's watermark, plus drained
`removed_session_ids`), releases the lock, and writes with targeted
`INSERT OR UPDATE` per changed session and `DELETE WHERE id = ?` per
removed id. Unchanged session rows stay untouched — a mutation on one
session no longer rewrites every other session row every commit. See
`src/state.rs` for `PersistRequest` / `PersistDelta` and `src/persist.rs`
for `persist_delta_via_cache`.

Mutation stamping is load-bearing: every session mutation must land
through `StateInner::session_mut` / `session_mut_by_index` /
`stamp_session_at_index` / `push_session` / `remove_session_at` /
`retain_sessions` so the record's `mutation_stamp` gets bumped. A
raw `&mut inner.sessions[idx]` would skip the stamp and the delta
persist would drop the update. See `src/state_inner.rs` for the
helpers.

Streaming paths (`append_text_delta`, `update_command_message`) bump revision and publish a `DeltaEvent` instead of a full snapshot, avoiding the cost of serializing all sessions on every token. They use `commit_delta_locked()` which bumps revision + wakes the persist thread but skips the full-state broadcast; callers emit the matching `DeltaEvent` explicitly via `publish_delta()` under the same lock.

Internal bookkeeping that the frontend doesn't need (e.g. recording Codex sandbox mode after runtime config) uses `persist_internal_locked()` directly without bumping revision.

### HTTP API

All routes are under `/api`. The backend serves JSON, and the frontend proxies requests through Vite in development.

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/api/health` | Health check + capability probe |
| GET | `/api/file?path=...` | Read file content |
| PUT | `/api/file` | Write file content |
| GET | `/api/fs?path=...` | List directory entries |
| GET | `/api/git/status?path=...` | Git status and branch info |
| POST | `/api/git/diff` | Build a structured git diff preview |
| POST | `/api/git/file` | Apply a file-level git action |
| POST | `/api/git/commit` | Create a git commit from staged changes |
| POST | `/api/git/push` | Push the current repo |
| POST | `/api/git/sync` | Pull, rebase, or otherwise sync the current repo |
| POST | `/api/terminal/run` | Run a shell command in a project- or session-scoped working directory. Request body enforces `command` ≤ 20,000 chars and `workdir` ≤ 4,096 chars (no interior NUL bytes), and captured output is capped. There is no process timeout. Returns 429 (`{ "error": ... }`) when the concurrency cap for that destination is exhausted; local and remote commands have independent budgets of 4 in-flight requests each. When the destination is remote, a 429 emitted by the remote host is re-emitted locally with the remote's display name prefixed onto the error message (e.g. `remote alice: too many local terminal commands are already running; limit is 4`), so the caller can distinguish a local cap rejection from a remote-side propagation. |
| POST | `/api/terminal/run/stream` | Run the same terminal command as `/api/terminal/run`, but return an SSE stream. `output` events carry `{ "stream": "stdout" \| "stderr", "text": string }`, `complete` carries the normal terminal response, and `error` carries `{ "error": string, "status": number }` for failures after the stream has started. Validation, workdir/scope resolution, and local concurrency-cap failures are returned as normal HTTP errors before the stream starts; local cap failures use HTTP 429 with `{ "error": ... }` and the same independent local/remote 4-in-flight budgets as the JSON route. Remote 429s discovered by the proxy are surfaced with `status: 429` and the remote display-name prefix in the error message; after the local SSE response has started they travel as SSE `error` frames rather than changing the local HTTP status. There is no process timeout. Remote-scoped commands proxy this streamed route when the remote supports it and fall back to the JSON route only for 404/405 older-remotes responses; successful non-SSE stream responses are treated as remote protocol errors to avoid double-running commands. |
| GET | `/api/state` | Metadata-first state snapshot; sessions are summary shells with `messagesLoaded: false` and no transcript payload |
| GET | `/api/workspaces` | List saved workspace layout summaries |
| GET | `/api/workspaces/{id}` | Read a persisted workspace layout |
| PUT | `/api/workspaces/{id}` | Save a persisted workspace layout |
| DELETE | `/api/workspaces/{id}` (200) -> `WorkspaceLayoutsResponse` | Delete a persisted workspace layout and return the remaining layout summaries |
| POST | `/api/settings` | Update app-wide preferences and remote config |
| GET | `/api/orchestrators/templates` | List orchestrator templates |
| POST | `/api/orchestrators/templates` | Create orchestrator template |
| GET | `/api/orchestrators/templates/{id}` | Read orchestrator template |
| PUT | `/api/orchestrators/templates/{id}` | Update orchestrator template |
| DELETE | `/api/orchestrators/templates/{id}` (200) -> `OrchestratorTemplatesResponse` | Delete orchestrator template and return the remaining template list so the client can replace local state after deletion |
| GET | `/api/orchestrators` | List orchestrator instances |
| POST | `/api/orchestrators` | Create orchestrator instance |
| GET | `/api/orchestrators/{id}` | Read orchestrator instance |
| POST | `/api/orchestrators/{id}/pause` | Pause an orchestrator instance -> `StateResponse` |
| POST | `/api/orchestrators/{id}/resume` | Resume an orchestrator instance -> `StateResponse` |
| POST | `/api/orchestrators/{id}/stop` | Stop an orchestrator instance -> `StateResponse` |
| GET | `/api/instructions/search` | Search instruction files for a session/workdir |
| GET | `/api/events` | SSE stream (state + delta events) |
| GET | `/api/reviews/{change_set_id}` | Read a persisted diff review document |
| PUT | `/api/reviews/{change_set_id}` | Save a persisted diff review document |
| GET | `/api/reviews/{change_set_id}/summary` | Read review-thread summary counts |
| POST | `/api/projects` | Create project |
| DELETE | `/api/projects/{id}` | Remove the local project reference and return `StateResponse`. Existing sessions and orchestrator instances are detached from the project and remain visible outside project scope. Remote-backed projects are removed only from local state; this route does not delete project data on the remote backend. |
| GET | `/api/projects/{id}/digest` | Read the project digest used by Telegram/mobile workflows |
| POST | `/api/projects/{id}/actions/{action_id}` | Dispatch a digest action such as approve, continue, or stop |
| POST | `/api/projects/pick` | Pick a local project root |
| POST | `/api/sessions` | Create session |
| GET | `/api/sessions/{id}` | Fetch one session -> `SessionResponse { revision, serverInstanceId, session }`. Local sessions return full transcripts; remote-proxy sessions can return an unloaded cached summary (`messagesLoaded: false`) on recoverable hydration fallback. |
| POST | `/api/sessions/{id}/settings` | Update session config |
| POST | `/api/sessions/{id}/model-options/refresh` | Refresh live model list/options |
| POST | `/api/sessions/{id}/codex/thread/fork` | Fork the live Codex thread into a new session |
| POST | `/api/sessions/{id}/codex/thread/archive` | Archive the live Codex thread |
| POST | `/api/sessions/{id}/codex/thread/unarchive` | Restore an archived Codex thread |
| POST | `/api/sessions/{id}/codex/thread/compact` | Request Codex thread compaction |
| POST | `/api/sessions/{id}/codex/thread/rollback` | Roll back the live Codex thread |
| GET | `/api/sessions/{id}/agent-commands` | Read local agent-command shortcuts |
| POST | `/api/sessions/{id}/messages` | Send message |
| POST | `/api/sessions/{id}/queued-prompts/{prompt_id}/cancel` | Cancel queued prompt |
| POST | `/api/sessions/{id}/stop` | Stop active turn |
| POST | `/api/sessions/{id}/kill` | Kill and remove session |
| POST | `/api/sessions/{id}/approvals/{message_id}` | Submit approval decision |
| POST | `/api/sessions/{id}/user-input/{message_id}` | Submit structured Codex user-input answers |
| POST | `/api/sessions/{id}/mcp-elicitation/{message_id}` | Submit an MCP elicitation response |
| POST | `/api/sessions/{id}/codex/requests/{message_id}` | Reply to a generic Codex app-server request |

For local sessions, `GET /api/sessions/{id}` is a local full-transcript read. For
unloaded remote-proxy sessions, the same route synchronously calls the owning
remote's `GET /api/sessions/{remote_id}` and may also call remote `/api/state`
when the remote session response is ahead of the locally applied remote
revision. That `/api/state` fetch is a freshness check and global remote resync:
it can update other proxy records for the same remote and publish local state
before returning the requested `SessionResponse`. If the upstream session route
cannot provide a full transcript before the visible-pane timeout, returns
`messagesLoaded: false`, or loses a freshness race with a newer remote-state
snapshot, TermAl can return the cached local summary with
`messagesLoaded: false`. Frontend targeted hydration must keep that response
unloaded and retry later rather than treating HTTP success as full transcript
hydration.

`GET /api/health` currently returns `{ ok: true, supportsInlineOrchestratorTemplates: true }`. Remote launchers use `supportsInlineOrchestratorTemplates` during health probes to decide whether a remote can accept inline local orchestrator templates or must be upgraded first.

### Terminal Command Execution

Workspace terminal tabs are command runners, not full PTY emulators. Each run
executes one shell command in a session- or project-scoped working directory and
records a history entry inside the tab. The frontend uses the streamed endpoint
by default so stdout and stderr arrive incrementally.

Design constraints:

- Local and remote commands have independent concurrency caps of four in-flight
  commands each; rejected requests return 429.
- Commands intentionally have no production timeout because users run long-lived
  foreground tools such as dev servers, `flutter run`, watches, and REPL-like
  processes.
- Captured stdout/stderr are bounded and marked truncated when the cap is hit.
- Local streamed commands observe SSE disconnects and kill the local process
  tree when the user leaves the stream.
- Remote streamed commands proxy the remote `/api/terminal/run/stream` SSE
  contract when available, and fall back to the JSON route only when the remote
  returns 404 or 405 for the stream route.
- Remote successful non-SSE responses are protocol errors rather than a reason
  to run the command again through the JSON endpoint.

### SSE Event Stream

`GET /api/events` returns a Server-Sent Events stream with three event types:

- **`state`** — metadata-first `StateResponse` JSON. Sent on initial connect, after `commit_locked()`, and as a recovery when the client falls behind. Session entries carry shell metadata, `messageCount`, and `messagesLoaded: false`; full transcripts are loaded through `GET /api/sessions/{id}`.
- **`delta`** - incremental `DeltaEvent` JSON. Sent during streaming (text deltas, text replacements, command output updates) and small process-global updates such as `CodexUpdated`. Cheaper than full state.
- **`workspaceFilesChanged`** - coalesced local workspace file watcher hints. Sent outside the main state revision stream with its own monotonically increasing file-event revision so source, diff, file tree, and git-preview panels can refresh only when touched paths match their scope.

All three carry a `revision: u64` field. `state` and `delta` share the main state revision counter, which the frontend uses to reject stale snapshots and detect gaps in the delta sequence. `workspaceFilesChanged` uses a separate file-event revision counter; the frontend batches same-tick file events and ignores file-event revisions strictly older than the last seen revision (same-revision events are merged while buffered).

`state` events and every snapshot-bearing response (`StateResponse`, `HealthResponse`, `CreateSessionResponse`, `SessionResponse`) additionally carry a `serverInstanceId: string` — a per-process UUID generated once via `Uuid::new_v4()` at `AppState::new_with_paths`. The id is not a secret and not a protocol boundary; it exists so the frontend can detect a server restart deterministically. After a restart, the revision counter rewinds to whatever SQLite held (usually lower than the browser's last-seen revision), which would otherwise cause every monotonic check in `shouldAdoptStateRevision` to reject the fresh state. `isServerInstanceMismatch` in `ui/src/state-revision.ts` returns `true` only when both the last-seen and incoming ids are non-empty AND differ; `shouldAdoptSnapshotRevision` accepts only unseen mismatched ids as restarts; mismatched ids already seen by the tab are rejected as late responses from older server processes. The unseen-restart branch overrides both the monotonic check and any `allowRevisionDowngrade` gate. The empty-string sentinel (`#[serde(default)]` on Rust, `""` fallback on older servers or fallback SSE payloads) means "unknown instance" and cannot trigger a restart branch — this is what lets `empty_state_events_response()` send a fallback payload without masquerading as a restart. New endpoints that return state-shaped responses must emit a non-empty `serverInstanceId` sourced from `AppState::server_instance_id`; otherwise a session hydration in flight across a restart gets silently rejected by the revision guard until the safety-net pollers re-fetch.

Every `Session` or session summary serialized on the wire carries
`messageCount: u32`. `StateResponse.sessions` are metadata-first summary
shells: they retain normal session metadata, set `messagesLoaded: false`, and
keep `messages: []` only as a temporary adapter-compatible shape while the
frontend migration is in progress. Full transcript-bearing sessions still come
from `SessionResponse` and `CreateSessionResponse`. `SessionCreated` and
`OrchestratorsUpdated.sessions` are metadata-first delta summaries with
`messagesLoaded: false` and `messages: []`. The backend computes
`messageCount` from the session record's transcript at wire-projection time;
the frontend keeps it on the session summary so reconnect/state adoption can
preserve transcript height and gap-detection metadata without waiting for
another session-scoped delta.

`Session.messageCount` is soft-rollout compatible via `#[serde(default)]`, but
`DeltaEvent.*.messageCount` is intentionally required on the wire. Mixed-version
remote SSE bridges that omit delta counts are treated as a hard protocol break;
see `docs/metadata-first-state-plan.md` Contract Precisions -> Field semantics.

```
DeltaEvent::TextDelta            { revision, session_id, message_id, message_index, message_count, delta, preview, session_mutation_stamp? }
DeltaEvent::TextReplace          { revision, session_id, message_id, message_index, message_count, text, preview, session_mutation_stamp? }
DeltaEvent::CommandUpdate        { revision, session_id, message_id, message_index, message_count, command, output, status, preview, session_mutation_stamp?, ... }
DeltaEvent::ParallelAgentsUpdate { revision, session_id, message_id, message_index, message_count, agents, preview, session_mutation_stamp? }
DeltaEvent::MessageCreated       { revision, session_id, message_id, message_index, message_count, message, preview, status, session_mutation_stamp? } // inserts a new message at message_index; if the id already exists, remove and reinsert it at that literal index
DeltaEvent::MessageUpdated       { revision, session_id, message_id, message_index, message_count, message, preview, status, session_mutation_stamp? } // replaces an existing message in place; message_index is a fast-path hint and must not reorder the transcript
DeltaEvent::SessionCreated       { revision, session_id, session } // metadata-first session summary; local + remote-proxied session creation; forwarded by remote backends after id localization
DeltaEvent::CodexUpdated         { revision, codex } // latest process-global CodexState snapshot; remotes consume the revision for ordering but do not localize Codex state into proxy sessions
DeltaEvent::OrchestratorsUpdated { revision, orchestrators[], sessions[] } // sessions[] contains metadata-first summaries for referenced sessions and is omitted on the wire when empty; IDs inside each instance are scoped to the originating server; translate via sync_remote_state_inner before forwarding remotely.
```

For inbound remote session-scoped deltas, `session_mutation_stamp?` is a
freshness marker when present. A missing stamp means "unknown", not "clear the
cached stamp", so receivers preserve any prior cached stamp and let later
metadata-only summaries decide whether targeted hydration is needed.

When a delta targets an unloaded remote-proxy session, TermAl repairs that
single transcript with remote `GET /api/sessions/{id}`. The returned
`SessionResponse.revision` is a remote-global revision, not a per-session
freshness marker: it may be greater than the triggering delta revision because
unrelated sessions changed upstream. The targeted repair accepts that newer
global revision only when the returned session's `(sessionMutationStamp,
messageCount)` exactly matches the triggering delta's post-state metadata. If
the stamp is missing or mismatched, the repair is rejected and the remote event
bridge falls back to `/api/state` resync so a future same-session transcript is
not localized early and then replayed again by later deltas. Successful remote
delta applications record a bounded in-memory replay key from the remote
revision plus the delta's semantic payload identity. Cheap variants use
structural fields such as session/message ids, message index, message count, and
mutation stamp; content-bearing or complex variants also include the exact
mutating payload, or a stable fingerprint of that payload, so distinct
same-revision sibling deltas still apply. Replay keys are cleared with the
remote applied-revision watermark when event-stream continuity is lost.
Targeted repairs record a session-specific transcript watermark at the returned
remote response revision, but keep the broad remote watermark at the triggering
delta revision so same-session stale deltas are skipped without suppressing
unrelated intermediate deltas from other sessions.

```
WorkspaceFilesChangedEvent {
  revision,
  changes: [
    { path, kind, rootPath?, sessionId?, mtimeMs?, sizeBytes? }
  ]
}
```

`kind` is `created`, `modified`, `deleted`, or `other`. `rootPath` and `sessionId`
scope a watcher hint when it can be tied to a project root or session workdir;
unscoped events still carry the absolute changed path as a fallback.

`TextDelta` appends streaming text to an in-progress message. `TextReplace` overwrites the full message text when the backend receives an authoritative completed payload that diverges from the streamed draft, so clients should replace the target message body instead of appending.

On broadcast channel lag, the backend falls back to sending a full state snapshot.

### Persistence

```
~/.termal/
|-- termal.sqlite          # primary store: app_state + sessions tables (+ WAL/-shm sidecars)
|-- orchestrators.json     # reusable orchestrator templates
`-- telegram-bot.json      # optional Telegram relay chat binding
```

`PersistedState` is the logical projection of `StateInner` that excludes
runtime handles, pending approval maps, and empty collections. On disk it
splits across two SQLite tables: `app_state` (one row per schema version +
one metadata row carrying preferences, projects, remotes, workspaces, and
bookkeeping counters) and `sessions` (one row per session keyed by id,
value_json carrying the serialized `PersistedSessionRecord`). This two-table
split lets the background persist thread write only the **changed** session
rows on each commit — see `collect_persist_delta`, `persist_delta_via_cache`,
and `SqlitePersistConnectionCache` in `src/persist.rs`.

On startup, the backend loads state from `termal.sqlite` when it exists and
otherwise boots a fresh local state. Template definitions live in
`orchestrators.json` so reusable workflow designs can be managed separately
from running instances.

---

## Remote Architecture

The implemented remote architecture is:

`Browser -> local TermAl server -> remote TermAl server`

The browser does not manage multiple backend origins directly. Instead, the
local server is the control plane and exposes the single browser-facing `/api`
and `/api/events` interface.

### Topology

#### Remote Connection Diagram

```
┌──────────────────────────────┐
│ Browser UI                   │
│ React app                    │
│ - one /api origin            │
│ - one /api/events stream     │
└──────────────┬───────────────┘
               │ HTTP + SSE
               ▼
┌─────────────────────────────────────────────────────────────┐
│ Local TermAl Server                                         │
│ Control plane                                               │
│ - stores preferences and remote config                      │
│ - owns browser-facing REST + SSE                            │
│ - maps project -> remoteId                                  │
│ - rewrites ids and aggregates state                         │
│ - supervises SSH sessions and remote servers                │
└──────────────┬───────────────────────────────┬──────────────┘
               │                               │
               │ local execution               │ SSH managed start + persistent tunnel
               ▼                               ▼
┌──────────────────────────────┐   ┌──────────────────────────────────────────┐
│ Local machine runtime        │   │ Remote machine                           │
│ LocalConnector               │   │ sshd                                     │
│ - local projects             │   │  └─ runs or reuses `termal server`       │
│ - local agent processes      │   │     through ssh -L port forwarding       │
│                              │   │     bound to 127.0.0.1 on remote host    │
└──────────────────────────────┘   └───────────────────┬──────────────────────┘
                                                       │ tunneled HTTP + SSE
                                                       ▼
                                    ┌──────────────────────────────────────────┐
                                    │ Remote TermAl Server                     │
                                    │ SshConnector target                      │
                                    │ - remote projects                        │
                                    │ - remote sessions                        │
                                    │ - remote agent runtimes                  │
                                    └──────────────────────────────────────────┘
```

#### Project Routing Diagram

```
Project selection in UI
        │
        ▼
projectId -> remoteId lookup in local control plane
        │
        ├─ remoteId = local
        │      -> LocalConnector
        │      -> local TermAl execution
        │
        └─ remoteId = build-box / laptop / workstation
               -> SshConnector
               -> SSH tunnel
               -> remote TermAl execution
```

For a remote machine:

1. The local TermAl server uses the system `ssh` client to connect to the
   remote host.
2. Managed mode runs `termal server` on the remote over that SSH session.
3. If managed mode does not become healthy, TermAl falls back to tunnel-only
   mode (`ssh -N`) and expects a TermAl server to already be listening on the
   remote host.
4. The remote TermAl server listens on `127.0.0.1:8787` on the remote machine.
5. The local TermAl server keeps a persistent local-forward tunnel to that
   remote server.
6. The local TermAl server speaks the normal TermAl HTTP and SSE protocol over
   that tunnel.

This is intentionally similar to the Remote-SSH shape used by editor tooling:
SSH is used to reach the machine, start the remote server, and carry the
transport. The browser still only talks to the local control plane.
The current remote config stores only connection settings (`id`, `name`,
`transport`, `enabled`, `host`, `port`, and `user`). Source checkout management,
binary installation, and remote upgrade orchestration are intentionally outside
the current shipped config surface.

### Control Plane Responsibilities

The local TermAl server owns:

- preferences and remote configuration
- the built-in local machine connection
- project-to-remote routing
- browser-facing state aggregation
- browser-facing SSE aggregation
- id rewriting or namespacing across remotes
- connection supervision and reconnect behavior

The local server is therefore both:

- the local execution backend for projects assigned to the local machine
- the coordinator for projects assigned to remote machines

### Project-Scoped Routing

Remote ownership is assigned at the project level.

- Each project has a `remoteId`.
- Each session belongs to a project.
- Each session inherits its routing from the owning project.
- File, directory, git, review, terminal, and session creation flows route by
  project ownership.

This avoids teaching the UI to choose a backend for every action. The user
chooses a remote when creating a project, and the rest of the routing follows
from that association.

### Session and Project Identity

Remote-native ids cannot be trusted to be globally unique across multiple
machines. The local control plane therefore exposes local browser-facing ids for
proxy projects, sessions, and orchestrator instances, while storing the
remote-native ids in runtime/persisted mapping fields such as
`remote_session_id`, `remote_project_id`, and `remote_orchestrator_id`.

The browser should treat those local ids as canonical. Remote-native ids remain
an internal proxying detail.

### State and Event Aggregation

The browser consumes one state stream from the local control plane.

That means the local server:

- fetches or subscribes to state from each configured remote
- merges those states into one browser-facing `StateResponse`
- rewrites project/session/orchestrator ids into browser-safe local ids
- emits one aggregate SSE stream
- uses its own aggregate revision counter instead of forwarding raw remote
  revisions directly

The frontend does not need to know whether a project is local or remote in
order to consume normal state and delta updates.

### SSH as the Permanent Remote Transport

SSH is not just a bootstrap convenience for the first version. It is the
intended long-term remote transport model.

Design constraints:

- The remote TermAl server should not be exposed publicly by default.
- The local control plane should prefer one persistent SSH session or tunnel per
  remote, not one SSH command per API call.
- SSE must travel over a stable, long-lived transport.
- The local server should supervise both the SSH connection and the remote
  `termal server` lifecycle.
- System `ssh` and `ssh-agent` should be preferred over custom browser-managed
  key handling.

### Managed SSH Startup

The current managed startup mode is intentionally small:

1. Build an SSH command with batch mode, local port forwarding, and keepalive
   settings.
2. Run `termal server` on the remote host.
3. Probe the forwarded local URL until `/api/health` succeeds.
4. Cache the remote capabilities and begin proxying REST/SSE through the
   tunnel.
5. If that path fails, try tunnel-only mode and probe the same forwarded URL.

This keeps the transport model simple and uses the user's normal system SSH
configuration and `ssh-agent`. It does not currently update remote source
checkouts or install TermAl binaries.

### API Shape

The remote TermAl server exposes the same HTTP and SSE protocol shape as a local
TermAl server as much as possible.

This keeps the system simpler:

- local execution can use the same backend contract as remote execution
- the local control plane can proxy or adapt requests through one transport
  abstraction
- remote machines remain regular TermAl servers rather than a second bespoke
  protocol

Recommended control-plane connector abstraction:

- `ensure_server_running`
- `request`
- `open_event_stream`

With at least two implementations:

- `LocalConnector`
- `SshConnector`

### UI Implications

The UI now follows these constraints:

- one browser connection to the local TermAl server
- a Settings > Remotes surface for SSH configuration
- project creation with a remote selector
- session creation scoped to a project
- remote-aware project/session labels and errors

The UI should still not evolve toward:

- direct browser connections to multiple backends
- one backend picker per action
- independent frontend-managed SSE connections per remote

---

## Agent Integration

### Claude Code

**Invocation:**
```bash
claude -p --output-format stream-json --input-format stream-json \
  --verbose --permission-prompt-tool stdio --include-partial-messages \
  --resume <external_session_id>   # if resuming
```

**Environment:** `CLAUDE_CODE_ENTRYPOINT=termal`

**Protocol:** Bidirectional NDJSON over stdin/stdout. One process per session, long-lived across turns.

**Thread architecture:** 4 dedicated threads per runtime:
1. **Writer** — receives `ClaudeRuntimeCommand` from an mpsc channel, serializes to NDJSON, writes to stdin
2. **Reader** — reads stdout line-by-line, parses JSON, routes events to `AppState` methods
3. **Stderr** — logs Claude's stderr output
4. **Waiter** — polls `child.try_wait()` to detect process exit

**Lifecycle:**
1. Spawn process → send `control_request { subtype: "initialize" }` → receive `control_response` with pid, models, commands
2. On user message → write `{ type: "user", message: { role: "user", content: [...] } }` to stdin
3. Receive streaming events: `assistant` (text, tool_use, tool_result), `result` (turn complete)
4. On tool approval needed → Claude sends `control_request { subtype: "can_use_tool" }` → TermAl either auto-approves or shows approval card → sends `control_response` with decision

**Session resume:** Pass `--resume <session_id>` on spawn. Claude restores full conversation context from its own `~/.claude/sessions/` storage.

### Codex

**Invocation:**
```bash
codex app-server   # JSON-RPC over stdin/stdout
```

**Protocol:** JSON-RPC 2.0 over stdio. One shared app-server process is reused across all live Codex sessions, and each session is mapped onto its own Codex thread inside that process.

**Thread architecture:** The shared process uses four helper threads:
1. **Writer** — serializes queued commands and JSON-RPC responses to stdin. All JSON-RPC requests except `initialize` (startup handshake) and `model/list` (pagination) are **fire-and-forget**: the writer writes the request and immediately returns to process the next command. Response waiting is handled by short-lived waiter threads spawned per-request, so one slow Codex response never blocks other sessions or commands.
2. **Reader** — parses stdout JSON lines and routes events to the correct session recorder. Non-JSON lines (log output, warnings) are skipped and logged to stderr rather than treated as fatal errors, so a single malformed line does not tear down the shared runtime.
3. **Stderr** — logs diagnostic output.
4. **Waiter** — watches for child-process exit and tears down any attached sessions.

**Fire-and-forget flow for prompts:** When a session already has a thread ID, the writer sends `turn/start` directly and returns. When a new thread is needed, the writer sends `thread/start` (or `thread/resume`) as a fire-and-forget write and spawns a waiter thread. That waiter extracts the thread ID from the response and feeds a `StartTurnAfterSetup` command back through the writer's command channel, which then sends `turn/start`. The writer thread never blocks on either step.

**Lifecycle:**
1. Spawn shared process -> send `initialize` RPC -> receive capabilities (only blocking step)
2. For each session, send `thread/start` (new) or `thread/resume` (existing) -> waiter thread extracts thread ID
3. On user message, send `turn/start` with input items (text + optional image attachments)
4. Receive notifications such as `item/agentMessage/delta`, `item/completed`, and `turn/completed`
5. On approval or structured interaction, surface a TermAl message card and answer via JSON-RPC once the user responds

**Session resume:** The persisted `external_session_id` holds the Codex thread ID. Session-scoped actions such as fork, archive, compact, and rollback are issued through the shared app-server.

### Cursor

**Invocation:**
```bash
cursor-agent acp
```

**Protocol:** ACP over stdio. One process per session.

**Behavior:** Cursor emits ACP session updates for thinking, assistant text, tool calls, and config updates. TermAl maps Cursor's permission options onto the session `cursor_mode` (`agent`, `plan`, or `ask`) before deciding whether to auto-answer or show an approval card.

### Gemini

**Invocation:**
```bash
gemini --acp [--approval-mode <mode>]
```

**Protocol:** ACP over stdio. One process per session.

**Behavior:** Gemini uses the same ACP normalization layer as Cursor, but its launch command can include the configured Gemini approval mode. TermAl also performs local readiness checks so missing CLI auth or missing `gemini` installation is surfaced before a session starts.

### Message Types

All agent integrations normalize into the same TermAl message model. Some variants are common across all agents, while others are only emitted by specific backends such as Codex or ACP.

| Type | Fields | Typical source |
|------|--------|----------------|
| `Text` | text, attachments, author | User input or agent response |
| `Thinking` | title, lines | Claude or ACP thought streaming |
| `Command` | command, output, status, languages | Tool calls and shell execution |
| `Diff` | file_path, summary, diff, change_type | File edit/create tools |
| `Markdown` | title, markdown | Structured markdown output |
| `FileChanges` | title, files[] | Local workspace watcher summary for files changed during or just after an agent turn |
| `SubagentResult` | title, summary, conversation_id, turn_id | Codex subagent/task results |
| `ParallelAgents` | agents[] | Codex parallel-agent progress |
| `Approval` | title, command, detail, decision | Permission requests |
| `UserInputRequest` | title, detail, questions, state | Codex `request_user_input` |
| `McpElicitationRequest` | title, detail, request, state | Codex MCP elicitation |
| `CodexAppRequest` | title, detail, method, params, state | Generic Codex app-server requests |

---

## Frontend

### Stack

- **React 18** with hooks, transitions, and refs for performance
- **TypeScript** for type safety
- **Custom CSS** with CSS variables for theming (not Tailwind)
- **Monaco Editor** for source viewing, editing, and diff preview,
  including inline rendered regions (Mermaid / KaTeX) as view zones
- **highlight.js** for syntax highlighting in message cards
- **react-markdown** + remark-gfm for markdown rendering
- **rehype-katex** + **remark-math** for `$...$` inline and `$$...$$`
  block math
- **mermaid** for fenced ```` ```mermaid ```` flowcharts and diagrams,
  rendered inside a sandboxed iframe per diagram
- **Vite** for dev server and build
- **Vitest** for tests

Feature-level behaviour for these renderers is captured in
[`features/source-renderers.md`](./features/source-renderers.md) and
[`features/markdown-document-view.md`](./features/markdown-document-view.md).

### Component Structure

```
App.tsx (main orchestrator)
├── Sidebar
│   ├── Session list (filterable: all / working / asking / completed)
│   ├── New session button + agent picker
│   └── Settings panel (defaults, theme)
├── Workspace
│   ├── WorkspaceNode (binary tree of splits)
│   │   ├── Pane
│   │   │   ├── PaneTabs (draggable, closable)
│   │   │   ├── Active tab content:
│   │   │   │   ├── AgentSessionPanel (chat view)
│   │   │   │   ├── SourcePanel (Monaco editor)
│   │   │   │   ├── DiffPanel (Monaco diff editor)
│   │   │   │   ├── FileSystemPanel (directory browser)
│   │   │   │   └── GitStatusPanel (branch + file status)
│   │   │   └── AgentSessionPanelFooter (composer + controls)
│   │   └── Split divider (drag to resize)
│   └── ...nested splits
└── Theme switcher
```

The current workspace also includes standalone control-surface tabs
(`controlPanel`, `sessionList`, `projectList`, `orchestratorList`),
orchestrator canvases, terminal tabs, and instruction-debugger tabs. The block
above is intentionally high-level rather than an exhaustive component tree.

### Workspace System

The workspace is a **binary tree** of panes. Each node is either a leaf (pane) or a split (two children with a direction and ratio).

```typescript
WorkspaceNode = { type: "pane", paneId }
             | { type: "split", id, direction: "row" | "column", ratio, children: [node, node] }

WorkspacePane = {
  id, tabs: WorkspaceTab[], activeTabId, activeSessionId,
  viewMode: PaneViewMode, sourcePath, ...
}
```

**Tab types:** session, source, filesystem, gitStatus, terminal, controlPanel,
orchestratorList, canvas, orchestratorCanvas, sessionList, projectList,
instructionDebugger, and diffPreview. Tabs are draggable between panes and can
be split into adjacent panes by dropping on pane edges.

**View modes per pane:**
- Session modes: `session` (chat), `prompt` (input focus), `commands` (command list), `diffs` (diff list)
- Tool modes: `source`, `filesystem`, `gitStatus`, `diffPreview`

When a session becomes active in a pane, the frontend keeps the existing
scroll-to-latest behavior and autofocuses the composer so typing can begin
immediately. Session conversation pages remain mounted for live tabs where
possible, which preserves browser scroll state across ordinary tab switches.
When a pane rebuild really does remount a session view, TermAl restores the
saved offset or forces the view back to the latest response when that tab had
been pinned to the bottom.

### State Management

No external state library. State lives in `App.tsx` via `useState` and `useRef`:

- `sessions` — canonical session list from backend
- `workspace`: pane/tab layout for the active workspace ID, cached locally and persisted through `/api/workspaces/{id}`
- `codexState` — shared Codex rate-limit info
- `draftsBySessionId` — per-session message drafts (local)
- `draftAttachmentsBySessionId` — per-session image attachments (local)
- `latestStateRevisionRef` — tracks the highest revision seen

### Real-time Updates

On mount, the frontend opens an `EventSource` to `/api/events`:

1. **`state` events** — metadata-first state snapshot. Accepted only if `revision > latestRevision` (via `shouldAdoptStateRevision`), OR if the carried `serverInstanceId` differs from the last-seen id (via `isServerInstanceMismatch`) — the restart branch accepts a revision downgrade because the monotonic check is meaningless across a counter rewind.
2. **`delta` events** — incremental updates. Accepted only if `revision === latestRevision + 1` (via `decideDeltaRevisionAction`). Session-scoped deltas use the session reducer; `orchestratorsUpdated` is handled separately because it carries orchestrator state without a `sessionId`, and remote forwarding must translate the embedded server-scoped IDs before re-publishing it locally. If a gap is detected, triggers a full state resync.

Applied deltas update the specific session/message in-place via `applyDeltaToSessions()`, avoiding full reconciliation.

Session creation returns `CreateSessionResponse { sessionId, session, revision, serverInstanceId }`; the frontend adopts the concrete created session immediately and records the response revision without requiring a full state snapshot.

**Server-restart detection** is keyed off `serverInstanceId`. `latestStateRevisionRef` and `lastSeenServerInstanceIdRef` are updated in lockstep on every accepted adoption (state events, `adoptCreatedSessionResponse`, `adoptFetchedSession`). A restart produces a new UUID at `AppState::new_with_paths`; the next snapshot from the restarted server carries that new id, `isServerInstanceMismatch` fires, `shouldAdoptSnapshotRevision` returns `true` regardless of revision ordering, and the client resyncs. This closes the "prompt invisible after server restart" class of bug: without it, a stale browser tab against a freshly started server would reject every snapshot the server sent (because the revision rewound) until the user forced a refresh.

### Session Reconciliation

`reconcileSessions()` merges incoming server state with the current local state, preserving React object identity where possible. This minimizes re-renders: if a session's data hasn't changed, the same object reference is reused.

### Theming

18 selectable color themes (defined in `themes.ts`) are stored as `.css` files
in `ui/src/themes/`. Each theme defines CSS custom properties (`--ink`,
`--paper`, `--line`, background gradients, Monaco colors, etc.). The active
theme is set via `data-theme` on `<html>` and persisted to `localStorage`.

Chrome style is separate from color theme. The user can keep the theme's own
chrome or choose Terminal, Editorial, Studio, or Blueprint styling with
`data-ui-style`. Global UI font size, editor font size, and density are also
runtime preferences.

### Message Rendering

Messages are rendered as typed cards:

- **Text** — chat bubble with optional image attachment previews
- **Thinking** — collapsible reasoning block
- **Command** — `IN` / `OUT` layout with copy button, collapsible output, status indicator
- **Diff** — file path header, summary line, unified diff with syntax highlighting. Click to open in diff preview tab
- **Markdown** — rendered markdown block
- **FileChanges** — agent turn summary listing changed files from workspace watcher events
- **SubagentResult / ParallelAgents** — Codex subagent output and parallel-agent progress
- **Approval** — title, command detail, accept/reject/accept-for-session buttons
- **UserInputRequest / McpElicitationRequest / CodexAppRequest** — structured interaction requests that need an explicit user response

Long conversations (80+ messages) use **page-band virtualized rendering**. The
mounted transcript band is real DOM; only unseen pages above/below it are
represented by virtual spacers. `VirtualizedConversationMessageList.tsx`
measures whole mounted page bands, grows the mounted band from real DOM edges,
and preserves a visible-row anchor when prepending pages or applying deferred
page-height corrections. The design intentionally avoids per-message estimated
height corrections in the live scroll path.

Deferred heavy message content participates in the same scroll contract. During
active transcript scrolling or page-jump cooldowns, the virtualized stack marks
the `.message-stack` with `data-deferred-render-suspended="true"`. Deferred
heavy content must not activate while that marker is present. When the cooldown
ends, the stack removes the marker and dispatches `termal:deferred-render-resume`
so near-viewport heavy blocks can activate after scroll geometry has settled.
Assistant markdown should keep the same deferred-render component mounted when
scroll state changes; toggling between eager markdown and the deferred wrapper
can swap measured content for a placeholder and shift the virtualized scroll
height during the first `PageUp` from the bottom.

### Monaco Integration

Two Monaco components:
- `MonacoCodeEditor` — read/write source viewing with language detection
- `MonacoDiffEditor` — side-by-side diff preview (original vs modified, reconstructed from unified patch)

Workers are loaded for JSON, CSS, HTML, and TypeScript/JavaScript. Theme mapping bridges TermAl themes to Monaco's built-in dark/light themes.

---

## Session Lifecycle

```
Create session (POST /api/sessions)
  → SessionRecord created, status = Idle, preview = "Ready for a prompt."
  → commit_locked() bumps revision, persists, publishes

Send message (POST /api/sessions/{id}/messages)
  → If session is Active or Approval: queue the prompt, return Queued
  → Otherwise: start turn immediately
    → Spawn agent process if runtime is None
    → Run initialize handshake
    → Send user message to agent stdin
    → Status = Active

Streaming response
  → Agent writes events to stdout
  → Reader thread parses, calls AppState methods:
    → push_message() for new messages (text, diff, command, etc.)
    → append_text_delta() for streaming text chunks
    → update_command_message() for running command output
  → Each call bumps revision and publishes delta or full state

Approval needed
  → Agent requests permission for a tool call
  → TermAl adds Approval message, status = Approval
  → Frontend shows approval card
  → User submits decision (POST /api/sessions/{id}/approvals/{mid})
  → Decision forwarded to agent, status = Active

Turn complete
  → Agent sends result/turn_completed
  → Status = Idle
  → If queued prompts exist: dispatch next one automatically

Stop (POST /api/sessions/{id}/stop)
  → Kill active runtime process
  → Reject pending approvals
  → Status = Idle
  → Dispatch next queued prompt if any

Kill (POST /api/sessions/{id}/kill)
  → Kill runtime, remove session from list entirely
```

### Prompt Queueing

When a session is busy (Active or Approval), new messages are queued in a `VecDeque`. The frontend shows these as `PendingPrompt` entries below the composer. Users can cancel individual queued prompts. After each turn completes, `dispatch_next_queued_turn()` pops the next prompt and starts it automatically.

---

## Project Structure

```text
termal/
|-- src/
|   |-- main.rs              # process mode selection + router assembly;
|   |                        # assembles the other *.rs files via include!()
|   |                        # into a single flat crate
|   |
|   |-- # State: core types + sessions + persist + broadcast
|   |-- state.rs             # AppState, StateInner, PersistRequest/Delta, core types
|   |-- state_inner.rs       # StateInner CRUD + session-array primitives + finders
|   |-- state_accessors.rs   # snapshot / readiness cache / session-state readers
|   |-- state_boot.rs        # boot-time: discovered Codex threads + recovery + normalize
|   |-- app_boot.rs          # AppState::new_with_paths — the heavy startup wiring
|   |-- sse_broadcast.rs     # commit_locked + persist-wake + state/delta/file broadcast
|   |-- persist.rs           # SQLite schema + persist_delta_via_cache + connection cache
|   |-- persisted_state.rs   # disk-projection types (PersistedState / PersistedSessionRecord)
|   |-- paths.rs             # path resolution, canonicalization, project-scoped guards
|   |
|   |-- # Sessions + turns + messages
|   |-- session_crud.rs       # create_session, create/delete_project, update_app_settings
|   |-- session_lifecycle.rs  # kill/stop/cancel session
|   |-- session_messages.rs   # push_message / append_text_delta / upsert_command_message
|   |-- session_config.rs     # update_session_settings + refresh_session_model_options
|   |-- session_identity.rs   # message IDs + external_session_id bindings + Codex thread state
|   |-- session_sync.rs       # runtime-driven syncs (model options, agent commands, cursor mode)
|   |-- session_interaction.rs # pending-approval registers + preview-text projections
|   |-- session_runtime.rs    # runtime handle types + kill utilities
|   |-- messages.rs           # low-level SessionRecord message helpers
|   |
|   |-- # Turn engine
|   |-- turns.rs             # canonical turn runner + blocking REPL turn
|   |-- turn_dispatch.rs     # start_turn + dispatch_* queue draining
|   |-- turn_lifecycle.rs    # Idle↔Active↔Approval state machine + RuntimeToken guards
|   |-- recorders.rs         # TurnRecorder / CodexTurnRecorder ecosystem
|   |
|   |-- # Agents
|   |-- agent_readiness.rs   # CLI availability probing cache
|   |-- claude.rs            # Claude NDJSON message handling
|   |-- claude_spawn.rs      # Claude CLI subprocess spawn + wire writers
|   |-- claude_args.rs       # Claude CLI argv construction + message parsing
|   |-- claude_spares.rs     # hidden Claude spare pre-warming
|   |-- codex.rs             # Codex shared-runtime spawn + session state
|   |-- codex_home.rs        # Codex home directory setup + stderr formatters
|   |-- codex_bin.rs         # Codex executable discovery + web-search formatters
|   |-- codex_rpc.rs         # Codex JSON-RPC transport (send + wait for response)
|   |-- codex_events.rs      # inbound Codex event dispatcher
|   |-- codex_notices.rs     # shared-runtime global notice handling
|   |-- codex_text_stream.rs # agent-message delta dedup + subagent buffering
|   |-- codex_app_requests.rs # approval/user-input/MCP request + item-event handlers
|   |-- codex_turn_cleanup.rs # per-turn reset + completed-turn cleanup worker
|   |-- codex_submissions.rs # user-driven approvals / replies back into Codex
|   |-- codex_thread_actions.rs # fork/archive/unarchive/compact/rollback Codex thread
|   |-- codex_discovery.rs   # scan Codex home for pre-existing threads
|   |-- codex_validation.rs  # validation helpers for Codex payloads
|   |-- shared_codex_mgr.rs  # shared Codex app-server lifecycle + exit cascade
|   |-- repl_codex.rs        # REPL-mode Codex driver
|   |-- acp.rs               # ACP (Claude / Cursor / Gemini) protocol driver
|   |-- gemini.rs            # Gemini-specific dotenv + GEMINI_CLI_SYSTEM_SETTINGS setup
|   |
|   |-- # HTTP API
|   |-- api.rs               # thin Axum handlers + shared helpers (router is in main.rs)
|   |-- api_git.rs           # git workflow routes (status/diff/file/commit/push/sync)
|   |-- api_files.rs         # file read/write + directory list + agent command discovery
|   |-- api_sse.rs           # state SSE stream + initial-snapshot + fallback payloads
|   |-- api_review.rs        # review-document CRUD routes
|   |-- runtime.rs           # shared runtime types + Claude/Codex/ACP command enums
|   |
|   |-- # Wire (DTOs)
|   |-- wire.rs              # shared wire vocabulary: ApiError, Agent, enums, core types
|   |-- wire_messages.rs     # Message enum + interaction request DTOs + parallel-agent types
|   |-- wire_git.rs          # every Git DTO (request + response + GitDiff types)
|   |-- wire_terminal.rs     # TerminalCommand DTOs + streaming types + tuning constants
|   |-- wire_review.rs       # ReviewDocument + threaded-comment shapes
|   |-- wire_project_digest.rs # project digest DTOs + status/progress text formatters
|   |
|   |-- # Remote
|   |-- remote.rs            # SSH tunnels + HTTP transport + terminal stream bridge
|   |-- remote_ssh.rs        # SSH connection setup + validation + health checks
|   |-- remote_routes.rs     # remote HTTP plumbing (get/post/put_json) + state sync
|   |-- remote_create_proxies.rs   # create_remote_{project,session,orchestrator}_proxy
|   |-- remote_codex_proxies.rs    # fork/archive/unarchive/compact/rollback thread proxies
|   |-- remote_session_proxies.rs  # uniform "resolve-forward-sync" session action proxies
|   |-- remote_sync.rs       # ID localization + apply_remote_state + delta event fan-out
|   |-- remote_terminal.rs   # remote terminal stream proxy
|   |
|   |-- # Orchestrators
|   |-- orchestrators.rs     # template CRUD + instance creation + draft normalizers
|   |-- orchestrator_lifecycle.rs    # running-instance state machine (pause/resume/stop)
|   |-- orchestrator_transitions.rs  # per-transition engine (prompt injection, branching)
|   |
|   |-- # Misc subsystems
|   |-- instructions.rs      # instruction search graph traversal + document classification
|   |-- git.rs               # git diff loading, status parsing, worktree readers, repo sync
|   |-- terminal.rs          # terminal run/stream, process-tree lifecycle, output buffer
|   |-- review.rs            # review-document persistence + change-set-id validation
|   |-- workspace_queries.rs # workspace layout CRUD + agent command listing
|   |-- workspace_watch.rs   # workspace file watcher threads
|   |-- telegram.rs          # Telegram digest/action relay mode
|   |
|   `-- tests/               # backend regression tests, split by domain
|       |-- mod.rs           # shared fixtures: TestRecorder, HTTP test server, handle factories
|       |-- acp_gemini.rs    # ACP + Gemini runtime configuration
|       |-- agent_commands.rs, agent_readiness.rs, claude.rs, codex_discovery.rs,
|       |-- codex_protocol.rs, codex_threads.rs, cursor.rs, file_changes.rs,
|       |-- git.rs, http_routes.rs, instruction_search.rs, json_rpc.rs,
|       |-- orchestrator.rs, persist.rs, project_digest.rs, projects.rs,
|       |-- remote.rs, review.rs, runtime_rpc.rs, session_lifecycle.rs,
|       |-- session_settings.rs, session_stop.rs, session_stop_runtime.rs,
|       |-- shared_codex.rs, telegram.rs, terminal.rs, workspace.rs
|-- ui/
|   |-- src/
|   |   |-- App.tsx
|   |   |-- api.ts
|   |   |-- workspace.ts
|   |   |-- live-updates.ts
|   |   `-- panels/
|   `-- vite.config.ts
|-- docs/
|   |-- architecture.md
|   |-- vision.md
|   |-- roadmap.md
|   |-- bugs.md
|   `-- features/
|-- Cargo.toml
`-- README.md
```

The backend is still compiled as one crate-level module through `include!`, but the implementation is now split by concern instead of living entirely inside `main.rs`.

---

## Key Design Decisions

**Single-process control plane.** All local state, HTTP handlers, SSE broadcasting, and remote supervision live inside one Rust server. Agent runtimes remain child processes managed over stdin/stdout, and remote machines are bridged back into that same control plane.

**SSE over WebSocket.** Server-Sent Events are enough for TermAl's unidirectional update stream. The client sends commands through REST, while SSE handles low-latency streaming updates and reconnection.

**Revision counter over timestamps.** A monotonic `u64` makes ordering cheap and deterministic. The frontend rejects stale snapshots and forces a resync when delta revisions skip.

**Shared Codex app-server.** Codex threads already carry their own cwd and thread identity, so one shared app-server process can service many Codex sessions. That reduces process churn while keeping session state logically separate.

**Include-split backend.** The backend still shares one crate namespace but is split across many focused files (see the Project Structure listing above) assembled via `include!()` in `main.rs`. Each file owns a specific concern — agent protocol driver, HTTP route group, wire DTO cluster, state sub-area, remote proxy family — so day-to-day edits touch one or two files instead of navigating a monolith. Rust's multiple-impl-blocks rule lets `AppState` / `StateInner` method clusters live in whichever file matches their domain, and the flat namespace means types and helpers are visible across every file without any `pub use` boilerplate.

**Agent-agnostic UI message model.** Claude, Codex, Cursor, and Gemini are normalized into the same `Message` enum. Adding a new agent is mostly a runtime and normalization task rather than a frontend rewrite.

**Custom CSS over Tailwind.** The frontend uses CSS custom properties and standalone theme files for theming, keeping runtime theme switching simple and avoiding build-time CSS machinery.
