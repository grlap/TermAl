# Bugs & Known Issues

Updated against the current checked-in code in `src/main.rs` and `ui/src/App.tsx`.

The older entries for "No image paste support", Claude `control_request` fallthrough, "No SSE/WebSocket for real-time updates", "Codex receive has no streaming", and "No queueing system for prompts" were stale. Those are implemented in the current tree.

The earlier command-card UX issue where `OUT` could render as an empty dark block was also fixed.
Command messages now use a compact `IN` / `OUT` layout with copy controls, a collapsible output
view for longer results, and a plain placeholder when there is no command output.

## Image attachment UX is inconsistent

**Severity:** Low â€” the transport path works, but the product copy and interaction model lag behind it.

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

**Severity:** Medium â€” blocks Windows support.

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

**Severity:** Medium â€” first message still pays startup cost.

Runtime processes are created lazily inside `dispatch_turn()`. When a session has
`SessionRuntime::None`, the first message spawns `spawn_claude_runtime()` or
`spawn_codex_runtime()`. With multi-project support (3â€“6 projects, each with multiple concurrent
sessions), naive pre-warming per project does not scale â€” holding idle processes for every
project is wasteful.

**Current impact:**
- Every new session pays process startup plus initialize-handshake latency on the first prompt
- Users running multiple concurrent sessions per project hit the cold start repeatedly

**Design: two strategies, split by agent protocol**

**Codex â€” single shared app-server (no pool needed).**
The Codex app-server is a long-lived JSON-RPC process. Each conversation is a `thread/start` call
that accepts its own `cwd`. A single app-server process can serve multiple sessions across
different projects â€” just call `thread/start` again with a different working directory. The current
architecture spawns one app-server per session with a session-scoped `CODEX_HOME`, which is
unnecessary overhead.

Refactor to:
1. Spawn one global Codex app-server on first Codex session creation (or on app start).
2. All Codex sessions share the single app-server process.
3. Each session calls `thread/start` with its own project `cwd`.
4. Session creation becomes near-instant â€” no process spawn, just a JSON-RPC call.
5. The session-scoped `CODEX_HOME` setup needs to be rethought (shared home, or per-project home
   instead of per-session).

**Claude â€” hidden session pool per `(project, agent)` tuple.**
The Claude protocol has no session reset â€” each process is one conversation. A process cannot be
reused for a new session. Pre-warming means spawning spare processes.

The pool strategy:
1. When the first Claude session spawns in a project directory, also create a **hidden session**
   for the same `(project, agent)` with a fully initialized runtime (reader threads, writer
   threads, initialize handshake â€” everything).
2. Hidden sessions are real sessions with real runtimes, just not visible in the UI.
3. When the user creates a new Claude session in that project, **unhide** the spare instead of
   cold-starting. Session #2 onwards is instant.
4. After unhiding, spawn the next hidden spare in the background.
5. Pool size of 1 spare per active `(project, agent)` is enough â€” the user rarely creates two
   sessions simultaneously.

Backend changes:
- Add a `hidden: bool` field to `Session` (or a `SessionVisibility` enum).
- The server filters hidden sessions from UI-facing API responses.
- "Create session" checks the pool first, unhides if a match exists, falls back to cold spawn.
- After any session spawn (visible or hidden), trigger spare creation for the same key.
- Hidden sessions that sit idle too long can be reaped to avoid unbounded resource use.

**Why not pre-warm on app startup:**
With 3â€“6 projects, spawning spares for all of them upfront wastes resources for projects the user
may not touch. The pool only activates for projects already in use, which is the right trade-off.

## Legacy/testing Codex REPL path still uses one-shot `codex exec --json`

**Severity:** Low â€” server mode is fixed, but the old testing path is not.

The server path now uses persistent `codex app-server` JSON-RPC with streaming `item/agentMessage/delta` events. However, `run_turn_blocking()` still routes the Codex REPL/testing mode through `run_codex_turn()`, which shells out to `codex exec --json` or `codex exec resume --json`.

**Impact:**
- REPL mode does not share the persistent app-server runtime
- REPL mode does not share the server approval flow
- Legacy `handle_codex_event()` and rollout-fallback code still have to be maintained

## Unhandled Codex rate limit notifications

**Severity:** Low â€” harmless, but noisy.

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


## Feature briefs

- [Session Model Switching](./features/model-switching.md)
- [Slash Commands](./features/slash-commands.md)
- [Gemini CLI Integration](./features/gemini-cli-integration.md)
- [Diff Review Workflow](./features/diff-review-workflow.md)
- [Territory Visualization](./features/territory-visualization.md)
- [Agent Integration Comparison](./features/agent-integration-comparison.md)

# Backlog

## Single-file codebase

**Severity:** Medium â€” maintainability concern, not a runtime bug.

`src/main.rs` is now ~5,700 lines and still contains routes, persistence, session state, Claude integration, Codex integration, REPL code, and tests. The logical boundaries are clear, but the file is past the point where changes are easy to isolate or review.

## Codex app-server integration is partial

**Severity:** Medium â€” server-mode basics are in place, but protocol coverage is still incomplete.

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

**Severity:** Medium - detailed brief:
- [Session Model Switching](./features/model-switching.md)

## No slash command support

**Severity:** Medium - detailed brief:
- [Slash Commands](./features/slash-commands.md)

## No Gemini CLI integration

**Severity:** Medium - detailed brief:
- [Gemini CLI Integration](./features/gemini-cli-integration.md)

## No test coverage for Codex app-server parsing or HTTP endpoints

**Severity:** Medium

There are unit tests for Claude parsing and for the older `handle_codex_event()` parsing path, but there are no tests for the newer Codex app-server message handling (`handle_codex_app_server_message()` and related helpers), and there are no HTTP route tests for the axum handlers.

## Codex session discovery is reinvented

**Severity:** Low â€” opportunity, not a runtime bug.

Codex already maintains thread state in `~/.codex/state.db`. TermAl still persists its own session list instead of querying Codex's own thread metadata.

If thread discovery eventually moves to Codex's database, TermAl could reuse existing metadata instead of maintaining a parallel index.

## Streaming refresh path is still heavier than necessary

**Severity:** Medium â€” noticeable when one session is streaming and the user is typing in another.

Prompt queueing, queued-prompt cancel, and `Stop` are implemented now. The biggest remaining
latency issue is the refresh path during live streaming.

**What improved already:**
- Draft keystrokes are now local to the composer instead of updating top-level app state on every key
- Session/message identity is preserved across many SSE updates so unchanged cards do not fully churn
- Streamed text deltas no longer rewrite persisted session state on every chunk
- Active long conversations now use a windowed message list instead of mounting every message card
- Heavy markdown and code blocks now defer their expensive render work until near the viewport
- Cached conversation pages per pane are now bounded so hidden long tabs do not grow without limit

**What still happens:**
- The backend still publishes a full `/api/events` state snapshot for each streamed text delta
- The frontend still reconciles the full sessions array and reruns pane-level derivations on each snapshot
- Under concurrent activity, that can still make typing feel slower than it should

**Tasks:**
- Profile frontend rerenders during active streaming to identify the remaining hot subtrees

## Agent replies in diff review comments

**Severity:** Medium - detailed brief:
- [Diff Review Workflow](./features/diff-review-workflow.md)

## No territory visualization

**Severity:** High - detailed brief:
- [Territory Visualization](./features/territory-visualization.md)

- Add click-through navigation from territory entries to the originating conversation message
- Add a persistent territory summary bar visible across all tabs
- Optionally overlay territory indicators in the source view and diff preview tabs

## Spec drift

**Severity:** Low â€” documentation only.

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
  session settings UI. Sessions start with the default model â€” this is about changing it after.
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
