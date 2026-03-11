# Bugs & Known Issues

Updated against the current checked-in code in `src/main.rs` and `ui/src/App.tsx`.

The older entries for "No image paste support", Claude `control_request` fallthrough, "No SSE/WebSocket for real-time updates", "Codex receive has no streaming", and "No queueing system for prompts" were stale. Those are implemented in the current tree.

The earlier command-card UX issue where `OUT` could render as an empty dark block was also fixed.
Command messages now use a compact `IN` / `OUT` layout with copy controls, a collapsible output
view for longer results, and a plain placeholder when there is no command output.

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

## No runtime pre-warming / session pooling

**Severity:** Medium — first message still pays startup cost.

Runtime processes are created lazily inside `dispatch_turn()`. When a session has
`SessionRuntime::None`, the first message spawns `spawn_claude_runtime()` or
`spawn_codex_runtime()`. With multi-project support (3–6 projects, each with multiple concurrent
sessions), naive pre-warming per project does not scale — holding idle processes for every
project is wasteful.

**Current impact:**
- Every new session pays process startup plus initialize-handshake latency on the first prompt
- Users running multiple concurrent sessions per project hit the cold start repeatedly

**Design: two strategies, split by agent protocol**

**Codex — single shared app-server (no pool needed).**
The Codex app-server is a long-lived JSON-RPC process. Each conversation is a `thread/start` call
that accepts its own `cwd`. A single app-server process can serve multiple sessions across
different projects — just call `thread/start` again with a different working directory. The current
architecture spawns one app-server per session with a session-scoped `CODEX_HOME`, which is
unnecessary overhead.

Refactor to:
1. Spawn one global Codex app-server on first Codex session creation (or on app start).
2. All Codex sessions share the single app-server process.
3. Each session calls `thread/start` with its own project `cwd`.
4. Session creation becomes near-instant — no process spawn, just a JSON-RPC call.
5. The session-scoped `CODEX_HOME` setup needs to be rethought (shared home, or per-project home
   instead of per-session).

**Claude — hidden session pool per `(project, agent)` tuple.**
The Claude protocol has no session reset — each process is one conversation. A process cannot be
reused for a new session. Pre-warming means spawning spare processes.

The pool strategy:
1. When the first Claude session spawns in a project directory, also create a **hidden session**
   for the same `(project, agent)` with a fully initialized runtime (reader threads, writer
   threads, initialize handshake — everything).
2. Hidden sessions are real sessions with real runtimes, just not visible in the UI.
3. When the user creates a new Claude session in that project, **unhide** the spare instead of
   cold-starting. Session #2 onwards is instant.
4. After unhiding, spawn the next hidden spare in the background.
5. Pool size of 1 spare per active `(project, agent)` is enough — the user rarely creates two
   sessions simultaneously.

Backend changes:
- Add a `hidden: bool` field to `Session` (or a `SessionVisibility` enum).
- The server filters hidden sessions from UI-facing API responses.
- "Create session" checks the pool first, unhides if a match exists, falls back to cold spawn.
- After any session spawn (visible or hidden), trigger spare creation for the same key.
- Hidden sessions that sit idle too long can be reaped to avoid unbounded resource use.

**Why not pre-warm on app startup:**
With 3–6 projects, spawning spares for all of them upfront wastes resources for projects the user
may not touch. The pool only activates for projects already in use, which is the right trade-off.

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

## No model change on running sessions

**Severity:** Medium — model is stored but never used or surfaced.

`Session.model` exists as a `String` field in both the Rust backend and TypeScript frontend, but it
is hardcoded to a static label (`"claude -p"` / `"codex exec"`) at session creation via
`Agent::model_label()`. The field is never displayed in the UI and cannot be changed. Sessions
always start with the agent's default model, which is correct — the missing piece is letting the
user switch models after a session is running, the way `/model` works inside Claude Code itself.

**What already works:**
- Claude protocol supports `set_model` control request for mid-session changes (spec lines 196–198)
- Claude's initialize `control_response` returns available `models` — TermAl ignores this today
- Codex model is controlled via `config.toml` in the session-scoped `CODEX_HOME` — TermAl already
  syncs this file but never writes a custom model into it
- `ClaudeRuntimeCommand` enum already follows the pattern needed (`SetPermissionMode` exists as a
  template for `SetModel`)

**Model discovery:**
- **Claude:** Parse the `models` field from the initialize `control_response`. Cache it in
  `AppState`. Persist to disk so subsequent app launches have the list immediately. Before any
  session has run, the list is empty — show "Default" only. Once any session initializes, the full
  list populates and persists.
- **Codex:** Read from `models_cache.json` in `CODEX_HOME` (already synced by
  `seed_termal_codex_home_from`).

**Backend tasks (`src/main.rs`):**
- Add `model: Option<String>` to `UpdateSessionSettingsRequest` (line ~5514)
- Add `SetModel(String)` variant to `ClaudeRuntimeCommand` (line ~1971) and handle it in the writer
  thread (line ~3003)
- Add `write_claude_set_model` following the pattern of `write_claude_set_permission_mode`
  (line ~3271) — sends a `{"subtype": "set_model", "model": "..."}` control request
- In `update_session_settings` (line ~465): for Claude, send `SetModel` to the live runtime via
  `input_tx`; for Codex, write the model into `config.toml` in the session's CODEX_HOME (takes
  effect on next turn). Update `session.model` in state for both.
- Parse the `models` list from Claude's initialize `control_response` in the stdout reader thread
  (line ~3070 area). Store in `AppState` and persist to disk.
- Read Codex `models_cache.json` on startup or first Codex session creation.
- Add `GET /api/models` endpoint returning `{ claude: [...], codex: [...] }`.

**Frontend tasks:**
- Add `model?: string` to the `updateSessionSettings` payload in `api.ts` (line ~88)
- Add a `fetchModels` API call for `GET /api/models`
- Add `"model"` to `SessionSettingsField` union and wire through `handleSessionSettingsChange`
- Add model `ThemedCombobox` to both `ClaudeSessionSettings` and `CodexSessionSettings` components,
  populated from the models API

**Codex mid-session note:** Codex has no mid-turn protocol for model switching. Changing the model
updates `config.toml` and takes effect on the next turn. The UI should communicate this.

## No slash command support

**Severity:** Medium — blocks parity with the native Claude Code experience.

Claude Code exposes slash commands (`/review`, `/release-notes`, `/security-review`, `/simplify`,
`/batch`, `/context`, `/extra-usage`, `/insights`, etc.) through a picker UI triggered by typing `/`
in the input. TermAl has no equivalent — there is no command discovery, no picker, and no way to
invoke these commands.

**How it works in the protocol:**

The `system` event with `subtype: "init"` (the initialize response) returns three fields that TermAl
currently ignores:
- `commands` — array of available slash commands with metadata (name, description, etc.)
- `models` — array of available models (covered in the model change item above)
- `pid` — Claude's process ID

TermAl only extracts `session_id` from this response (line ~4348 in `handle_claude_event`). The
`commands` and `models` fields are silently dropped.

**How commands are invoked:**

Slash commands are sent as regular user messages. The client provides the autocomplete/picker UI, but
the actual text (e.g. `/review`) is sent as a normal `user` message over the NDJSON protocol. Claude
Code handles the command server-side. This means TermAl does not need a special protocol path for
commands — it only needs:
1. Discovery: parse the `commands` field from the init response
2. UI: show a command picker when the user types `/` in the composer
3. Dispatch: send the selected command text as a regular message

**The screenshot also shows a "Context" section** with items like "Resume conversation". This is a
separate concept from slash commands — it relates to session resume and context management. This may
come from the same init response or from a separate protocol field.

**Backend tasks (`src/main.rs`):**
- In `handle_claude_event` (line ~4348), parse `commands` from the `system` init event alongside
  `session_id`. Each command likely has at minimum `name` and `description` fields.
- Store available commands per session in `SessionRecord` or in a shared `AppState` cache (commands
  are likely the same across all Claude sessions for a given account).
- Expose commands through the state API so the frontend can read them — either as a field on
  `Session` or via a dedicated `GET /api/commands` endpoint.
- Persist to disk alongside the models cache so commands are available on next app launch before any
  session initializes.

**Frontend tasks:**
- Add a `SlashCommand` type (e.g. `{ name: string; description: string }`) to `types.ts`
- Detect `/` at the start of composer input and show a filtered command picker (similar to how
  Claude Code shows the picker in the screenshot)
- On selection, either insert the command text into the composer or send it directly as a message
- The picker should support keyboard navigation and fuzzy filtering (user typed `/re` to filter
  down to `/review`, `/release-notes`)

**Codex equivalent:** Codex may not have an equivalent slash command system. If it does, it would
come through the JSON-RPC initialize response. Check `models_cache.json` and the Codex app-server
init handshake for any command-like metadata.

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

## Streaming refresh path is still heavier than necessary

**Severity:** Medium — noticeable when one session is streaming and the user is typing in another.

Prompt queueing, queued-prompt cancel, and `Stop` are implemented now. The biggest remaining
latency issue is the refresh path during live streaming.

**What improved already:**
- Draft keystrokes are now local to the composer instead of updating top-level app state on every key
- Session/message identity is preserved across many SSE updates so unchanged cards do not fully churn
- Streamed text deltas no longer rewrite persisted session state on every chunk

**What still happens:**
- The backend still publishes a full `/api/events` state snapshot for each streamed text delta
- The frontend still reconciles the full sessions array and reruns pane-level derivations on each snapshot
- Under concurrent activity, that can still make typing feel slower than it should

**Tasks:**
- Profile frontend rerenders during active streaming to identify the remaining hot subtrees
- Narrow state adoption so unrelated sessions do less work when another session streams
- If needed later, move from full-state SSE snapshots to smaller incremental update events

## Agent replies in diff review comments

**Severity:** Medium — closes the review feedback loop.

When the user leaves review comments on a diff preview and hands them off to an agent, the agent
currently has no way to reply inline. Comments are one-directional: user writes, agent reads.

**Desired behavior:**
- When an agent addresses a review comment, it can post a reply on the same anchor
- Agent replies appear inline in the diff preview alongside the user's original comment
- Each comment thread shows the back-and-forth (user comment → agent reply → user follow-up)
- Agent replies set the comment status to `resolved` or leave it `open` with an explanation

**Tasks:**
- Extend the review comment schema to support threaded replies with an `author` field (`user` or `agent`)
- Add a backend endpoint or convention for the agent to append replies to an existing review file
- Update the diff preview UI to render comment threads instead of single comments
- Update the agent handoff prompt to instruct the agent to write replies, not just resolve silently

## No territory visualization

**Severity:** High — the single biggest coordination gap in a multi-agent workflow.

A developer paired with an agent has immense leverage: one person can drive multiple agents across
different parts of a codebase simultaneously. But that leverage collapses without coordination
visibility. Today the user has to hold the full territorial picture in their head — which agent is
working where, which files are in flight, whether two sessions are about to collide. That mental
bookkeeping scales badly and is the first thing to break under load.

File edits are buried inside individual conversation streams. There is no cross-session view that
answers "which agent last touched this file?", "what is each agent working on right now?", or "are
two sessions about to collide on the same module?" The developer is the sole coordination layer, and
the tool gives them nothing to coordinate with.

**Why this matters more than most features:**
- The value of TermAl scales with how many agents the developer can run concurrently
- Concurrent agents are only useful if the developer can steer them without constant context-switching
- Territory visualization is the difference between "I'm running three agents" and "I'm effectively
  managing three agents" — without it, parallelism becomes chaos
- Conflict detection is not just about preventing git merge pain; it is about preserving the
  developer's trust that concurrent agents are safe to run

**Desired behavior:**
- A dedicated territory view (tab or overlay) shows the project tree annotated with agent activity
- Each file or directory shows which agent(s) have read or written it during the current work session
- Color-coded ownership makes it obvious at a glance: e.g. blue = Claude, orange = Codex,
  green = Gemini, striped = contested (multiple agents touched it)
- Recency matters: recent changes are brighter/bolder, stale activity fades
- Clicking a file in the territory view jumps to the most recent conversation message where that
  file was changed
- A heatmap mode can highlight hotspots — files with the most churn across agents
- Conflict warnings surface when two active sessions are both editing the same file or overlapping
  lines
- A compact summary bar (always visible, not just in the territory tab) shows the live territory
  status: e.g. "Claude: 4 files · Codex: 7 files · 1 conflict"

**Data sources:**
- Diff messages already carry `filePath` and `changeType` per agent turn
- Tool-use events (file reads, writes, command executions) can be tagged with session and agent
- Git status can supplement the view with uncommitted changes not yet attributed to an agent

**Tasks:**
- Track file-level read/write activity per session in backend state (agent, session, file, action,
  timestamp)
- Add a `/api/territory` endpoint that returns the aggregated activity map
- Add a territory view tab using the generic workspace tab system
- Render a project tree with agent-colored annotations and recency decay
- Add a heatmap toggle that ranks files by cross-agent churn
- Add conflict detection: warn when two active sessions have pending writes to the same file
- Add click-through navigation from territory entries to the originating conversation message
- Add a persistent territory summary bar visible across all tabs
- Optionally overlay territory indicators in the source view and diff preview tabs

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
- [ ] Add territory visualization:
  track per-session file read/write activity in backend state, expose it through `/api/territory`,
  render a project-tree view color-coded by agent with recency decay, heatmap mode, conflict
  warnings, click-through to originating messages, and a persistent summary bar. This is the core
  coordination surface that makes concurrent agent workflows safe and manageable.

## P1

- [ ] Add slash command support:
  parse the `commands` field from Claude's initialize `system` init event, store and expose
  available commands, and add a `/`-triggered command picker in the composer with fuzzy filtering
  and keyboard navigation. Commands are sent as regular user messages.
- [ ] Add model change on running sessions:
  discover available models dynamically from Claude's initialize `control_response` and Codex's
  `models_cache.json`, cache and persist the list, expose via `GET /api/models`, send `set_model`
  control request to Claude and update `config.toml` for Codex, and add a model selector to the
  session settings UI. Sessions start with the default model — this is about changing it after.
- [ ] Migrate REPL mode off legacy `codex exec --json` and onto the app-server path so server mode
  and REPL mode share one implementation.
- [ ] Replace the `try_wait()` polling loops in the Claude and Codex runtime supervisors with
  blocking wait threads or async child handling.
- [ ] Refactor Codex to a single shared app-server:
  replace per-session app-server spawning with one long-lived process that serves all Codex
  sessions via `thread/start` with per-session `cwd`. Rethink session-scoped `CODEX_HOME`.
- [ ] Add Claude hidden session pool:
  when the first Claude session spawns in a project, create a hidden spare session with a fully
  initialized runtime for the same `(project, cwd)`. On new session creation, unhide the spare
  and spawn the next one. Add `hidden` field to `Session`, filter from UI responses, and add
  idle reaping.
- [ ] Fix Windows path resolution:
  add `USERPROFILE` fallback for Codex home and TermAl data directory resolution.
- [ ] Align attachment UX with actual capabilities:
  show the right composer hint per agent, add drag-and-drop, and keep the docs in sync with the
  implementation.
- [ ] Reduce streaming refresh overhead:
  profile SSE-driven rerenders while another session is active, narrow state adoption for
  unrelated sessions, and only consider incremental events after the frontend hot path is trimmed.
- [ ] Add post-edit diff preview from agent messages:
  when an agent reports that it updated a file, let the user open a new tab with a diff preview of
  those changes and include a link back to the originating conversation or message.
- [ ] Add saved review comments on diff previews:
  let the user leave PR-style comments on files or hunks, persist them to disk in a structured
  format, and make them available for later agent turns.
- [ ] Add agent replies to diff review comments:
  let the agent post threaded replies on review comment anchors so the diff preview shows a
  back-and-forth conversation instead of one-directional user comments.

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

# Implementation Plan: Territory Visualization

This is the concrete delivery plan for the territory visualization feature.

## Core insight

The developer paired with agents is the coordination layer. TermAl's value scales with how many
agents the developer can run concurrently, but that only works if the tool gives them a live picture
of who is changing what. Territory visualization is not a dashboard — it is the coordination
surface.

## Goals

- Maintain a server-side aggregated index of all file-level activity across sessions and agents.
- Show the developer a live, at-a-glance territorial map of the project.
- Detect and surface conflicts before they become git merge problems.
- Catch external changes (editor saves, other tools, remote pushes) so the map stays honest.

## Non-goals for v1

- No line-level or hunk-level territory granularity (file-level is enough to start).
- No automatic conflict resolution or agent orchestration.
- No cross-repo territory (single working directory per TermAl instance).
- No persistent territory history across TermAl restarts in v1 (rebuild from session replay later).

## Data model

### Touch event

Every time an agent reads, writes, creates, or deletes a file, the backend records a touch:

```rust
struct Touch {
    file_path: String,
    session_id: String,
    agent: Agent,              // Claude, Codex, Gemini
    action: TouchAction,       // Read, Write, Create, Delete
    lines_added: u32,
    lines_removed: u32,
    message_id: String,        // for click-through to conversation
    timestamp: DateTime<Utc>,
}

enum TouchAction {
    Read,
    Write,
    Create,
    Delete,
}
```

Sources:
- `DiffMessage` on turn completion → `Write` / `Create` / `Delete` with line counts from the diff
- Tool-use events (file_read, file_write, command execution) → `Read` / `Write`
- These are already flowing through the session message stream; the territory index just needs to
  observe them

### File aggregate

The territory index maintains a rolled-up summary per file:

```rust
struct FileTerritory {
    file_path: String,
    touches: Vec<Touch>,        // append-only log
    dominant_agent: Option<Agent>,
    agents_involved: HashSet<Agent>,
    sessions_involved: HashSet<String>,
    contested: bool,            // true if multiple agents have written
    total_writes: u32,
    total_lines_changed: u32,
    last_write: DateTime<Utc>,
    last_read: DateTime<Utc>,
    external_change_detected: bool,
}
```

Rules:
- `dominant_agent` = the agent with the most total `lines_changed` (writes only, reads don't count
  for dominance)
- `contested` = true when two or more distinct agents have at least one `Write` / `Create` / `Delete`
  on the same file
- `external_change_detected` = true when the git poll finds changes not attributable to any session

### Directory rollup

Aggregate file-level data upward into directories so the tree view can show territory at any depth:

```rust
struct DirectoryTerritory {
    dir_path: String,
    dominant_agent: Option<Agent>,
    contested: bool,
    file_count: u32,             // files with any touches under this dir
    contested_file_count: u32,
    agents_involved: HashSet<Agent>,
}
```

This is computed on demand from the file index, not stored separately.

## Git supplementation

The territory map is only useful if it is honest. Agent-tracked touches cover TermAl activity, but
the developer also edits files in their editor, runs scripts, pulls from remote, etc. A periodic
git poll fills that gap.

### Working tree poll

A background task runs on a configurable interval (default: 5 seconds):

1. Run `git status --porcelain` to get the list of modified, added, and deleted files in the
   working tree.
2. For each changed file, check whether the territory index already has a recent touch that explains
   the change (i.e., a TermAl session wrote it within the last poll interval).
3. Any file that changed but has no matching TermAl touch → mark as `external_change_detected` and
   record an `External` touch with no session or agent attribution.
4. Files that were previously marked external but are no longer in `git status` output → clear the
   external flag (the change was committed or reverted).

### Remote poll

A separate, less frequent background task (default: 60 seconds, configurable):

1. Run `git fetch --quiet` to update remote tracking refs.
2. Run `git rev-list --count HEAD..@{upstream}` to check if upstream has new commits.
3. If upstream has diverged, optionally run `git diff --name-only HEAD...@{upstream}` to get the
   list of files that would change on pull.
4. Surface these as `upstream` territory entries — files the remote has changed that the developer
   hasn't pulled yet.

This does NOT auto-pull. It just makes the territory map aware that the ground has shifted.

### Git poll constraints

- Both polls run in a dedicated background thread, not on the main tokio runtime, to avoid blocking
  async work.
- Poll intervals should be configurable through the settings API.
- The working tree poll should debounce: if a TermAl agent is actively writing (a turn is in
  progress), skip the poll or suppress external attribution for files the active session is known to
  be editing.
- The remote poll should be opt-in or off by default if the repo has no configured upstream.

## Conflict detection

Contested files are the highest-signal output of the territory system. The backend should
proactively detect and categorize conflicts:

**Level 1 — File-level contest:**
Two or more agents have written to the same file. Low urgency; this is information, not necessarily
a problem.

**Level 2 — Active collision:**
Two sessions with active (running) turns are both writing to the same file right now. Higher
urgency; one of them is likely about to create a merge conflict.

**Level 3 — External desync:**
An agent wrote a file, and then an external change was detected on the same file before the agent's
changes were committed. The agent's mental model of that file is now stale.

Each conflict level should surface differently in the UI (color intensity, icon, notification).

## API

### Territory snapshot

`GET /api/territory`

Returns the full territory map:

```json
{
  "files": [
    {
      "filePath": "src/main.rs",
      "dominantAgent": "Claude",
      "agentsInvolved": ["Claude", "Codex"],
      "sessionsInvolved": ["session-1", "session-4"],
      "contested": true,
      "totalWrites": 12,
      "totalLinesChanged": 347,
      "lastWrite": "2026-03-10T14:22:00Z",
      "lastRead": "2026-03-10T14:25:00Z",
      "externalChangeDetected": false,
      "conflictLevel": 1
    }
  ],
  "conflicts": [
    {
      "filePath": "src/main.rs",
      "level": 1,
      "agents": ["Claude", "Codex"],
      "sessions": ["session-1", "session-4"],
      "description": "Both Claude and Codex have written to this file"
    }
  ],
  "summary": {
    "totalTrackedFiles": 23,
    "byAgent": {
      "Claude": { "files": 8, "linesChanged": 412 },
      "Codex": { "files": 17, "linesChanged": 891 }
    },
    "contestedFiles": 2,
    "externalChanges": 1,
    "activeConflicts": 0
  },
  "gitStatus": {
    "upstreamBehind": 3,
    "upstreamFiles": ["README.md", "Cargo.toml", "src/lib.rs"]
  }
}
```

### Territory for a single file

`GET /api/territory/{filePath}`

Returns the full touch log for one file, including the click-through `messageId` for each touch.
Useful for the detail drill-down.

### Territory SSE

Territory updates should piggyback on the existing `/api/events` SSE stream. When the territory
index changes (new touch, conflict detected, external change found), include a territory delta in
the next SSE snapshot so the frontend stays live without polling.

## UI

### Territory tab

A new workspace tab type:

```ts
type WorkspaceTab =
  | // ... existing types
  | { id: string; kind: "territory" };
```

The tab renders a collapsible project tree with:
- Agent color indicators per file and directory (solid = one agent, striped = contested)
- Recency decay: bright for recent activity, fading over time
- Inline metrics: lines changed, write count
- Conflict badges at each level
- External change indicators
- Click any file → opens the most recent conversation message where it was changed

### Summary bar

Always visible across all tabs (in the status area or header):

`Claude: 8 files (412 lines) · Codex: 17 files (891 lines) · 2 contested · 1 external`

Clicking the summary bar opens the territory tab. Conflict counts should pulse or highlight when a
new conflict is detected.

### Heatmap mode

A toggle in the territory tab that reranks the tree by activity intensity instead of alphabetical
path order. Files with the most cross-agent churn float to the top. Useful for spotting hotspots
when the project tree is large.

### Conflict notifications

When a Level 2 (active collision) or Level 3 (external desync) conflict is detected, surface a
non-blocking toast notification so the developer sees it even if they are not looking at the
territory tab.

## Implementation phases

### Phase 1: touch tracking and server index

- Add the `Touch` and `FileTerritory` structs to backend state.
- Hook into `dispatch_turn()` completion to record touches from `DiffMessage` events.
- Hook into tool-use events for read tracking.
- Add `GET /api/territory` returning the snapshot.
- Add territory deltas to the SSE stream.

### Phase 2: territory tab and summary bar

- Add `territory` as a `WorkspaceTab` kind.
- Render the project tree with agent colors and recency decay.
- Add the persistent summary bar.
- Add click-through from territory entries to conversation messages.

### Phase 3: git supplementation

- Add the working tree poll background task.
- Add external change detection and attribution.
- Add the remote poll background task (opt-in).
- Surface upstream divergence in the territory snapshot and UI.

### Phase 4: conflict detection and notifications

- Implement the three conflict levels.
- Add conflict badges to the territory tree.
- Add toast notifications for Level 2 and Level 3 conflicts.
- Add heatmap mode.

## Testing plan

Backend:
- Touch recording from a simulated turn with diff messages
- File aggregate computation (dominant agent, contested flag, line counts)
- Directory rollup correctness
- External change detection against a mock `git status` output
- Conflict level classification
- Territory snapshot API round-trip

Frontend:
- Territory tab renders file tree with correct agent colors
- Summary bar updates live from SSE deltas
- Recency decay visual correctness (mock timestamps)
- Click-through opens the right session and message
- Heatmap mode reorders by activity
- Conflict toast appears on Level 2+ detection

Integration:
- Two concurrent sessions (Claude + Codex) editing different files → no conflicts, clean territory
- Two concurrent sessions editing the same file → contested flag, Level 1 conflict
- External file edit between agent turns → external change detected, Level 3 if agent wrote it
- Git fetch reveals upstream changes → upstream files surface in territory

## Acceptance criteria

- The territory tab shows a project tree annotated with which agent(s) touched each file.
- The summary bar is visible across all tabs and shows live agent activity counts.
- Contested files are visually distinct from single-agent files.
- External changes (edits outside TermAl) are detected and shown within the poll interval.
- Clicking a file in the territory view navigates to the conversation message that last changed it.
- Conflict notifications surface without requiring the developer to check the territory tab.

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
