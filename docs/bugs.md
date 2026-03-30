# Bugs & Known Issues

Updated against the current checked-in code in `src/main.rs`, `ui/src/App.tsx`,
`ui/package.json`, and `ui/vite.config.ts`.

The older entries for "No image paste support", the image-attachment UX mismatch, Claude `control_request` fallthrough, "No SSE/WebSocket for real-time updates", "Codex receive has no streaming", "No queueing system for prompts", the stale `/api/state`-after-SSE bootstrap race, the false-positive delta SSE reconciliation drift when a message already existed locally, shared Codex turn-scoped subagent ordering, shared Codex `item_completed` multipart truncation, stale shared-Codex agent-event turn filtering, the shared Codex buffered-result flush edge, multiple simultaneous approvals, Windows `HOME`-only path resolution, process-exit polling, unhandled Codex rate-limit notifications, the stale "Backend persists full state on every streaming text delta" entry, the old unscoped `/api/file` read bug, the legacy Codex REPL `exec --json` path, the old per-session / partial Codex app-server notes, the old Codex session-discovery note, the startup Codex home-discovery mismatch, the REPL Codex import leak, the discovery truncation bug, the startup discovery settings-clobber bug, the shared Codex lock-order deadlock, the Codex DB fallback-scan abort, the unbounded generic Codex app-request payload path, the MCP elicitation schema-enforcement gap, the uncapped Codex discovery query, the old interaction-request state name, the stale command-delta timestamp note, agent replies in diff review comments, the spec-drift note for `docs/claude-pair-spec.md`, the Claude hidden-session pool note, the old agent-native slash-command note, the stale runtime `try_wait()` polling note, the stale recorder-duplication note, the SSH remote-host injection bug, the remote review scope-routing bug, the remote bridge lifecycle bug, the remote ghost-session snapshot bug, the Remotes draft-reset bug, the workspace-first remote create-session bug, the remote-toggle copy mismatch, the stale Claude approval-cancel item, the `..` git pathspec validation gap, the failed Claude Task detail-loss bug, the stale `insert_message_before` contract note, the stale Claude parallel-agent type-duplication note, the streaming-refresh UI performance entry (render callback stabilization, mountedSessions referential stability, box-shadow transition scoping, MeasuredMessageCard memoization, sameJsonValue rewrite), and the stale architecture-doc API table note were stale. Those are implemented in the current tree.

The newer shared Codex regressions where stale `task_complete` summaries could bleed into the next
answer or a pre-answer summary insert could overwrite the final-answer preview are also fixed in
the current tree. Parallel-agent progress updates now also use the targeted delta SSE path instead
of forcing full-state snapshots.

The earlier command-card UX issue where `OUT` could render as an empty dark block was also fixed.
Command messages now use a compact `IN` / `OUT` layout with copy controls, a collapsible output
view for longer results, and a plain placeholder when there is no command output.

The stale `reconcileSession` `projectId` note, the `clear_runtime()` revision note, the file
read/write size-limit note, the missing CORS note, the remote-delta diagnostic note, the duplicate
project-text normalizer note, the `should_dispatch_next` consistency note, the Codex completed-text
reconciliation note, and the `TextReplace` architecture-doc note are also fixed in the current tree.

The shared Codex kill-isolation note, the committed-kill response-contract note, and the stale
killed-thread ignore-list growth note are also fixed in the current tree. Shared-session kill
failures now detach locally instead of tearing down the shared Codex runtime, kill now returns the
committed authoritative state while post-removal cleanup warnings are logged server-side, and
discovery import prunes ignored Codex thread IDs that no longer exist.

The non-Codex external-session tombstone leak, the shared-Codex `stop_session` detach gap, the
shared-Codex post-interrupt cleanup gap, and the tooltip vs. session-panel model-option trim
divergence are also fixed in the current tree. Codex tombstone clearing is now scoped to Codex
sessions only, `stop_session` now always detaches shared Codex session bookkeeping after an
interrupt attempt and continues through runtime cleanup/stop-message recording even if that
interrupt fails, and tooltip/panel model-option matching now uses one shared lookup utility.

The CORS `allow_methods` missing DELETE and the orchestrator card grab handle missing keyboard
support are also fixed in the current tree. DELETE is now included in the CORS method list, and
the card grab handle responds to Enter and Space key presses.

The newer orchestration runtime issues around entry-session instruction injection, transition
handoff durability, per-turn result extraction, template-read serialization during instance
creation, kill cleanup, and agent readiness validation are also fixed in the current tree.
Runtime orchestration sessions now inject template instructions on their first prompt whether it
comes from a user kickoff or a transition, transition handoff now goes through a persist-first
queued prompt before any runtime send, transition result extraction is scoped to the current
turn, template loading is serialized under the state mutex during instance creation, kill cleanup
immediately normalizes owning orchestrator instances, and orchestrator-spawned sessions now
validate local agent readiness before they are created.

The follow-up orchestration regressions around duplicate replay after dispatch, silent stalled
pending transitions, and resume-loop starvation are also fixed in the current tree. Failed
transition dispatch now becomes a visible destination-session error instead of leaving hidden
pending work behind, runtime completion handlers log orchestration-finalization failures, and one
instance's failed dispatch no longer prevents other orchestrators from draining their own pending
work.

The missing axum coverage for orchestrator instance routes is also fixed in the current tree.
`GET /api/orchestrators`, `POST /api/orchestrators`, and `GET /api/orchestrators/{id}` now have
route-level tests in addition to the direct state-level orchestration tests.

The settings-tab selection-frame padding issue is also fixed in the current tree.

The frontend workspace-layout `controlPanelSide` typing note is also fixed in the current tree. The
workspace layout API types now narrow `controlPanelSide` to `"left" | "right"` in `ui/src/api.ts`,
matching the backend enum contract.

The `ApprovalDecision::Pending` panic and the cyclic transition graph resource exhaustion are also
fixed in the current tree. `Pending` is now rejected by the early guard alongside `Interrupted`
and `Canceled`, and template validation runs a DFS-based cycle detection that rejects non-DAG
transition graphs.

The orchestrator handoff restart recovery gap is also fixed in the current tree. On startup,
`dispatch_orphaned_queued_prompts` scans for idle sessions with queued prompts and no active
runtime, and auto-dispatches them. This covers the window where a transition was committed
(queued prompt persisted, pending transition removed) but the process crashed before
`dispatch_next_queued_turn` ran. A backend regression now covers that exact restart window.

The orchestration stop/error fan-out bug, the shared workspace-tab validator drift, the
file-not-found 400/404 mismatch, and the `stop_session` cleanup-contract asymmetry are also
fixed in the current tree.

---

## `stop_session` treats failed dedicated-runtime kills as successful stops

**Severity:** High - the UI can report a session stopped while the agent process is still running,
and queued follow-up work can be dispatched on the same session.

`stop_session` now routes all runtime shutdown failures through `shutdown_removed_runtime(...)`, logs
any error, and then continues clearing the runtime, marking the session idle, and dispatching the
next queued prompt. For dedicated Claude/Codex/ACP runtimes, `kill_child_process()` only returns an
error after the child is still alive even after a kill attempt. In that case the UI says the turn
stopped, but the old agent process can continue running out of band.

That is materially different from the old shared-Codex detach warning path. A failed dedicated kill
means TermAl no longer owns the session state for a process that may still be executing tools or
editing files, and a queued prompt can be launched as if the stop had succeeded.

**Current behavior:**
- `stop_session` logs dedicated runtime kill failures as cleanup warnings and still returns HTTP 200
- the session runtime is cleared and the session is marked `Idle` even when the child process did
  not stop
- queued prompts can dispatch immediately after the failed cleanup path

**Proposal:**
- keep dedicated-runtime kill failures as hard stop failures instead of treating them as best-effort
- reserve the warn-and-continue behavior for shared-runtime detach paths that intentionally do not
  own process lifetime the same way
- add a regression that covers queued-prompt behavior when a dedicated runtime fails to stop

## Orchestrator start only adopts sessions from the returned `StateResponse`

**Severity:** Medium - starting an orchestration can leave frontend revision tracking stale and
force unnecessary resyncs on the next SSE update.

`createOrchestratorInstance()` returns a full `StateResponse`, and `OrchestratorTemplatesPanel`
forwards that snapshot through `onStateUpdated`. However, `App` handles the callback by calling
`adoptSessions(state.sessions)` instead of `adoptState(state)`. The newly-created sessions appear,
but the frontend never records the returned revision or any other non-session fields that changed
with the same snapshot.

That leaves `latestStateRevisionRef` behind the backend and makes the next delta look like a gap.
At best that triggers an avoidable resync; at worst any non-session metadata in the returned state
stays stale until some later full snapshot arrives.

**Current behavior:**
- starting an orchestration only replaces the in-memory session list
- the returned `revision`, `projects`, readiness metadata, and other `StateResponse` fields are not
  adopted through the normal snapshot path
- the next SSE delta can appear to skip a revision even though the client already received the
  state-changing response

**Proposal:**
- route orchestrator start responses through `adoptState(response.state)` instead of `adoptSessions`
- add an App-level regression that verifies orchestrator starts advance revision tracking
- cover any non-session metadata returned with the same response so the callback stays aligned with
  the rest of the snapshot adoption flow

## Duplicated `CONTROL_SURFACE_TAB_KINDS` constant across App.tsx and workspace.ts

**Severity:** Medium - maintenance risk; adding a new control-surface kind requires updating both
locations, and a missed update causes forward-sync and reverse-sync to disagree.

`CONTROL_SURFACE_TAB_KINDS` in `App.tsx` and `CONTROL_SURFACE_KINDS` in `workspace.ts` contain the
same six tab kinds. The canonical definition of what constitutes a "control surface" should live in
one place.

**Current behavior:**
- two identical `ReadonlySet<string>` constants exist in separate files
- adding a new control-surface kind (e.g., an orchestrator canvas) requires updating both

**Proposal:**
- export `CONTROL_SURFACE_KINDS` from `workspace.ts` and import it in `App.tsx`
- remove the local `CONTROL_SURFACE_TAB_KINDS` declaration

## Forward/reverse pane sync can both fire for the same control-surface tab

**Severity:** Medium - confusing double-set of `selectedProjectId` with potentially different values.

When a control-surface tab (e.g., `gitStatus`) is selected, the forward-sync block in
`handlePaneTabSelect` may call `setSelectedProjectId` via `resolveWorkspaceTabProjectId`, and then
the reverse-sync block also fires (because the tab kind is in `CONTROL_SURFACE_TAB_KINDS`) and may
call `setSelectedProjectId` again with a different project derived from the nearest session. React 18
batching means only the last value wins, but the intent is unclear and the overlapping early-return
paths are fragile.

**Current behavior:**
- both forward-sync and reverse-sync blocks can execute for the same tab selection
- `selectedProjectId` may be set twice with different values in the same handler
- the interaction between the two blocks is hard to reason about

**Proposal:**
- restructure into two mutually exclusive branches: if the selected tab is a control surface, run
  only reverse-sync; otherwise, run only forward-sync
- use a single discriminator at the top of the handler to clarify the control flow

## Drag-over `text/plain` MIME fallback triggers false drop indicators

**Severity:** Medium - visual confusion, no data corruption.

The `handleTabRailDragOver` handler in `PaneTabs.tsx` and the pane body `onDragOver` handler in
`App.tsx` fall back to checking `dataTransfer.types.includes("text/plain")` when the custom MIME
type is absent. This means any drag that carries `text/plain` data (selected text, browser
bookmarks, OS file drops) will show drop indicators on the tab rail and pane body. The actual
drop is safe - `readWorkspaceTabDragData` validates - but the visual affordance is misleading.

**Current behavior:**
- dragging selected text over a tab rail or pane body shows a drop indicator
- dropping non-tab content does nothing (correctly rejected by the drop handler)

**Proposal:**
- tighten the `hasTabDragType` guard to also check `launcherDraggedTabRef.current` is non-null
  before accepting the `text/plain` fallback
- consider setting a second custom MIME type (e.g. `application/x-termal-tab`) as a lightweight
  flag that avoids the `text/plain` ambiguity

## Workspace docs lag the server-backed layout implementation

**Severity:** Note - documentation drift only, but it obscures the real API and persistence model.

The new multi-browser workspace implementation is now in the codebase, but the docs are not fully
aligned with what shipped. `docs/features/multi-browser-workspaces.md` still says Phase 1 only
needs `GET /api/workspaces/{id}` and `PUT /api/workspaces/{id}`, while the implementation also
adds `GET /api/workspaces` to drive the workspace switcher. `docs/architecture.md` still says the
workspace layout is local-only and not persisted to the backend.

**Current behavior:**
- the feature brief understates the implemented workspace API surface
- the architecture doc still describes workspace layout as browser-local state
- readers cannot rely on the docs to understand the current server-backed workspace model

**Proposal:**
- update `docs/features/multi-browser-workspaces.md` to include the list route and switcher-driven
  flow
- update `docs/architecture.md` so the persistence section and API table reflect server-backed
  workspace layouts

## Switching workspaces in the current tab can drop the last debounced server save

**Severity:** Medium - the latest layout can be persisted only to browser-local storage and never
reach the server-backed workspace record.

Workspace switching now persists layouts to the backend with a debounced `PUT /api/workspaces/{id}`
write, but `navigateToWorkspace()` immediately calls `window.location.assign(...)` for same-tab
navigation. The save effect cleanup clears any pending timeout on unmount, so a quick workspace
switch can cancel the last pending backend write even though the layout already changed locally.

That means the current browser can often recover from `localStorage`, but other browsers, fresh
profiles, and any later server-driven bootstrap can still see a stale workspace layout because the
authoritative backend copy never received the final update.

**Current behavior:**
- same-tab workspace navigation can happen before the debounced server save fires
- unmount cleanup clears the pending persistence timer instead of flushing it
- the latest layout may exist only in browser-local storage while the backend still serves an
  older workspace snapshot

**Proposal:**
- flush pending workspace saves before same-tab navigation or persist them during `pagehide`
- consider `navigator.sendBeacon` or an explicit synchronous handoff path for the final save
- add a regression that changes a layout, switches workspaces immediately, and asserts the backend
  still receives the last `PUT /api/workspaces/{id}`

## Dropping a session on the tab rail ignores the hovered insertion index

**Severity:** Low - tab re-targeting works, but the visual insertion affordance is inaccurate.

`placeSessionDropInWorkspaceState()` accepts a `tabIndex`, and the tab rail computes hovered drop
positions, but the `placement === "tabs"` path forwards session drops through
`openSessionInWorkspaceState(...)`, which does not take the insertion index. As a result, the drop
indicator can show a specific slot while the actual session placement falls back to the existing
open/focus behavior.

**Current behavior:**
- dropping a session onto a highlighted tab position does not honor the computed insertion index
- existing sessions are focused and new sessions are appended/opened using the default path
- the UI can preview one tab position while committing another

**Proposal:**
- thread the `tabIndex` through the session-drop tab-placement path
- route tab-rail session drops through the insertion-aware tab open helper instead of the generic
  open/focus path


## Markdown localhost file-link normalization overmatches same-origin web URLs

**Severity:** Low - legitimate app-origin web links with file-like paths can be rewritten as local
source links.

The new markdown helper normalizes any URL whose `origin` matches `window.location.origin` before
checking whether the pathname "looks like" a file path. That is broad enough to catch ordinary
same-origin documentation or asset URLs such as `/docs/architecture.md` or `/assets/logo.svg`,
because they also end in dotted path segments.

The localhost Windows-path case from the Questly/Supabase link is valid and should stay clickable,
but same-origin web routes should not be reinterpreted as source-file links just because they have
an extension.

**Current behavior:**
- same-origin HTTP URLs with file-like pathnames are treated as non-external by the markdown link
  helper
- raw same-origin URL text can be collapsed into a workspace-style file label even when the target
  is ordinary web content
- current tests cover localhost file URLs, but not same-origin docs/assets URLs that should remain
  normal anchors

**Proposal:**
- restrict the normalization to loopback/file-shaped URLs instead of all same-origin file-like paths
- require an explicit absolute filesystem path shape before converting an HTTP URL into a source
  link target
- add regression coverage for both localhost file URLs and ordinary same-origin web URLs

## Orchestrator template `Run` button treats unknown project IDs as local

**Severity:** Low - stale template project IDs can expose a runnable action that only fails after
submission.

`OrchestratorTemplatesPanel` derives `selectedProjectIsLocal` by calling
`isLocalRemoteId(selectedProject?.remoteId)`. Because `isLocalRemoteId(undefined)` returns `true`,
a template draft whose `projectId` is set but missing from the loaded project list is treated like
a runnable local project. That can happen with persisted draft state or templates that still
reference a deleted project.

The backend still rejects the launch because `create_orchestrator_instance` requires the project ID
to resolve to a real local project. The result is an enabled `Run` button that produces a backend
error instead of being blocked in the UI.

**Current behavior:**
- a draft/template with an unknown `projectId` can enable `Run`
- clicking `Run` sends `POST /api/orchestrators` and fails with an avoidable backend validation
  error
- current panel tests cover template CRUD/persistence/validation, but not stale-project `Run`
  behavior

**Proposal:**
- only enable `Run` when `draft.projectId` resolves to a known `Project`
- treat missing project metadata as non-runnable instead of implicitly local
- add frontend tests for stale/unknown project IDs and `Run`-button error/disable states

## Feature briefs
- [Project-Scoped Remotes](./features/project-scoped-remotes.md)
- [Session Model Switching](./features/model-switching.md)
- [Slash Commands](./features/slash-commands.md)
- [Gemini CLI Integration](./features/gemini-cli-integration.md)
- [Diff Review Workflow](./features/diff-review-workflow.md)
- [Territory Visualization](./features/territory-visualization.md)
- [Agent Integration Comparison](./features/agent-integration-comparison.md)
- [Multi-Browser Workspaces](./features/multi-browser-workspaces.md)

# Backlog

## Backend module sizes are growing

**Severity:** Medium - maintainability concern, not a runtime bug.

The backend was split from a single `src/main.rs` into focused modules (`api.rs`, `state.rs`,
`runtime.rs`, `turns.rs`, `remote.rs`, `tests.rs`), but several of those modules are already
large. `src/state.rs` and `src/turns.rs` each carry substantial logic that could benefit from
further decomposition as features stabilize.

## Session model controls still need polish

**Severity:** Medium - detailed brief:
- [Session Model Switching](./features/model-switching.md)

Session-scoped model switching is implemented for Claude, Codex, Cursor, and
Gemini. The remaining work is polish: richer capability metadata, stronger
refresh recovery, and deeper end-to-end coverage.

## Gemini ACP integration still needs hardening

**Severity:** Medium - detailed brief:
- [Gemini CLI Integration](./features/gemini-cli-integration.md)

Gemini is implemented as a first-class ACP-backed agent now. The remaining work
is hardening: clearer auth/setup recovery, broader ACP protocol coverage, and
more end-to-end testing around model refresh and approval-mode changes.

## No territory visualization

**Severity:** High - detailed brief:
- [Territory Visualization](./features/territory-visualization.md)

- Add click-through navigation from territory entries to the originating conversation message
- Add a persistent territory summary bar visible across all tabs
- Optionally overlay territory indicators in the source view and diff preview tabs

---

# Implementation Tasks

Concrete work implied by the current TermAl parity gaps. Ordered by user impact and dependency.

## P0

- [ ] Add territory visualization:
  track per-session file read/write activity in backend state, expose it through `/api/territory`,
  render a project-tree view color-coded by agent with recency decay, heatmap mode, conflict
  warnings, click-through to originating messages, and a persistent summary bar. This is the core
  coordination surface that makes concurrent agent workflows safe and manageable.

## P1

- [ ] Polish session model controls:
  keep the current session-scoped model switching, but continue improving live
  metadata, validation, recovery flows, and create/clone defaults so the model
  UX feels intentional across Claude, Codex, Cursor, and Gemini.
- [ ] Add drag-and-drop image attachments:
  pasted image attachments are documented in the composer now, but drag-and-drop is still missing.
- [ ] Debounce delta persistence:
  stop writing the full state to disk on every streaming text chunk. Accumulate deltas in memory
  and flush periodically (e.g. 500 ms) or on turn completion. Index messages by ID for O(1)
  lookup. Cache `collect_agent_readiness` instead of re-scanning PATH on every snapshot. Move
  disk I/O outside the mutex lock.

## P2

- [ ] Expand HTTP route tests for the axum API:
  Codex thread actions (archive, unarchive, fork, rollback) and interactive request submissions
  (user input, MCP elicitation, generic app requests) now have HTTP route tests via
  `tower::ServiceExt`. Still missing: session creation, message send, settings updates, Claude
  approvals, and SSE state events.
- [ ] Add backend regression tests for divergent completed-text replacement in Codex streaming:
  cover both shared-Codex and REPL-Codex paths where the final authoritative text must replace,
  not append to, previously streamed content.
- [ ] Add a `stop_session` test with a queued prompt pending when a shared Codex interrupt fails:
  verify that `dispatch_next_queued_turn` fires and the queued prompt is dispatched after the
  best-effort cleanup completes.
- [ ] Add a `stop_session` regression for failed dedicated-runtime kills:
  cover Claude/Codex/ACP kill failures where the child process is still alive and assert the
  session is not treated as cleanly stopped or allowed to dispatch queued follow-up work.
- [ ] Add frontend reconcile tests for new interactive message types:
  `userInputRequest`, `mcpElicitationRequest`, and `codexAppRequest` messages are handled by the
  reconciler but have no test coverage verifying that state changes (e.g. pending ? submitted)
  correctly produce new message references.
- [ ] Add `SessionCanvasPanel.test.tsx` afterEach cleanup:
  other test files (`PaneTabs.test.tsx`, `App.test.tsx`) explicitly call `cleanup()` in
  `afterEach`; this file should match the project convention.
- [ ] Add PaneTabs test for "Git Sync" context menu action:
  only the "Git Push" path is exercised; add a test that clicks "Git Sync" and verifies
  `syncGitChanges` is called with the expected workdir.
- [ ] Add PaneTabs test for git status fetch failure in context menu:
  the `statusError` / `statusMessage` state fields exist but the error rendering path when
  `fetchGitStatus` rejects is uncovered.
- [ ] Add OrchestratorTemplatesPanel tests for update, delete, and run flows:
  mocks for `updateOrchestratorTemplate` and `deleteOrchestratorTemplate` are set up but no test
  exercises them. Also cover `createOrchestratorInstance`, `onStateUpdated`, and stale/unknown
  project handling for the `Run` action.
- [ ] Add an App-level orchestrator-start adoption regression:
  verify that starting an orchestration adopts the full returned `StateResponse` revision and other
  metadata instead of only replacing the session list.
- [ ] Add MarkdownContent regression coverage for localhost file URLs vs. same-origin web links:
  keep `http://127.0.0.1/.../C:/...#L15C1` links opening source files, but ensure ordinary
  same-origin docs/assets URLs remain normal anchors.
- [ ] Add OrchestratorTemplateLibraryPanel tests for fetch error and event-driven re-fetch:
  the error branch (`getErrorMessage`) and the `ORCHESTRATOR_TEMPLATES_CHANGED_EVENT` re-load
  path have no test coverage.
- [ ] Add backend orchestrator validation tests for self-loop, duplicate ID, and empty-sessions:
  `normalize_orchestrator_template_draft` rejects self-referencing transitions, duplicate
  session/transition IDs, and drafts with zero sessions. Unknown-target and cyclic-transition
  coverage now exists; keep filling in the remaining validation cases.
- [ ] Add backend tests for template-level orchestrator project fallback:
  current `create_orchestrator_instance` coverage still passes `projectId` explicitly in the
  request. Add state/route tests where the template supplies `projectId` and the request omits it.
- [ ] Add orchestrator lifecycle endpoints (stop, pause, resume):
  `OrchestratorInstanceStatus` defines `Running`, `Paused`, and `Stopped` but no API endpoint
  transitions between them. Users cannot stop a running orchestration except by killing
  individual sessions.
- [ ] Add orchestrator instances to `StateResponse` or a dedicated SSE delta:
  the frontend currently has no push notification for orchestrator state changes (transitions
  fired, instances completed). It must poll `GET /api/orchestrators`.
- [ ] Add session/transition count limits to orchestrator template validation:
  `normalize_orchestrator_template_draft` has no upper bounds on `sessions.len()` or
  `transitions.len()`. Cap at ~50 sessions and ~200 transitions.
- [ ] Add unit tests for `rescopeControlSurfacePane`:
  cover gitStatus (workdir update), filesystem (rootPath update), controlPanel-like (origin-only
  update), pane-not-found no-op, and no-active-tab no-op branches in `workspace.test.ts`.
- [ ] Add unit tests for `findNearestSessionPaneId`:
  cover left-preference when sessions exist on both sides, right-only fallback, no session panes
  returning null, and paneId not in workspace returning null.
- [ ] Add `openDiffPreviewInWorkspaceState` test for control-surface anchor redirect:
  the existing test covers the docked controlPanel case; add a test where the preferred pane is a
  standalone gitStatus or filesystem pane and verify the diff opens adjacent to the session pane.
- [ ] Add App-level regression coverage for control-surface tab selection sync:
  render a docked control panel plus standalone git/files panes with neighboring sessions, then
  verify selecting the control surface adopts the nearest session context instead of leaving stale
  origin-based project state behind.
- [ ] Add unit tests for `ensureWorkspaceViewId` and `createWorkspaceViewId`:
  `workspace-storage.ts` exports these functions for URL-param handling and workspace ID
  generation. A round-trip test in jsdom would guard against regressions in workspace ID
  normalization or URL-param handling.
- [ ] Add sort-order assertion to `list_workspace_layouts` backend test:
  `list_workspace_layouts_route_returns_saved_workspaces` checks presence but not the documented
  `updated_at` descending sort order. Assert index positions after inserting workspaces with
  distinct timestamps.
- [ ] Add workspace-navigation regression coverage for debounced server layout saves:
  change a workspace layout, switch to another workspace in the current tab before the debounce
  timer fires, and assert the last layout still reaches `PUT /api/workspaces/{id}`.
- [ ] Add a tab-rail session-drop insertion-order regression:
  cover `placeSessionDropInWorkspaceState` / App-level tab drops so a session dropped at a
  specific tab index is inserted at that position instead of always falling back to append/focus.
- [ ] Extract shared drag-drop test setup helper:
  the two drag-drop tests in `App.test.tsx` duplicate ~60 lines of identical fetch-mock and
  setup boilerplate. Extract a shared `renderAppWithProjectAndSession()` helper.
- [ ] Add unit tests for orchestrator geometry functions:
  `anchorPosition`, `nearestAnchorSide`, `nearestAnchorPosition`, `buildTransitionGeometry`,
  and `isValidAnchor` are pure deterministic functions with no DOM dependencies.
- [ ] Continue splitting backend modules as they grow:
  `src/main.rs` was split into `api.rs`, `state.rs`, `runtime.rs`, `turns.rs`, `remote.rs`, and
  `tests.rs`. Some of these modules (especially `state.rs` and `turns.rs`) are already large and
  could benefit from further decomposition as features stabilize.

## Later
