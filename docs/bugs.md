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

The `ApprovalDecision::Pending` panic, the unbounded orchestrator template graph size, and the
frontend/client-side template size drift are also fixed in the current tree. `Pending` is now
rejected by the early guard alongside `Interrupted` and `Canceled`, template validation caps
templates at 50 sessions and 200 transitions, and `OrchestratorTemplatesPanel` now mirrors those
limits locally while disabling the "Add session" affordance at the cap.

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
and the stop-time callback suppression contracts. Terminal runtime callbacks (`fail_turn`,
`mark_turn_error`, `finish_turn_ok`, `handle_runtime_exit`) that arrive during a dedicated
stop window are now deferred in ordered `deferred_stop_callbacks` queues and replayed in arrival
order when the stop fails, so a session can no longer get stuck in a stale Active state or
reconstruct the wrong completion-then-exit sequence after a failed kill attempt. Tab-rail
session drops now also preserve the hovered insertion index for already-open sessions, and
workspace regressions cover both the newly opened and already-open move paths.

The `RuntimeExited`-before-other-callbacks deferred replay ordering bug and the stale `runButton`
DOM reference concern are also fixed in the current tree. The failed-stop replay loop now sorts
`RuntimeExited` callbacks last via `sort_by_key` before replaying, so `TurnCompleted` and other
terminal callbacks always process before the runtime is cleared. The regression test
`failed_dedicated_stop_replays_runtime_exit_last_even_when_it_arrives_first` covers the reversed
arrival order `[RuntimeExited(None), TurnCompleted]` and asserts the session ends `Idle`. The
"keeps restored template edits dirty" test already re-queries the run button after the Reset
click via a separate `refreshedRunButton` reference, so the pre-reset reference is only used for
pre-reset assertions and does not produce a stale-DOM false positive.

The SSE reconnect-timer race, consolidate-delivery empty/pruned-predecessor edge cases,
consolidate-only deadlock handling, restored-draft initialization gaps, self-loop transition
rendering collapse, panel draft-persistence debounce loss, and the write-only
`last_delivered_completion_revision` note are also fixed in the current tree. SSE `onopen` now
clears the fallback reconnect state-resync timer immediately, consolidate delivery skips
zero-predecessor templates and runtime-pruned predecessors instead of constructing empty
deliveries, deadlocked consolidate-only cycles now fail closed with an orchestrator `error_message`
instead of staying `Running`, legacy templates and saved drafts missing `inputMode` now default to
`queue` on both backend deserialization and frontend restore without weakening the main TypeScript
template type, stale restored drafts pointing at deleted templates are dropped back to an empty
clean state, self-loop transitions render as cubic SVG paths, `OrchestratorTemplatesPanel` flushes
pending localStorage writes on `pagehide`, `beforeunload`, and unmount, and completed sessions no
longer reschedule already-delivered completion revisions. Reconnect fallback `/api/state` refreshes
now also force-adopt same-revision snapshots after backend restarts, queued work no longer
auto-dispatches for sessions owned by stopped orchestrators, and `kill_session` now reruns
orchestrator reconciliation so consolidate deadlocks are surfaced instead of leaving instances
stuck `Running`. Reconnect fallback state refreshes also treat reconnect-path `/api/state`
snapshots as authoritative when the backend restarts behind the last streamed delta, provided no
newer SSE state arrived while the fetch was in flight. Legacy queued prompts without explicit
provenance now deserialize as `Legacy` instead of `User`, and stopped orchestrators clear stale
legacy/orchestrator-owned queued prompts while preserving explicit user work.

---

## `ui/src/App.tsx` is large enough to trigger Babel deoptimization warnings

**Severity:** Note - the main frontend entry file is now large enough to create tooling friction even though the app still runs correctly.

Running the frontend now emits Babel's "code generator has deoptimised the styling" warning for
`ui/src/App.tsx` because the file exceeds the generator's 500 KB pretty-print threshold. This is not
an immediate runtime bug, but it is a concrete signal that build output, source-map ergonomics, and
routine maintenance are all getting worse as more UI behavior accumulates in one file.

The underlying problem is structural rather than cosmetic: workspace layout logic, pane routing,
control-surface state, session rendering, and modal/settings behavior all continue to land in the
same top-level module. That raises the cost of review and makes unrelated UI changes collide in the
same file more often than they should.

**Current behavior:**

- `ui/src/App.tsx` is large enough to trigger Babel's deoptimization warning during frontend builds
- multiple distinct UI responsibilities still live in one monolithic module
- routine frontend changes frequently require editing or reviewing the same oversized file

**Proposal:**

- split `ui/src/App.tsx` into smaller modules by responsibility instead of continuing to grow the main file
- extract self-contained workspace/controller hooks and pane-rendering sections first, where the existing seams already exist
- keep `App.tsx` as the composition/root wiring layer instead of the implementation home for every control flow

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

- [ ] Add frontend reconcile tests for new interactive message types:
      `userInputRequest`, `mcpElicitationRequest`, and `codexAppRequest` messages are handled by the
      reconciler but have no test coverage verifying that state changes (e.g. pending ? submitted)
      correctly produce new message references.
- [ ] Add test for deferred callbacks discarded on a successful stop:
      pre-stage a `DeferredStopCallback::TurnCompleted` entry, let `stop_session` succeed normally, and
      assert the session ends `Idle` (not `Error`) and `deferred_stop_callbacks` is empty.
- [ ] Fix multi-callback ordering oracle test to pre-populate a message:
      `failed_dedicated_stop_replays_multiple_deferred_callbacks_in_order` starts with zero messages;
      pre-populate an in-progress assistant message so `finish_turn_ok` produces a measurably different
      `messages.len()` from `RuntimeExited` alone, making the oracle comparison unambiguous.
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
- [ ] Extract shared drag-drop test setup helper:
      the two drag-drop tests in `App.test.tsx` duplicate ~60 lines of identical fetch-mock and
      setup boilerplate. Extract a shared `renderAppWithProjectAndSession()` helper.
- [ ] Add unit tests for orchestrator geometry functions:
      `anchorPosition`, `nearestAnchorSide`, `nearestAnchorPosition`, `buildTransitionGeometry`,
      `buildSelfLoopTransitionGeometry`, `anchorNormal`, `cubicBezierPoint`, `cubicBezierDerivative`,
      `perpendicularOffsetPoint`, and `isValidAnchor` are pure deterministic functions with no DOM
      dependencies.
- [ ] Continue splitting backend modules as they grow:
      `src/main.rs` was split into `api.rs`, `state.rs`, `runtime.rs`, `turns.rs`, `remote.rs`, and
      `tests.rs`. Some of these modules (especially `state.rs` and `turns.rs`) are already large and
      could benefit from further decomposition as features stabilize.

## Later
