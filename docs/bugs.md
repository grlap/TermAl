# Bugs & Known Issues

Updated against the current checked-in code in `src/main.rs`, `ui/src/App.tsx`,
`ui/package.json`, and `ui/vite.config.ts`.

The older entries for "No image paste support", Claude `control_request` fallthrough, "No SSE/WebSocket for real-time updates", "Codex receive has no streaming", "No queueing system for prompts", Windows `HOME`-only path resolution, and unhandled Codex rate-limit notifications were stale. Those are implemented in the current tree.

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

## Source file reads are not scoped to the active session or workspace

**Severity:** High - source mode can read the wrong file and currently trusts arbitrary absolute paths.

The source viewer sends a raw `path` to `GET /api/file`. The backend accepts any absolute path as-is
and resolves relative paths against the backend process cwd rather than the session's `workdir`.
That means source-mode correctness depends on where TermAl was launched, not on the active session.

**Current impact:**
- Relative diff paths from sessions rooted in another project can resolve to the wrong file
- The source viewer can read files outside the active project if a diff path or manual entry points there
- Multi-project behavior is inconsistent because file lookup is not tied to session context

**Affected code (`src/main.rs`, `ui/src/App.tsx`, `ui/src/api.ts`):**
- `resolve_requested_path()` resolves relative paths against `std::env::current_dir()`
- `/api/file` does not validate that a requested path stays inside an allowed root
- `fetchFile()` and the source-view loader forward only a path, with no session/workdir context

**Fix:**
- Change file reads to resolve relative paths against the requesting session's `workdir`
- Reject reads outside the allowed project root set instead of accepting arbitrary absolute paths
- Consider including `sessionId` in the file-read route so validation has the right context

## Claude approval cancel can leave a session stuck in Approval

**Severity:** Medium

When Claude emits `control_cancel_request`, TermAl removes the internal pending-approval entry but
does not update the approval message, session status, or preview. If the turn then completes
without another state transition, the session can remain stuck in `Approval`.

**Current impact:**
- The UI can continue showing a canceled approval as if it were still live
- Later prompts get queued because `dispatch_turn()` treats `Approval` as busy
- The session may need to be stopped or restarted to recover

**Affected code (`src/main.rs`):**
- `clear_claude_pending_approval_by_request()`
- `control_cancel_request` handling in the Claude reader loop
- `finish_turn_ok_if_runtime_matches()`, which only transitions `Active -> Idle`

**Fix:**
- When a Claude approval is canceled, update the corresponding message state and republish session state
- Recompute session status from the remaining live approvals instead of leaving it at `Approval`
- Add a regression test for canceled approvals

## Multiple simultaneous approvals are not modeled correctly

**Severity:** Medium

Approval state is effectively treated as a single boolean on the session. `update_approval()`
answers one approval message and then moves the whole session back to `Active`, even if other
pending approvals still exist.

**Current impact:**
- A second live approval can become unanswerable once the first one is resolved
- Session preview/status can claim the agent is continuing even though another approval is still pending
- This is fragile against future Codex or Claude protocol changes that emit multiple approvals in flight

**Affected code (`src/main.rs`):**
- `update_approval()`
- `pending_claude_approvals` / `pending_codex_approvals` bookkeeping

**Fix:**
- Base session status on whether any live approvals remain after each decision
- Keep approval messages independently resolvable instead of gating everything on one session-level `Approval` state
- Add tests that exercise two concurrent approvals for both agents

## Initial state bootstrapping can apply an older snapshot after a newer SSE update

**Severity:** Medium

The frontend opens `/api/events` and separately calls `/api/state` during initial load. The SSE
stream already emits an initial snapshot immediately, so a newer SSE payload can arrive before the
older `/api/state` response and then get overwritten by that stale response.

**Current impact:**
- The UI can briefly roll back to older session state during startup or reconnect
- Active-session status, previews, or message lists can flicker backwards before the next SSE event

**Affected code (`src/main.rs`, `ui/src/App.tsx`):**
- `/api/events` emits an initial `state` event
- The app boot effect also calls `fetchState()` and unconditionally adopts the result

**Fix:**
- Use one bootstrap path instead of two, or add a monotonic revision so older snapshots can be ignored
- Add a frontend regression test that simulates SSE beating the `/api/state` response

## Polling for process exit

**Severity:** Low

Both `spawn_claude_runtime()` and `spawn_codex_runtime()` still use a `sleep(100ms)` loop around `child.try_wait()` to detect process exit. This is functional, but it is still polling.

**Affected code (`src/main.rs`):**
- Claude wait thread in `spawn_claude_runtime()`
- Codex wait thread in `spawn_codex_runtime()`

**Fix:** Replace the polling loop with a dedicated waiter thread that blocks on `child.wait()`, or move runtime supervision to async child handling.

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

---


## Node 24 deprecation warning from the legacy Vite dev proxy

**Severity:** Low - local dev noise only.

The UI toolchain is still on an older Vite stack:
- `vite` 2.9.18
- `@vitejs/plugin-react` 1.3.2
- `vitest` 0.18.1

When the dev server runs on modern Node releases such as Node 24, Vite's bundled `http-proxy`
path still calls the deprecated `util._extend` helper. TermAl hits that path because
`ui/vite.config.ts` configures `server.proxy` for `/api` and `/api/events`.

**Current behavior:**
- `npm run dev` can print `(node:...) [DEP0060] DeprecationWarning: The util._extend API is deprecated`
- the warning comes from Vite's dev proxy implementation, not from TermAl application code
- `npm run build` and `npm run test` still pass, so this does not block production output

**Proposal:**
- upgrade the frontend dev toolchain together instead of patching `node_modules`
- include at least `vite`, `@vitejs/plugin-react`, and `vitest` in the same refresh
- verify the dev proxy path after the upgrade on current Node, since the warning is tied to the
  proxy code path rather than to React or app logic
- do not spend time replacing the proxy configuration in TermAl just to hide the warning

**Temporary stance:** Until the toolchain refresh is scheduled, treat this as expected local dev
noise.

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

## Codex app-server and HTTP route coverage is still partial

**Severity:** Medium

There are unit tests for Claude parsing, for the legacy `handle_codex_event()` path, and for a
small subset of the newer Codex app-server notifications. Coverage is still thin for
`handle_codex_app_server_message()` request/item parsing, and there are still no HTTP route tests
for the axum handlers.

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
- Streamed text and command updates now have a dedicated SSE delta path instead of forcing a full
  state snapshot for every chunk
- Active long conversations now use a windowed message list instead of mounting every message card
- Heavy markdown and code blocks now defer their expensive render work until near the viewport
- Cached conversation pages per pane are now bounded so hidden long tabs do not grow without limit

**What still happens:**
- The mount path still starts `EventSource("/api/events")` and `fetchState()` in parallel, so a
  late `/api/state` response can overwrite newer delta-applied UI state
- The UI currently drops delta events when the target message is missing, so once the state drifts
  there is no immediate self-heal until a later full snapshot arrives
- Full-state adoption still reruns the broader session reconciliation path whenever a state event
  does arrive, so concurrent activity can still make typing feel slower than it should

**Tasks:**
- Profile frontend rerenders during active streaming to identify the remaining hot subtrees

## Delta SSE reconciliation can drift behind live session state

**Severity:** Medium

The new delta stream reduces the amount of work done during live output, but it also introduced a
correctness risk during initial load and reconnects.

**Current behavior:**
- The frontend opens `/api/events` and fetches `/api/state` at the same time
- If a delta event is applied first, a slower `/api/state` response can still replace the newer
  in-memory session tree
- Later deltas are ignored when the referenced message is not present in the overwritten state
- The backend paths for streamed text and command updates now publish deltas without also emitting a
  matching full state snapshot for each update

**Impact:**
- In-flight assistant output can temporarily disappear on first load or reconnect
- Once the UI loses the target message for a delta, subsequent deltas for that message are dropped
- Users only recover when some later full snapshot happens to realign the session tree

**Fix:**
- Make initial hydration and SSE ordering explicit: either ignore stale `/api/state` payloads or
  add a monotonic revision so older state cannot overwrite newer deltas
- Keep delta reconciliation lossless when a message is missing by falling back to a full refresh or
  by buffering deltas until the message exists

**Likely change locations:**
- Frontend minimum: `ui/src/App.tsx`
  `adoptState()` must stop blindly replacing newer in-memory state, the startup `useEffect()` must
  stop racing `fetchState()` against live SSE updates, and `applyDelta()` must recover when the
  target message is missing instead of silently dropping the update
- Backend hardening: `src/main.rs`
  add a monotonic revision to `StateResponse` and `DeltaEvent`, publish it from `/api/events`, and
  stamp it on streamed text and command delta events so the frontend can reject stale state

## Command delta inserts lose timestamps

**Severity:** Low â€” user-visible metadata regression.

The new `commandUpdate` delta event can create command messages in the UI before any later full
snapshot arrives, but the delta payload does not include a timestamp.

**Current behavior:**
- The backend creates a real timestamp when it first inserts a command message
- The delta payload only sends command text, output, language metadata, status, and preview
- When the frontend receives the first `commandUpdate` for a message it has not seen before, it
  creates the card with an empty timestamp string

**Impact:**
- Freshly inserted command cards can render blank message metadata
- If no later full state event arrives soon after, the blank timestamp persists indefinitely

**Fix:**
- Include `timestamp` in the `commandUpdate` payload, or keep emitting a full state update when a
  command message is first inserted
- Add a frontend regression test for first-seen command deltas

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

- [ ] Lock down `/api/file`:
  resolve relative paths against the requesting session's `workdir`, reject reads outside allowed
  roots, and stop treating arbitrary absolute paths as valid source-view targets.
- [ ] Fix approval lifecycle bookkeeping:
  canceled Claude approvals should clear the session out of `Approval`, and resolving one approval
  must not hide other live approvals in the same session.
- [ ] Remove the startup snapshot race:
  avoid letting a late `/api/state` response overwrite newer `/api/events` deltas, or add a
  revision field so stale state can be ignored before it wipes streamed messages from the UI.
  Frontend touch points: `ui/src/App.tsx` in `adoptState()` and the startup `useEffect()`.
  Backend hardening if needed: add revision metadata in `src/main.rs` `StateResponse` and
  `DeltaEvent`.
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
- [ ] Align attachment UX with actual capabilities:
  show the right composer hint per agent, add drag-and-drop, and keep the docs in sync with the
  implementation.
- [ ] Reduce streaming refresh overhead:
  profile SSE-driven rerenders while another session is active, narrow state adoption for
  unrelated sessions, and only consider incremental events after the frontend hot path is trimmed.
- [ ] Fix lossless delta reconciliation:
  make the delta SSE path recover when the UI is missing the referenced message, and add frontend
  coverage for the interaction between initial `/api/state` hydration and live `/api/events`
  updates. Primary change point: `ui/src/App.tsx` `applyDelta()`.
- [ ] Preserve command timestamps in the delta path:
  include `timestamp` in first-seen `commandUpdate` payloads or force a full state refresh on
  insert so command cards do not render blank metadata.
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
- [ ] Refresh the frontend dev toolchain to remove the Node 24 `util._extend` deprecation from
  Vite's proxy path; upgrade `vite`, `@vitejs/plugin-react`, and `vitest` together and verify
  `npm run dev` with the existing `/api` proxy config.
- [ ] Add unit tests for Codex app-server parsing:
  cover request handling, streaming message assembly, notification filtering, and error paths.
- [ ] Add HTTP route tests for the axum API:
  session creation, message send, settings updates, approvals, kill, and SSE state events.
- [ ] Refresh `docs/claude-pair-spec.md` so the architecture and milestone tracking match the
  current axum + React implementation.
- [ ] Split `src/main.rs` into focused modules once the feature work above stops churning large
  integration surfaces.

## Later
