# TermAl — Architecture

> A WhatsApp-style interface for controlling AI coding agents running on your machine.

---

## System Overview

```
Browser (React)                      Rust Backend (axum)
┌──────────────────────┐             ┌──────────────────────────────────┐
│  App.tsx             │  SSE /api   │  AppState                        │
│  ├── Sidebar         │◄═══════════╡  ├── StateInner (Mutex)          │
│  ├── Workspace       │  events +   │  │   ├── sessions: Vec<Record>  │
│  │   ├── Pane[]      │  deltas     │  │   ├── revision: u64          │
│  │   └── Tabs[]      │             │  │   └── codex: CodexState      │
│  └── Composer        │             │  ├── state_events (broadcast)    │
│                      │  REST /api  │  ├── delta_events (broadcast)    │
│  api.ts              │────────────>│  └── persistence_path            │
└──────────────────────┘             │                                  │
                                     │  Agent Runtimes                  │
                                     │  ├── Claude (child process)      │
                                     │  │   NDJSON over stdin/stdout    │
                                     │  └── Codex (child process)       │
                                     │      JSON-RPC over stdin/stdout  │
                                     └──────────────────────────────────┘
```

**Frontend:** React 18 + TypeScript, served on `:4173` (dev) with Vite proxy to backend.
**Backend:** Rust + axum + tokio, runs on `:8787`. Spawns AI agents as child processes.
**Persistence:** Single JSON file at `~/.termal/sessions.json`.
**Real-time:** Server-Sent Events with monotonic revision counter for ordering.

---

## Backend

### Entry Points

The binary has two modes:

1. **Server mode** (default) — starts an axum HTTP server on `0.0.0.0:8787`, serves the API, manages long-lived agent processes.
2. **REPL mode** (`--repl`) — interactive terminal loop. Reads prompts from stdin, runs one turn at a time via `run_turn_blocking()`. Mostly used for testing.

### Core State

```rust
AppState {
    inner: Arc<Mutex<StateInner>>,      // all mutable state
    state_events: broadcast::Sender,     // full-state SSE channel (cap 128)
    delta_events: broadcast::Sender,     // incremental SSE channel (cap 256)
    persistence_path: Arc<PathBuf>,      // ~/.termal/sessions.json
    default_workdir: String,
}

StateInner {
    revision: u64,                       // monotonic, bumped on visible changes
    sessions: Vec<SessionRecord>,        // all sessions with runtime handles
    codex: CodexState,                   // shared Codex rate-limit info
    next_session_number: usize,
    next_message_number: u64,
}
```

**SessionRecord** wraps the serializable `Session` with runtime-only fields:

```rust
SessionRecord {
    session: Session,                          // id, name, agent, status, messages, etc.
    runtime: SessionRuntime,                   // None | Claude(handle) | Codex(handle)
    pending_claude_approvals: HashMap,         // request_id → ClaudePendingApproval
    pending_codex_approvals: HashMap,          // message_id → CodexPendingApproval
    queued_prompts: VecDeque<QueuedPromptRecord>,
    external_session_id: Option<String>,       // Codex thread ID or Claude session ID
    codex_approval_policy: CodexApprovalPolicy,
    codex_sandbox_mode: CodexSandboxMode,
    active_codex_approval_policy: Option<...>, // what the running process actually uses
    active_codex_sandbox_mode: Option<...>,
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

All routes are under `/api`. The backend serves JSON; the frontend proxies through Vite in dev.

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/api/health` | Health check |
| GET | `/api/state` | Full state snapshot |
| GET | `/api/events` | SSE stream (state + delta events) |
| POST | `/api/sessions` | Create session → `CreateSessionResponse` |
| POST | `/api/sessions/{id}/settings` | Update session config → `StateResponse` |
| POST | `/api/sessions/{id}/messages` | Send message → `StateResponse` (202) |
| POST | `/api/sessions/{id}/queued-prompts/{pid}/cancel` | Cancel queued prompt |
| POST | `/api/sessions/{id}/stop` | Stop active turn |
| POST | `/api/sessions/{id}/kill` | Kill and remove session |
| POST | `/api/sessions/{id}/approvals/{mid}` | Submit approval decision |
| GET | `/api/file?path=...` | Read file content |
| PUT | `/api/file` | Write file content |
| GET | `/api/fs?path=...` | List directory entries |
| GET | `/api/git/status?path=...` | Git status + branch info |

### SSE Event Stream

`GET /api/events` returns a Server-Sent Events stream with two event types:

- **`state`** — full `StateResponse` JSON. Sent on initial connect, after `commit_locked()`, and as a recovery when the client falls behind.
- **`delta`** — incremental `DeltaEvent` JSON. Sent during streaming (text deltas, command output updates). Cheaper than full state.

Both carry a `revision: u64` field. The frontend uses this to reject stale snapshots and detect gaps in the delta sequence.

```
DeltaEvent::TextDelta    { revision, session_id, message_id, delta, preview }
DeltaEvent::CommandUpdate { revision, session_id, message_id, command, output, status, preview, ... }
```

On broadcast channel lag, the backend falls back to sending a full state snapshot.

### Persistence

```
~/.termal/
└── sessions.json    # PersistedState (JSON)
```

`PersistedState` is a projection of `StateInner` that excludes runtime handles, pending approval maps, and empty collections. It stores the revision counter, session configs, message history, and Codex state. On startup, the backend loads this file and reconstructs `StateInner`.

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

**Protocol:** JSON-RPC 2.0 over stdio. One app-server process per session (currently; planned: one shared process for all Codex sessions).

**Thread architecture:** Same 4-thread pattern as Claude (writer, reader, stderr, waiter).

**Lifecycle:**
1. Spawn process → send `initialize` RPC → receive capabilities
2. Send `thread/start` (new) or `thread/resume` (existing) → receive thread ID
3. On user message → send `turn/start` RPC with input items (text + optional image attachments)
4. Receive notifications: `item/agentMessage/delta` (streaming text), `item/started`/`item/completed` (tool results), `turn/completed`
5. On approval needed → Codex sends `item/commandExecution/requestApproval` or `item/fileChange/requestApproval` → TermAl shows approval card → responds with accept/decline/acceptForSession

**Session resume:** Store the Codex thread ID as `external_session_id`. On next spawn, send `thread/resume` instead of `thread/start`.

### Message Types

Both agents produce the same set of TermAl message types:

| Type | Fields | Source |
|------|--------|--------|
| `Text` | text, attachments, author | User input or agent response |
| `Thinking` | title, lines | Claude extended thinking blocks |
| `Command` | command, output, status (running/success/error), languages | Bash/shell tool calls |
| `Diff` | file_path, summary, diff (unified patch), change_type (edit/create) | File edit/create tool calls |
| `Markdown` | title, markdown | Structured markdown output |
| `Approval` | title, command, detail, decision | Permission requests |

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

### State Management

No external state library. State lives in `App.tsx` via `useState` and `useRef`:

- `sessions` — canonical session list from backend
- `workspace` — pane/tab layout (local, not persisted to backend)
- `codexState` — shared Codex rate-limit info
- `draftsBySessionId` — per-session message drafts (local)
- `draftAttachmentsBySessionId` — per-session image attachments (local)
- `latestStateRevisionRef` — tracks the highest revision seen

### Real-time Updates

On mount, the frontend opens an `EventSource` to `/api/events`:

1. **`state` events** — full state snapshot. Accepted only if `revision > latestRevision` (via `shouldAdoptStateRevision`).
2. **`delta` events** — incremental updates. Accepted only if `revision === latestRevision + 1` (via `decideDeltaRevisionAction`). If a gap is detected, triggers a full state resync.

Applied deltas update the specific session/message in-place via `applyDeltaToSessions()`, avoiding full reconciliation.

Session creation returns `CreateSessionResponse { sessionId, state }` — the full state is embedded in the response, eliminating a separate fetch round-trip.

### Session Reconciliation

`reconcileSessions()` merges incoming server state with the current local state, preserving React object identity where possible. This minimizes re-renders: if a session's data hasn't changed, the same object reference is reused.

### Theming

17 CSS themes stored as individual `.css` files in `ui/src/themes/`. Each theme defines CSS custom properties (`--ink`, `--paper`, `--line`, background gradients, etc.). The active theme is set via `data-theme` attribute on `<html>` and persisted to `localStorage`.

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

```
termal/
├── src/
│   └── main.rs                    # Entire backend (~7600 lines)
├── ui/
│   ├── src/
│   │   ├── App.tsx                # Main component (~4500 lines)
│   │   ├── api.ts                 # API client
│   │   ├── types.ts               # Shared TypeScript types
│   │   ├── workspace.ts           # Pane/tab/split state management
│   │   ├── live-updates.ts        # Delta event application
│   │   ├── state-revision.ts      # Revision ordering logic
│   │   ├── session-reconcile.ts   # State reconciliation
│   │   ├── session-list-filter.ts # Sidebar filtering
│   │   ├── highlight.ts           # Syntax highlighting
│   │   ├── diff-preview.ts        # Unified diff parsing
│   │   ├── monaco.ts              # Monaco editor setup
│   │   ├── pane-keyboard.ts       # Keyboard navigation
│   │   ├── themes.ts              # Theme management
│   │   ├── styles.css             # Global styles + CSS variables
│   │   ├── MonacoCodeEditor.tsx   # Source editor component
│   │   ├── MonacoDiffEditor.tsx   # Diff viewer component
│   │   ├── panels/
│   │   │   ├── AgentSessionPanel.tsx       # Chat message thread
│   │   │   ├── AgentSessionPanelFooter.tsx # Composer + controls  (in App.tsx)
│   │   │   ├── PaneTabs.tsx               # Tab bar with drag-drop
│   │   │   ├── SourcePanel.tsx            # File editor panel
│   │   │   ├── DiffPanel.tsx              # Diff preview panel
│   │   │   ├── FileSystemPanel.tsx        # Directory browser
│   │   │   └── GitStatusPanel.tsx         # Git status panel
│   │   ├── themes/                # 17 CSS theme files
│   │   └── *.test.ts              # Tests
│   ├── package.json
│   ├── vite.config.ts             # Dev proxy: /api → :8787
│   └── tsconfig.json
├── docs/
│   ├── architecture.md            # This file
│   ├── bugs.md                    # Bug tracker + implementation tasks
│   ├── claude-pair-spec.md        # Original product spec (partially stale)
│   └── features/                  # Feature briefs
├── Cargo.toml
└── Cargo.lock
```

---

## Key Design Decisions

**Single-process backend.** All sessions share one Rust process. Agent runtimes are child processes managed via stdin/stdout. No microservices, no message broker. Simple to deploy, simple to debug.

**SSE over WebSocket.** Server-Sent Events are simpler than WebSocket for a unidirectional update stream. The client only sends data via REST calls. SSE handles reconnection automatically.

**Revision counter over timestamps.** A monotonic `u64` is cheaper to compare than timestamps and immune to clock skew. The frontend rejects any state with `revision <= current` and requests a resync if a delta's revision is non-contiguous.

**Delta events for streaming.** During active generation, text arrives token-by-token. Publishing a full state snapshot (all sessions, all messages) per token is expensive. Delta events carry only the changed field, and the frontend patches it into the local state.

**One file per layer.** `main.rs` is the entire backend; `App.tsx` is the main frontend component. This is a known tech-debt tradeoff — iteration speed over modularity while the architecture is still changing. The module boundaries are clear and documented in bugs.md for when the split happens.

**Agent-agnostic message model.** Both Claude and Codex produce the same `Message` variants (Text, Command, Diff, Approval, etc.). The frontend doesn't know which agent produced a message — it just renders the type. This makes adding new agents (Gemini CLI is next) a backend-only change for basic support.

**Custom CSS over Tailwind.** The app uses CSS custom properties for theming with 17 hand-crafted themes. Each theme is a standalone `.css` file that sets color variables. No build-time CSS processing needed.
