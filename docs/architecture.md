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
**Persistence:** `~/.termal/sessions.json` stores sessions, projects, preferences, remote config, workspace layouts, and orchestrator instances. `~/.termal/orchestrators.json` stores orchestrator templates.
**Real-time:** Server-Sent Events with a monotonic revision counter for ordering.

**Current status:** The current implementation uses server-backed workspace layouts with per-workspace local cache warm starts.

**Remote direction:** The long-term remote model keeps the browser on a single
local TermAl server. That local server stores preferences, manages remote
connections, and routes project work to local or remote TermAl servers over
SSH-managed tunnels.

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
    persistence_path: Arc<PathBuf>,            // ~/.termal/sessions.json
    orchestrator_templates_path: Arc<PathBuf>, // ~/.termal/orchestrators.json
    state_events: broadcast::Sender<String>,
    delta_events: broadcast::Sender<String>,
    shared_codex_runtime: Arc<Mutex<Option<SharedCodexRuntime>>>,
    remote_registry: Arc<RemoteRegistry>,
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
  → persist_state(path, inner)        // write ~/.termal/sessions.json
  → publish_state_locked(inner)       // broadcast full StateResponse on SSE
  → Ok(revision)
```

Streaming paths (`append_text_delta`, `update_command_message`) bump revision and publish a `DeltaEvent` instead of a full snapshot, avoiding the cost of serializing all sessions on every token.

Internal bookkeeping that the frontend doesn't need (e.g. recording Codex sandbox mode after runtime config) uses `persist_state()` directly without bumping revision.

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
| GET | `/api/state` | Full state snapshot |
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

`GET /api/health` currently returns `{ ok: true, supportsInlineOrchestratorTemplates: true }`. Remote launchers use `supportsInlineOrchestratorTemplates` during health probes to decide whether a remote can accept inline local orchestrator templates or must be upgraded first.

### SSE Event Stream

`GET /api/events` returns a Server-Sent Events stream with three event types:

- **`state`** — full `StateResponse` JSON. Sent on initial connect, after `commit_locked()`, and as a recovery when the client falls behind.
- **`delta`** ? incremental `DeltaEvent` JSON. Sent during streaming (text deltas, text replacements, command output updates). Cheaper than full state.
- **`workspaceFilesChanged`** - coalesced local workspace file watcher hints. Sent outside the main state revision stream with its own monotonically increasing file-event revision so source, diff, file tree, and git-preview panels can refresh only when touched paths match their scope.

All three carry a `revision: u64` field. `state` and `delta` share the main state revision counter, which the frontend uses to reject stale snapshots and detect gaps in the delta sequence. `workspaceFilesChanged` uses a separate file-event revision counter; the frontend batches same-tick file events and ignores file-event revisions strictly older than the last seen revision (same-revision events are merged while buffered).

```
DeltaEvent::TextDelta            { revision, session_id, message_id, delta, preview }
DeltaEvent::TextReplace          { revision, session_id, message_id, message_index, text, preview }
DeltaEvent::CommandUpdate        { revision, session_id, message_id, command, output, status, preview, ... }
DeltaEvent::ParallelAgentsUpdate { revision, session_id, message_id, message_index, agents, preview }
DeltaEvent::OrchestratorsUpdated { revision, orchestrators[] } // IDs inside each instance are scoped to the originating server; translate via sync_remote_state_inner before forwarding remotely.
```

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
└── sessions.json    # PersistedState (JSON)
```

`PersistedState` is a projection of `StateInner` that excludes runtime handles, pending approval maps, and empty collections. It stores the revision counter, session configs, message history, Codex state, and persisted workspace layout documents keyed by workspace ID. On startup, the backend loads this file and reconstructs `StateInner`.

---

## Remote Architecture Direction

The chosen remote architecture is:

`Browser -> local TermAl server -> remote TermAl server`

The browser should not manage multiple backend origins directly. Instead, the
local server remains the control plane and exposes the single browser-facing
`/api` and `/api/events` interface.

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
               │ local execution               │ SSH bootstrap + persistent tunnel
               ▼                               ▼
┌──────────────────────────────┐   ┌──────────────────────────────────────────┐
│ Local machine runtime        │   │ Remote machine                           │
│ LocalConnector               │   │ sshd                                     │
│ - local projects             │   │  └─ runs bootstrap script, then          │
│ - local agent processes      │   │     starts or reuses `termal server`     │
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

1. The local TermAl server uses SSH to connect to the remote host.
2. For managed SSH remotes, SSH can run a bootstrap script in a configured
   TermAl checkout on the remote machine.
3. The bootstrap script updates that checkout (`git fetch`, `git checkout`,
   `git pull --ff-only`) and starts the backend from source.
4. The first useful version can use `cargo run -- server` for convenience,
   though `cargo build --release && ./target/release/termal server` is the
   better long-lived default.
5. The remote TermAl server listens on `127.0.0.1` only on the remote machine.
6. The local TermAl server keeps a persistent SSH tunnel to that remote server.
7. The local TermAl server speaks the normal TermAl HTTP and SSE protocol over
   that tunnel.

This is intentionally similar to the Remote-SSH shape used by editor tooling:
SSH is used to reach the machine, start the remote server, and carry the
transport. The browser still only talks to the local control plane.
For the first managed iteration, the control plane updates source on the remote
instead of copying prebuilt binaries. That avoids cross-compilation while still
letting one laptop manage multiple machines.

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
- File, directory, git, review, and session creation flows should route by
  project ownership.

This avoids teaching the UI to choose a backend for every action. The user
chooses a remote when creating a project, and the rest of the routing follows
from that association.

### Session and Project Identity

Remote-native ids cannot be trusted to be globally unique across multiple
machines. The local control plane must therefore expose collision-safe browser
ids for projects and sessions.

Examples:

- `local::project-1`
- `build-box::project-1`
- `local::session-3`
- `build-box::session-3`

Whether these are literal exposed ids or stable local aliases is an
implementation detail, but the browser should only deal with globally unique
identifiers.

### State and Event Aggregation

The browser should continue to consume one state stream from the local control
plane.

That means the local server must:

- fetch or subscribe to state from each configured remote
- merge those states into one browser-facing `StateResponse`
- rewrite project and session ids into browser-safe ids
- emit one aggregate SSE stream
- use its own aggregate revision counter instead of forwarding raw remote
  revisions directly

The frontend should not need to know whether a project is local or remote in
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

### V0 Managed Bootstrap

The first managed-remote bootstrap should optimize for developer-controlled
machines rather than general-purpose machine provisioning.

Assumptions:

- The remote machine already has `git`, `cargo`, and the Rust toolchain
  installed.
- The remote machine already has a local TermAl checkout at a configured path.
- The local machine can push commits before asking a remote to update.

Recommended flow:

1. SSH into the remote machine.
2. `cd` to the configured TermAl checkout.
3. Fast-forward that checkout to the desired branch, tag, or commit.
4. Start `termal server` from source in that checkout.
5. Reuse the existing tunnel, health check, and SSE bridge design.

This keeps the remote-control-plane design intact while deferring binary
distribution, artifact caching, checksums, and cross-compilation support to a
later iteration.

### API Shape

The remote TermAl server should expose the same HTTP and SSE protocol shape as a
local TermAl server as much as possible.

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

The UI should evolve toward:

- one browser connection to the local TermAl server
- a Settings surface for remote configuration
- project creation that requires selecting a remote
- session creation that requires selecting a project
- remote-aware project/session status display

The UI should not evolve toward:

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
- **Monaco Editor** for source viewing, editing, and diff preview
- **highlight.js** for syntax highlighting in message cards
- **react-markdown** + remark-gfm for markdown rendering
- **Vite** for dev server and build
- **Vitest** for tests

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

**Tab types:** session, source, filesystem, gitStatus, diffPreview. Tabs are draggable between panes.

**View modes per pane:**
- Session modes: `session` (chat), `prompt` (input focus), `commands` (command list), `diffs` (diff list)
- Tool modes: `source`, `filesystem`, `gitStatus`, `diffPreview`

When a session becomes active in a pane, the frontend keeps the existing
scroll-to-latest behavior and also autofocuses the composer so typing can begin
immediately.

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

1. **`state` events** — full state snapshot. Accepted only if `revision > latestRevision` (via `shouldAdoptStateRevision`).
2. **`delta` events** — incremental updates. Accepted only if `revision === latestRevision + 1` (via `decideDeltaRevisionAction`). Session-scoped deltas use the session reducer; `orchestratorsUpdated` is handled separately because it carries orchestrator state without a `sessionId`, and remote forwarding must translate the embedded server-scoped IDs before re-publishing it locally. If a gap is detected, triggers a full state resync.

Applied deltas update the specific session/message in-place via `applyDeltaToSessions()`, avoiding full reconciliation.

Session creation returns `CreateSessionResponse { sessionId, state }` — the full state is embedded in the response, eliminating a separate fetch round-trip.

### Session Reconciliation

`reconcileSessions()` merges incoming server state with the current local state, preserving React object identity where possible. This minimizes re-renders: if a session's data hasn't changed, the same object reference is reused.

### Theming

16 selectable color themes (defined in `themes.ts`) are stored as `.css` files in `ui/src/themes/`. Each theme defines CSS custom properties (`--ink`, `--paper`, `--line`, background gradients, etc.). The active theme is set via `data-theme` attribute on `<html>` and persisted to `localStorage`.

### Message Rendering

Messages are rendered as typed cards:

- **Text** — chat bubble with optional image attachment previews
- **Thinking** — collapsible reasoning block
- **Command** — `IN` / `OUT` layout with copy button, collapsible output, status indicator
- **Diff** — file path header, summary line, unified diff with syntax highlighting. Click to open in diff preview tab
- **Markdown** — rendered markdown block
- **Approval** — title, command detail, accept/reject/accept-for-session buttons

Long conversations (80+ messages) use **windowed rendering** — only messages near the viewport are mounted.

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
|   |-- main.rs              # process mode selection + router assembly
|   |-- api.rs               # axum handlers and transport glue
|   |-- state.rs             # AppState, sessions, persistence, shared state
|   |-- runtime.rs           # Claude/Codex/ACP process management
|   |-- turns.rs             # recorder pipeline and blocking REPL turns
|   |-- remote.rs            # SSH tunnels, remote proxying, SSE bridge
|   |-- orchestrators.rs     # template CRUD and runtime instance engine
|   |-- telegram.rs          # Telegram digest/action relay mode
|   `-- tests.rs             # backend regression tests
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

**Include-split backend.** The backend still shares one crate namespace, but responsibility is separated into `api.rs`, `state.rs`, `runtime.rs`, `turns.rs`, `remote.rs`, `orchestrators.rs`, and `telegram.rs`. That keeps cross-cutting types easy to share without claiming the backend is still one giant `main.rs`.

**Agent-agnostic UI message model.** Claude, Codex, Cursor, and Gemini are normalized into the same `Message` enum. Adding a new agent is mostly a runtime and normalization task rather than a frontend rewrite.

**Custom CSS over Tailwind.** The frontend uses CSS custom properties and standalone theme files for theming, keeping runtime theme switching simple and avoiding build-time CSS machinery.
