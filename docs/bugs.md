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

The `ApprovalDecision::Pending` panic, the cyclic transition graph resource exhaustion, the
unbounded orchestrator template graph size, and the frontend/client-side template size drift are
also fixed in the current tree. `Pending` is now rejected by the early guard alongside
`Interrupted` and `Canceled`, template validation runs a DFS-based cycle detection that rejects
non-DAG transition graphs while also capping templates at 50 sessions and 200 transitions, and
`OrchestratorTemplatesPanel` now mirrors those limits locally while disabling the "Add session"
affordance at the cap.

The orchestrator handoff restart recovery gap is also fixed in the current tree. On startup,
`dispatch_orphaned_queued_prompts` scans for idle sessions with queued prompts and no active
runtime, and auto-dispatches them. This covers the window where a transition was committed
(queued prompt persisted, pending transition removed) but the process crashed before
`dispatch_next_queued_turn` ran. A backend regression now covers that exact restart window.

The orchestration stop/error fan-out bug, the shared workspace-tab validator drift, the
file-not-found 400/404 mismatch, and the `stop_session` cleanup-contract asymmetry are also
fixed in the current tree.

The `stop_session` dedicated-runtime kill-failure, the orchestrator-start `adoptSessions` vs
`adoptState` gap, the duplicated `CONTROL_SURFACE_TAB_KINDS` constant, the control-surface
forward/reverse pane sync double-fire, the pane-local Files/Git global root/workdir mismatch,
the canvas stale origin metadata on move, the `text/plain` MIME drag false-drop affordances, the
workspace docs lag, the debounced server save drop on navigation, the Sessions-tab reuse contextual
targeting gap, the markdown same-origin overmatch, and the orchestrator `Run` button stale project
handling are also fixed in the current tree. Failed dedicated-runtime kills now roll back session
state and return an error instead of treating the stop as clean, orchestrator start now routes the
returned `StateResponse` through `adoptState` to keep revision tracking in sync, `CONTROL_SURFACE_KINDS`
is now exported from `workspace.ts` as the single canonical set, control-surface tab selection now
runs before and exclusive of session-tab sync, Files/Git panels now derive their root/workdir from
the same pane-local session/project context as their launchers, canvas and sessionList tabs now
refresh their origin metadata when moved to a new pane context, `PaneTabs.tsx` now only accepts
the explicit custom MIME type to guard the drop affordance, workspace saves are flushed with
`keepalive` on `pagehide` and before navigation, the Sessions tab is now moved to the contextual
pane on reuse, `isMarkdownLocalFileUrl` was narrowed to loopback-only and gated behind an
explicit Windows-path shape guard, and the orchestrator `Run` button now blocks on missing or
stale projects. Two TypeScript compile errors introduced in the same batch (spurious `keepalive`
fields on API functions lacking an `options` parameter, and out-of-scope drag refs in
`SessionPaneView`) were also caught and fixed.

The `openOrchestratorListInWorkspaceState` stale origin-metadata gap on tab reuse, the `pagehide`
stale-closure fragility, the Windows-only `looksLikeAbsoluteHttpMarkdownFilePath` regex, and the
process-global kill-failure test hook are also fixed in the current tree. The orchestrator list
tab now refreshes its `originSessionId`/`originProjectId` and moves to the contextual pane on reuse
(matching the canvas and Sessions singleton patterns); the `pagehide` effect now calls
`flushWorkspaceLayoutSaveRef.current` so the empty dep array is definitively safe regardless of
helper internals; `looksLikeAbsoluteHttpMarkdownFilePath` now also matches Unix absolute paths via
a top-level directory allowlist; and the kill-failure injection hook is now scoped to a specific
`Arc<SharedChild>` by pointer rather than a global label string. `stop_session` now keeps the
previous session status and preview visible while shutdown is pending, guarded by an internal
`runtime_stop_in_progress` flag so intentional runtime exits are not misclassified as failures.
Launcher and cross-window tab drags also share one guarded known-drag fallback across the pane
body and tab rail, which restores reduced-MIME browser behavior without reintroducing arbitrary
`text/plain` false positives. Stop-in-progress sessions now also suppress matching runtime
failure, retry, error, exit, completion, and Codex thread-state callbacks until the stop
finishes, and backend regressions cover the double-stop conflict, queued-prompt suppression,
and the stop-time callback suppression contracts.

---

## Failed dedicated stop attempts can drop runtime callbacks during the suppression window

**Severity:** Medium - a dedicated session can stay visibly Active with stale preview/messages if the runtime emits terminal callbacks while a failed stop attempt is in flight.

`stop_session()` now sets `runtime_stop_in_progress` before calling `shutdown_removed_runtime(...)`, and the runtime callback handlers early-return while that flag is set. If a dedicated Claude/Codex/ACP stop attempt ultimately fails and returns an error, the flag is cleared and the runtime is left attached, but any `finish_turn_ok`, `mark_turn_error`, retry, exit, or thread-state callbacks that arrived during the failed shutdown window have already been discarded.

That means a rare dedicated stop failure can now hide real runtime state transitions. Inference from the code path: if the agent exits or completes naturally while the stop attempt is timing out, the user can be left with a session that still looks Active even though the terminal callback already came and was ignored.

**Current behavior:**
- `stop_session()` sets `runtime_stop_in_progress` before attempting dedicated runtime shutdown
- matching runtime callbacks are suppressed while that flag is true
- if the dedicated stop returns an error, the runtime stays attached but suppressed callbacks are not replayed

**Proposal:**
- preserve or replay terminal runtime callbacks when a dedicated stop attempt fails
- or narrow suppression so failed-stop paths still honor completion/error/exit callbacks
- add a regression that forces a dedicated stop failure while a matching runtime callback arrives during the shutdown window

## Tab-rail session drops still ignore insertion index for already-open sessions

**Severity:** Low - dragging an already-open session onto a tab rail still repositions it to the pane default instead of the hovered slot.

`placeSessionDropInWorkspaceState()` now threads `tabIndex` through the new-session path, but the already-open-session path still delegates to `openSessionInWorkspaceState()`, which only moves or activates the existing tab and does not accept an insertion index. So the visible insertion affordance is now accurate for closed sessions, but not for reordering an already-open session into a specific rail slot.

The current workspace test only covers the newly-opened session case, so this remaining path can regress quietly.

**Current behavior:**
- dropping a not-yet-open session on the tab rail uses the requested `tabIndex`
- dropping an already-open session ignores the hovered `tabIndex`
- the existing regression test covers only the not-yet-open session path

**Proposal:**
- thread an optional `tabIndex` through the existing-session move path in `openSessionInWorkspaceState()`
- preserve the hovered insertion slot when moving an already-open session into the target pane
- add a workspace regression for the already-open session reorder case

## P2

- [ ] Expand HTTP route tests for the axum API:
  Codex thread actions (archive, unarchive, fork, rollback) and interactive request submissions
  (user input, MCP elicitation, generic app requests) now have HTTP route tests via
  `tower::ServiceExt`. Still missing: session creation, message send, settings updates, Claude
  approvals, and SSE state events.
- [ ] Add backend regression tests for divergent completed-text replacement in Codex streaming:
  cover both shared-Codex and REPL-Codex paths where the final authoritative text must replace,
  not append to, previously streamed content.
- [ ] Add a dedicated stop-failure callback suppression regression:
  force a dedicated runtime stop failure while a matching completion/error/exit callback arrives
  during `runtime_stop_in_progress`, and assert the callback is not lost once the stop fails.
- [ ] Add a `stop_session` test with a queued prompt pending when a shared Codex interrupt fails:
  verify that `dispatch_next_queued_turn` fires and the queued prompt is dispatched after the
  best-effort cleanup completes.
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
- [ ] Add OrchestratorTemplatesPanel tests for update and delete flows:
  mocks for `updateOrchestratorTemplate` and `deleteOrchestratorTemplate` are set up but no test
  exercises them. (Run, `onStateUpdated`, and stale-project handling are now covered.)
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
- [ ] Add App-level control-surface launch regressions for pane-local Files/Git roots:
  render split panes with different session contexts, then verify Files/Git launchers and panels
  use a root/workdir that matches the same pane-local session/project metadata they emit.
- [ ] Add a canvas-move regression for post-relocation session/project sync:
  when an existing shared canvas is moved into a new pane, assert its origin metadata and the
  target pane's `activeSessionId` are updated to the new launch context instead of the old one.
- [ ] Add unit tests for canvas/sessionList origin-refresh on existing tab reuse:
  `openCanvasInWorkspaceState` and `openSessionListInWorkspaceState` now update `originSessionId`/
  `originProjectId` via `replaceWorkspaceTabInPane` when the existing tab's origin differs from the
  new launch context. Add `workspace.test.ts` cases where the existing tab has a null origin and
  assert the new origin values are written after re-open.
- [ ] Add unit tests for `ensureWorkspaceViewId` and `createWorkspaceViewId`:
  `workspace-storage.ts` exports these functions for URL-param handling and workspace ID
  generation. A round-trip test in jsdom would guard against regressions in workspace ID
  normalization or URL-param handling.
- [ ] Add sort-order assertion to `list_workspace_layouts` backend test:
  `list_workspace_layouts_route_returns_saved_workspaces` checks presence but not the documented
  `updated_at` descending sort order. Assert index positions after inserting workspaces with
  distinct timestamps.
- [ ] Add orchestratorList same-pane origin-refresh test:
  `openOrchestratorListInWorkspaceState` now updates `originSessionId`/`originProjectId` on reuse,
  but the existing workspace.test.ts only covers the cross-pane move path. Add a case where the
  orchestratorList is already in the target pane and verify that the origin fields are updated
  without a move occurring.
- [ ] Remove duplicate `saveWorkspaceLayoutSpy.mockClear()` in the pagehide test:
  `App.test.tsx` "flushes a pending workspace layout save with keepalive on pagehide" calls
  `.mockClear()` twice in a row with no intervening action; remove the redundant second call.
- [ ] Add tab-rail session-drop insertion-order regressions:
  cover both the new-session path and the already-open-session move path with a non-zero
  `tabIndex`, asserting the tab lands at the hovered rail position in both cases.
- [ ] Add PaneTabs `dragLeave` cleanup regression for the known-drag path:
  after a `dragOver` shows a drop indicator via `getKnownDraggedTab`, fire `dragLeave` and assert
  the indicator is removed. Current tests only verify the indicator appears but not that it clears.
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
