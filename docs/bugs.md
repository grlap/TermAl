# Bugs & Known Issues

Updated against the current checked-in code in `src/main.rs` and `ui/src/App.tsx`.

The older entries for "No image paste support", Claude `control_request` fallthrough, "No SSE/WebSocket for real-time updates", and "Codex receive has no streaming" were stale. Those are implemented in the current tree.

## Image attachment UX is inconsistent

**Severity:** Low — the transport path works, but the product copy and interaction model lag behind it.

The web UI captures pasted image files, converts them to base64 draft attachments, shows previews,
and sends them with `POST /api/sessions/{id}/messages`. The backend validates the payload and now
forwards attachments to both Claude and Codex sessions.

**Current behavior:**
- Paste support exists in the composer for any active session
- Claude prompts encode attachments as image blocks
- Codex prompts encode attachments as `image` input items with data URLs
- Supported formats are PNG, JPEG, GIF, and WebP
- A 5 MB limit is enforced in both the frontend and backend
- Drag-and-drop attachments are still not implemented

**Tasks:**
- Add a regression test for Codex attachment submission so the docs do not drift again
- Implement drag-and-drop attachment support, or explicitly document paste-only behavior in the UI

## Polling for process exit

**Severity:** Low

Both `spawn_claude_runtime()` and `spawn_codex_runtime()` still use a `sleep(100ms)` loop around `child.try_wait()` to detect process exit. This is functional, but it is still polling.

**Affected code (`src/main.rs`):**
- Claude wait thread in `spawn_claude_runtime()`
- Codex wait thread in `spawn_codex_runtime()`

**Fix:** Replace the polling loop with a dedicated waiter thread that blocks on `child.wait()`, or move runtime supervision to async child handling.

## Codex home & data directory resolution breaks on Windows

**Severity:** Medium — blocks Windows support.

`resolve_source_codex_home_dir()` and `resolve_termal_data_dir()` still rely on `$HOME`. On Windows, `%USERPROFILE%` is the common fallback.

**Affected code (`src/main.rs`):**
- `resolve_source_codex_home_dir()` falls back to `$HOME/.codex` when `CODEX_HOME` is not set
- `resolve_termal_data_dir()` resolves `$HOME/.termal`

**Fix:** Add a `USERPROFILE` fallback after `HOME`:
```rust
let home = std::env::var_os("HOME")
    .or_else(|| std::env::var_os("USERPROFILE"))
    .ok_or_else(|| anyhow!("neither HOME nor USERPROFILE is set"))?;
```

Apply the same fallback to `resolve_termal_data_dir()`. Using `%APPDATA%` would be more Windows-native, but `USERPROFILE` is enough to remove the hard failure.

## No runtime preload / pre-warming

**Severity:** Medium — first message still pays startup cost.

Runtime processes are still created lazily inside `dispatch_turn()`. When a session has `SessionRuntime::None`, the first message spawns `spawn_claude_runtime()` or `spawn_codex_runtime()`.

**Current impact:**
- No early auth/config sanity check on app startup
- The first prompt in a session pays process startup plus initialize-handshake latency
- Selecting a session in the UI does not preload its runtime

**Possible improvement:**
1. Probe runtime availability and basic config on app start
2. Warm the session runtime when the user opens or focuses a session

## Legacy/testing Codex REPL path still uses one-shot `codex exec --json`

**Severity:** Low — server mode is fixed, but the old testing path is not.

The server path now uses persistent `codex app-server` JSON-RPC with streaming `item/agentMessage/delta` events. However, `run_turn_blocking()` still routes the Codex REPL/testing mode through `run_codex_turn()`, which shells out to `codex exec --json` or `codex exec resume --json`.

**Impact:**
- REPL mode does not share the persistent app-server runtime
- REPL mode does not share the server approval flow
- Legacy `handle_codex_event()` and rollout-fallback code still have to be maintained

## Unhandled Codex rate limit notifications

**Severity:** Low — harmless, but noisy.

The Codex app-server emits `account/rateLimits/updated` notifications with account-level usage
data such as `planType`, `primary.usedPercent`, `primary.resetsAt`, `secondary.usedPercent`, and
`secondary.resetsAt`.

TermAl does not currently handle that notification in
`handle_codex_app_server_notification()`, so it falls through to
`log_unhandled_codex_event()` and prints repeated diagnostics like:
`codex diagnostic> unhandled Codex app-server notification 'account/rateLimits/updated': {...}`

**Impact:**
- harmless protocol noise shows up in logs during normal Codex usage
- real unhandled protocol issues are harder to distinguish from expected background events
- useful rate-limit information is dropped instead of being surfaced anywhere in the app

**Fix:**
- minimum: add `account/rateLimits/updated` to the known-notification ignore list
- preferred: parse and persist the rate-limit payload, expose it through the backend state API,
  and surface it in the UI

**Test coverage:** Add a unit test that verifies `account/rateLimits/updated` no longer reaches
the unhandled-event logger.

---

# Backlog

## Single-file codebase

**Severity:** Medium — maintainability concern, not a runtime bug.

`src/main.rs` is now ~5,700 lines and still contains routes, persistence, session state, Claude integration, Codex integration, REPL code, and tests. The logical boundaries are clear, but the file is past the point where changes are easy to isolate or review.

## Codex app-server integration is partial

**Severity:** Medium — server-mode basics are in place, but protocol coverage is still incomplete.

Server mode already uses `codex app-server` over stdio JSON-RPC with:
- `initialize`
- `thread/start` and `thread/resume`
- `turn/start`
- `item/agentMessage/delta`
- approval handling for `item/commandExecution/requestApproval` and `item/fileChange/requestApproval`

**Still missing:**
- REPL migration off the legacy `codex exec --json` path
- UI actions for fork, rollback, archive, unarchive, and compaction
- handling for additional app-server request types beyond command/file approvals
- mapping for more notifications beyond the current subset

## No Gemini CLI integration

**Severity:** Medium — missing a major agent.

Only Claude and Codex are wired through session creation, UI selection, runtime spawning, and message dispatch. There is no Gemini runtime adapter, no Gemini session option in the UI, and no backend path for streaming Gemini events.

The likely integration path is still to spawn `gemini` in non-interactive mode with `--output-format stream-json`, similar to the Claude adapter.

## No test coverage for Codex app-server parsing or HTTP endpoints

**Severity:** Medium

There are unit tests for Claude parsing and for the older `handle_codex_event()` parsing path, but there are no tests for the newer Codex app-server message handling (`handle_codex_app_server_message()` and related helpers), and there are no HTTP route tests for the axum handlers.

## Codex session discovery is reinvented

**Severity:** Low — opportunity, not a runtime bug.

Codex already maintains thread state in `~/.codex/state.db`. TermAl still persists its own session list instead of querying Codex's own thread metadata.

If thread discovery eventually moves to Codex's database, TermAl could reuse existing metadata instead of maintaining a parallel index.

## No queueing system for prompts

**Severity:** Medium — blocks fluid multi-tasking workflows.

While an agent is running a turn, the user cannot submit another prompt. The composer is effectively
locked until the current turn completes.

**Desired behavior:**
- Allow the user to type and submit a follow-up prompt while an agent turn is in progress
- Queue the pending prompt and display it visually in the conversation (e.g. as a "pending" bubble)
- Allow the user to cancel a pending prompt before it is dispatched
- When the active turn finishes, automatically dispatch the next queued prompt

**Tasks:**
- Add a prompt queue to the backend session state (per-session FIFO)
- Update `dispatch_turn()` to drain the queue after each turn completes
- Add a UI indicator for queued/pending prompts in the conversation view
- Add a cancel action on pending prompts so the user can remove them before dispatch
- Ensure canceling the active turn does not silently discard queued prompts

## Spec drift

**Severity:** Low — documentation only.

The spec in `docs/claude-pair-spec.md` still describes a Tauri app with IPC commands, while the implementation is an axum server plus a React frontend. It also understates the current Codex app-server integration and still omits Gemini entirely.

---

# Implementation Tasks

Concrete work implied by the current TermAl parity gaps. Ordered by user impact and dependency.

## P0

- [ ] Add Gemini as a first-class agent in the backend and UI:
  `Agent` enum, session creation, session rendering, and persistence need to stop assuming the
  world is only Claude or Codex.
- [ ] Implement a persistent Gemini runtime adapter:
  spawn `gemini` with `--output-format stream-json`, map stdout events into TermAl messages, and
  wire message dispatch through the same session runtime path used by Claude and Codex.
- [ ] Expose Codex thread actions in the product:
  add backend routes and UI actions for `thread/fork`, `thread/rollback`, `thread/archive`,
  `thread/unarchive`, and `thread/compact/start`.
- [ ] Expand Codex app-server request handling beyond command/file approvals:
  TermAl should not silently fall back to "unhandled request" logging for additional interactive
  request types.

## P1

- [ ] Implement a prompt queueing system:
  allow the user to submit follow-up prompts while an agent turn is running, display pending
  prompts in the conversation, and let the user cancel them before dispatch.
- [ ] Migrate REPL mode off legacy `codex exec --json` and onto the app-server path so server mode
  and REPL mode share one implementation.
- [ ] Replace the `try_wait()` polling loops in the Claude and Codex runtime supervisors with
  blocking wait threads or async child handling.
- [ ] Add runtime warm-up:
  preflight auth/config on startup and preload runtimes when a session is opened or focused.
- [ ] Fix Windows path resolution:
  add `USERPROFILE` fallback for Codex home and TermAl data directory resolution.
- [ ] Align attachment UX with actual capabilities:
  show the right composer hint per agent, add drag-and-drop, and keep the docs in sync with the
  implementation.
- [ ] Add post-edit diff preview from agent messages:
  when an agent reports that it updated a file, let the user open a new tab with a diff preview of
  those changes and include a link back to the originating conversation or message.
- [ ] Add saved review comments on diff previews:
  let the user leave PR-style comments on files or hunks, persist them to disk in a structured
  format, and make them available for later agent turns.

## P2

- [ ] Handle Codex `account/rateLimits/updated` explicitly:
  at minimum ignore it as known noise; preferably persist it and expose it in the UI.
- [ ] Add unit tests for Codex app-server parsing:
  cover request handling, streaming message assembly, notification filtering, and error paths.
- [ ] Add HTTP route tests for the axum API:
  session creation, message send, settings updates, approvals, kill, and SSE state events.
- [ ] Refresh `docs/claude-pair-spec.md` so the architecture and milestone tracking match the
  current axum + React implementation.
- [ ] Split `src/main.rs` into focused modules once the feature work above stops churning large
  integration surfaces.

## Later

- [ ] Replace TermAl's parallel Codex session index with discovery from Codex's own thread
  metadata when that becomes worth the complexity.

# Implementation Plan: Backlinks, Diff Preview, and Review Comments

This is the concrete delivery plan for the diff-preview and saved-review workflow.

## Goals

- Let the user open a structured diff preview in a new tab directly from an agent update.
- Let the preview link back to the exact conversation session and message that produced the change.
- Let the user add PR-style review comments and save them to disk.
- Let a later agent turn find the saved review file and act on unresolved comments.

## Non-goals for v1

- No browser URL routing or shareable deep links yet.
- No multi-user review system.
- No git-hosted PR sync.
- No dependency on an external diff viewer library in the first pass.

## Current constraints

- The frontend has no router today; navigation is entirely workspace-state driven.
- Workspace tabs are effectively session tabs today, with source view as a pane mode rather than a
  first-class tab entity.
- `DiffMessage` only carries `filePath`, `summary`, `diff`, and `changeType`.
- Backend state updates are already pushed through `/api/events` SSE snapshots, which is sufficient
  for this feature.
- The UI has no diff rendering dependency today, so phase 1 should use a small internal unified
  diff parser and renderer.

## Proposed architecture

### 1. Link target system

Introduce a typed in-app link system so navigation is explicit and reusable.

```ts
type LinkTarget =
  | { kind: "session"; sessionId: string }
  | { kind: "message"; sessionId: string; messageId: string }
  | { kind: "source"; path: string }
  | { kind: "diffPreview"; changeSetId: string; originSessionId: string; originMessageId: string };
```

Rules:
- All in-app navigation goes through one `openLink(target, options)` helper.
- The helper decides whether to focus an existing tab or open a new one.
- The first version only needs in-memory app navigation, not browser history.

### 2. Generic workspace tabs

Refactor the workspace model so a pane can hold first-class tabs instead of only session IDs.

```ts
type WorkspaceTab =
  | { id: string; kind: "session"; sessionId: string }
  | { id: string; kind: "source"; path: string }
  | {
      id: string;
      kind: "diffPreview";
      changeSetId: string;
      originSessionId: string;
      originMessageId: string;
    };
```

Rules:
- Deduping for diff preview tabs should key off `changeSetId`.
- A pane can still default to opening session tabs the same way it does today.
- Source view should migrate into the same tab model so navigation remains consistent.

### 3. Change-set identity

Every diff preview needs a stable ID that survives reopening and review-file lookup.

Proposal:
- Add `changeSetId` to `DiffMessage`.
- For v1, generate one change set per diff message.
- When the backend later groups multiple file diffs from one response, multiple `DiffMessage`
  entries can share the same `changeSetId`.

Minimum backend metadata to add to diff-like messages:
- `originSessionId`
- `originMessageId`
- `changeSetId`
- optional `turnId` if a turn-level grouping ID becomes useful later

## Diff preview plan

### Rendering model

Phase 1:
- Parse unified diff text in the frontend into `files -> hunks -> lines`.
- Render a structured viewer with:
  file header
  change type
  hunk header
  old/new line numbers
  added/removed/context styling
- Keep a "Raw patch" toggle for debugging and fallback.

Phase 2:
- Support grouped previews for all file diffs in the same response.
- Add better context collapsing and optional split view if needed.

### Why not add a library first

The repo currently has no diff-viewer dependency and no routing layer. The first implementation
should minimize moving pieces:
- build a small parser for the subset of unified diffs TermAl already emits
- validate the UX
- only introduce a third-party diff library if the internal renderer becomes a maintenance burden

## Review comment plan

### Comment scopes

Support these scopes in v1:
- change-set level comment
- file-level comment
- hunk-level comment
- line-level comment

### Stable anchors

Never anchor comments to DOM position. Use structured targets:

```ts
type ReviewAnchor =
  | { kind: "changeSet" }
  | { kind: "file"; filePath: string }
  | { kind: "hunk"; filePath: string; hunkHeader: string }
  | {
      kind: "line";
      filePath: string;
      hunkHeader: string;
      oldLine: number | null;
      newLine: number | null;
    };
```

### Review file format

Persist review state under the existing TermAl workspace data directory:

```text
.termal/
  sessions.json
  reviews/
    <change-set-id>.json
    <change-set-id>.md   # optional export later
```

Proposed JSON schema:

```json
{
  "version": 1,
  "changeSetId": "change-session-3-message-42",
  "origin": {
    "sessionId": "session-3",
    "messageId": "message-42",
    "agent": "Codex",
    "workdir": "/Users/greg/GitHub/Personal/termal",
    "createdAt": "2026-03-09T18:55:00Z"
  },
  "files": [
    {
      "filePath": "docs/bugs.md",
      "changeType": "edit"
    }
  ],
  "comments": [
    {
      "id": "comment-1",
      "anchor": {
        "kind": "line",
        "filePath": "docs/bugs.md",
        "hunkHeader": "@@ -10,3 +10,8 @@",
        "oldLine": null,
        "newLine": 17
      },
      "body": "This should mention Codex attachment support explicitly.",
      "status": "open",
      "author": "user",
      "createdAt": "2026-03-09T19:02:00Z",
      "updatedAt": "2026-03-09T19:02:00Z"
    }
  ]
}
```

Rules:
- `status` starts as `open`; allowed values: `open`, `resolved`, `applied`, `dismissed`.
- The agent should treat only `open` comments as active review feedback by default.
- The file should be fully replaceable on save to keep backend logic simple in v1.

## API plan

Add a small review API beside the existing session routes.

Suggested routes:
- `GET /api/reviews/{changeSetId}`
- `PUT /api/reviews/{changeSetId}`
- `GET /api/reviews/{changeSetId}/summary`

Notes:
- `PUT` can save the whole review document for v1 instead of building comment-level mutation
  endpoints.
- Review save/load does not need a separate realtime channel; normal UI state can refresh after
  save.
- If the frontend only needs the saved file path for handoff, the backend can also return
  `reviewFilePath` in the review payload.

## UI plan

### Conversation surface

- Add `Open preview` to each diff card.
- If multiple diff cards later share one `changeSetId`, the action can open the grouped preview.
- Add a subtle saved-review indicator when comments already exist for that change set.

### Diff preview tab

Header actions:
- `Back to conversation`
- `Copy review file path`
- `Insert review into prompt`
- `Raw patch`

Body:
- structured diff viewer
- inline comment affordances
- review sidebar or footer listing open and resolved comments

### Backlink behavior

- `Back to conversation` focuses the origin session tab if it is already open.
- If the origin session tab is not open, open it in the current pane and scroll to the origin
  message.
- Highlight the origin message briefly so the jump is visually obvious.

## Agent handoff plan

The point of saving review files is to make them usable in later turns without manual copy-paste.

V1 handoff flow:
1. User opens diff preview.
2. User adds comments and saves review.
3. UI shows the saved review path, for example
   `.termal/reviews/change-session-3-message-42.json`.
4. User clicks `Insert review into prompt`.
5. The composer gets a short, structured handoff message like:
   `Please address the open review comments in .termal/reviews/change-session-3-message-42.json`
6. The later agent turn reads the file from disk and resolves comments one by one.

## Implementation phases

### Phase 1: metadata and navigation

- Add `originSessionId`, `originMessageId`, and `changeSetId` to diff messages.
- Refactor frontend workspace state to generic tabs.
- Add `LinkTarget` and `openLink()`.
- Add `Open preview` and `Back to conversation`.

### Phase 2: diff preview

- Implement a small unified diff parser in the frontend.
- Render a structured diff preview tab.
- Keep raw patch fallback.
- Add tab dedupe by `changeSetId`.

### Phase 3: saved review comments

- Add backend review store under `.termal/reviews/`.
- Add `GET`/`PUT` review routes.
- Add inline and file-level comment UI.
- Save and reload review documents.

### Phase 4: agent handoff and polish

- Add `Insert review into prompt`.
- Add saved-review indicators in conversation cards.
- Add resolved/open filtering.
- Add optional Markdown export only if JSON proves too opaque for manual inspection.

## Testing plan

Backend:
- review file save/load round-trip
- invalid review payload rejection
- missing review file returns empty/default state
- diff message serialization includes origin metadata

Frontend:
- `Open preview` opens the correct diff preview tab
- re-opening the same change set focuses instead of duplicating
- backlink opens the right session and highlights the right message
- diff parser handles create and edit patches
- comment anchors survive reload from saved JSON

Integration:
- Claude-generated diff can open preview, save comments, and insert review path into prompt
- Codex-generated diff can do the same

## Acceptance criteria

- Clicking `Open preview` on a diff-related agent update opens a new diff preview tab.
- The preview tab can navigate back to the originating conversation message.
- The diff view is structured and readable without forcing the user to parse raw patch text.
- Review comments can be added at change-set, file, hunk, and line scope.
- Review comments persist to `.termal/reviews/<changeSetId>.json`.
- A later agent turn can be pointed at that file and identify open comments without ambiguity.

# Agent Integration Comparison

Cross-agent reference for TermAl adapter design.

## Protocol & Transport

| | Claude Code | Codex | Gemini CLI |
|---|---|---|---|
| **Wire protocol** | NDJSON (custom) | JSON-RPC 2.0 | Direct SDK / `stream-json` stdout |
| **Transport** | stdin/stdout only | stdin/stdout **or** WebSocket | In-process (SDK) or spawned CLI |
| **Bidirectional** | Yes (stdin/stdout) | Yes (JSON-RPC) | No (output-only stream; approval via policy, not IPC) |
| **Server mode** | No | `codex app-server` | No (`a2a-server` exists but different purpose) |
| **Best TermAl integration** | Spawn child, NDJSON over stdio | Spawn app-server, JSON-RPC over stdio | Spawn CLI with `--output-format stream-json` |

## Session Persistence

| | Claude Code | Codex | Gemini CLI |
|---|---|---|---|
| **Storage format** | Append-only `.jsonl` | Dual: JSONL rollout + SQLite cache | Single `.json` per session |
| **Location** | `.claude/sessions/{id}.jsonl` | `~/.codex/sessions/YYYY/MM/DD/rollout-{id}.jsonl` | `~/.gemini/tmp/<project>/chats/session-{ts}-{id}.json` |
| **Resume mechanism** | `--resume <session-id>` flag | `thread/resume` (by ID, path, or history) | Reload JSON, pass to `resumeChat()` |
| **Session discovery** | Scan `.claude/sessions/` | Query SQLite `threads` table | Scan `chats/` directory |
| **Cross-session memory** | File-based (`CLAUDE.md`) | Two-stage pipeline (extract → consolidate in SQLite) | `save_memory` tool writes to `GEMINI.md` |

## Context & Instructions

| | Claude Code | Codex | Gemini CLI |
|---|---|---|---|
| **Instructions file** | `CLAUDE.md` | `AGENTS.md` | `GEMINI.md` (configurable) |
| **Hierarchy** | Global + Project (2-tier) | Global + Project + Subdirectory (3-tier) | Global + Extension + Project (3-tier + JIT) |
| **Memory file** | `~/.claude/memory.md` | N/A (SQLite-based) | `~/.gemini/memory.md` |
| **Ignore file** | `.claudeignore` | N/A | `.geminiignore` |
| **JIT subdirectory discovery** | No | No | Yes |

## Approval & Permissions

| | Claude Code | Codex | Gemini CLI |
|---|---|---|---|
| **Mechanism** | `control_request` / `control_response` over stdio | `ServerRequest` types in JSON-RPC | Policy engine (TOML rules) + confirmation bus |
| **TermAl can intercept** | Yes (respond to `can_use_tool`) | Yes (respond to approval `ServerRequest`) | No (policy is evaluated in-process, not over IPC) |
| **Persist decisions** | Settings file | N/A | `auto-saved.toml` |
| **Safety checkers** | Built-in | Sandbox model | Pluggable (in-process + external) |

## Advanced Features

| | Claude Code | Codex | Gemini CLI |
|---|---|---|---|
| **Thread forking** | No | `thread/fork` | No |
| **Compaction** | Auto summarization | `thread/compact/start` | 2-pass LLM summarization + verification |
| **Rollback** | No | `thread/rollback` | `rewindTo(messageId)` + shadow git restore |
| **Archival** | No | `thread/archive` / `thread/unarchive` | No |
| **Loop detection** | Repetition detection | No | 3-tier (hash + content + LLM judge) |
| **Hooks** | Pre-commit style | No | 11 lifecycle events |
| **Sub-agents** | Via tools | Single agent | Local + remote (A2A protocol) |
| **Checkpointing** | Git (optional) | Git | Shadow git repo (independent of project git) |
| **Mid-turn interrupt** | `control_request` interrupt | Yes (via JSON-RPC) | No IPC mechanism |

## Pricing

| | Claude Code | Codex | Gemini CLI |
|---|---|---|---|
| **Model** | Subscription (Max plan) | Per-token (API key) | Per-token (API key or OAuth) |

## VS Code Extension

| | Claude Code | Codex | Gemini CLI |
|---|---|---|---|
| **Extension** | `anthropic.claude-code` | No official extension | `vscode-ide-companion` package |
| **Connects via** | Spawns CLI, NDJSON over stdio | N/A | HTTP + MCP (Model Context Protocol) |
| **Context sync** | File context via tools | N/A | Active file, cursor, selection, open files |
| **Auth** | Same process (no auth needed) | N/A | Bearer token via env var |
