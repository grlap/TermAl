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

The `ApprovalDecision::Pending` panic and the cyclic transition graph resource exhaustion are also
fixed in the current tree. `Pending` is now rejected by the early guard alongside `Interrupted`
and `Canceled`, and template validation runs a DFS-based cycle detection that rejects non-DAG
transition graphs.

The orchestrator handoff restart recovery gap is also fixed in the current tree. On startup,
`dispatch_orphaned_queued_prompts` scans for idle sessions with queued prompts and no active
runtime, and auto-dispatches them. This covers the window where a transition was committed
(queued prompt persisted, pending transition removed) but the process crashed before
`dispatch_next_queued_turn` ran. A backend regression now covers that exact restart window.

---

## Orchestrator transitions fire on stop and error paths, not only on completed replies

**Severity:** High - downstream automation can run on failure text instead of a completed result.

The current runtime fans out `OnCompletion` transitions from `stop_session`, runtime-exit
handling, and error paths like `fail_turn_if_runtime_matches` / `mark_turn_error_if_runtime_matches`.
That contradicts the current orchestration contract, which defines `OnCompletion` as a source
session becoming prompt-ready after the agent replied. A manually stopped turn or a crashed/error
turn did not produce a completed agent reply, so routing their failure text downstream changes the
meaning of the graph and can cascade invalid follow-up work.

The problem is reinforced by the new tests: the backend now explicitly asserts that
`stop_session` and runtime-exit failures should enqueue downstream prompts. That gives false
confidence around behavior that the feature brief says should not happen.

**Current behavior:**
- `stop_session` schedules downstream transitions and forwards `"Turn stopped by user."`
- runtime exits and turn-error paths schedule downstream transitions even when the source turn did
  not complete normally
- tests codify this behavior as the expected contract

**Proposal:**
- only fire `OnCompletion` transitions from the successful reply -> idle completion path
- treat stop/error/exit paths as terminal or retryable failure states without downstream fan-out
- replace the current stop/error orchestration tests with regressions that assert transitions do
  not fire for non-completed turns

## `tab-drag.ts` and `workspace-storage.ts` validators have drifted

**Severity:** Medium — correctness risk on cross-window tab drags.

Both files contain independent `isWorkspaceTab` switch statements with near-identical validation
logic, but they have diverged:

- `tab-drag.ts` does not validate `originProjectId` on `source`, `filesystem`, `gitStatus`, or
  `diffPreview` tab kinds, while `workspace-storage.ts` does.
- `tab-drag.ts` checks `diffPreview.language` with `isNullableString`, while
  `workspace-storage.ts` checks it with `isOptionalNullableString`.

A tab payload with a numeric `originProjectId` on a `source` tab would pass the drag validator
but fail the storage validator, potentially causing a silent drop on persist.

**Current behavior:**
- a cross-window drag of a `source` tab with a corrupted `originProjectId` is accepted by the
  drag channel but rejected by the storage layer on the next persist cycle
- the `tab-drag.test.ts` tests only cover `originProjectId` validation for `controlPanel`,
  `sessionList`, and `projectList` tab kinds

**Proposal:**
- extract shared tab-shape validation into a common module (e.g. `tab-validation.ts`) and
  import from both `tab-drag.ts` and `workspace-storage.ts`
- alternatively, align the two validators manually and add tests for `originProjectId` on all
  tab kinds in `tab-drag.test.ts`

---

## `read_file` returns HTTP 400 for file-not-found instead of 404

**Severity:** Low — semantic mismatch, not a runtime bug.

When `fs::read_to_string` fails with `io::ErrorKind::NotFound`, the `read_file` handler returns
`ApiError::bad_request(...)` (HTTP 400) instead of `ApiError::not_found(...)` (HTTP 404). The
same pattern appears in `read_directory` and the instruction file read path. A missing resource
is semantically a 404, not a malformed request.

**Current behavior:**
- `GET /api/file?path=/nonexistent` returns HTTP 400 with `{"error": "File not found: ..."}`
- the frontend does not currently distinguish 400 from 404, so no user-visible bug

**Proposal:**
- use `ApiError::not_found(...)` for `io::ErrorKind::NotFound` in `read_file`, `read_directory`,
  and the instruction file read path

---

## `stop_session` error handling is asymmetric across runtime types

**Severity:** Low - inconsistent API contract, not data loss.

`stop_session` now treats a shared Codex `interrupt_and_detach` failure as a warning (logs to
stderr, returns HTTP 200 with the cleaned-up snapshot). However, a non-shared Codex `handle.kill()`
failure and all non-Codex `runtime.kill()` failures still propagate as HTTP 500. The same endpoint
has two error contracts depending on an internal implementation detail (shared vs. non-shared
runtime).

This is pragmatically correct — `detach()` handles authoritative cleanup for shared runtimes, so
the interrupt is best-effort, while killing a dedicated child process is not best-effort (a failure
leaves an orphan). But `kill_session` already takes the warn-and-continue approach for all runtime
types.

**Current behavior:**
- shared Codex stop with interrupt failure → HTTP 200, warning logged to stderr
- non-shared Codex stop with kill failure → HTTP 500
- Claude/ACP stop with kill failure → HTTP 500
- the frontend sees no signal that a shared Codex interrupt partially failed

**Proposal:**
- consider applying the same best-effort pattern to non-shared kills in `stop_session` for
  consistency with the shared path and with `kill_session`
- alternatively, document the asymmetry as intentional if the orphan-process risk justifies it
- optionally amend the stop message text to indicate the interrupt may still be in progress, or add
  a warning-level field to the response

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

## Backend module sizes are growing

**Severity:** Medium â€” maintainability concern, not a runtime bug.

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
- [ ] Add frontend reconcile tests for new interactive message types:
  `userInputRequest`, `mcpElicitationRequest`, and `codexAppRequest` messages are handled by the
  reconciler but have no test coverage verifying that state changes (e.g. pending → submitted)
  correctly produce new message references.
- [ ] Add `tab-drag.test.ts` coverage for `originProjectId` on all tab kinds and `drag-end` message:
  the drag channel validator is missing `originProjectId` checks on `source`, `filesystem`,
  `gitStatus`, `canvas`, `instructionDebugger`, and `diffPreview` tabs. The `drag-end` message
  type in the discriminated union has no test at all.
- [ ] Add `SessionCanvasPanel.test.tsx` afterEach cleanup:
  other test files (`PaneTabs.test.tsx`, `App.test.tsx`) explicitly call `cleanup()` in
  `afterEach`; this file should match the project convention.
- [ ] Add PaneTabs test for "Git Sync" context menu action:
  only the "Git Push" path is exercised; add a test that clicks "Git Sync" and verifies
  `syncGitChanges` is called with the expected workdir.
- [ ] Add PaneTabs test for git status fetch failure in context menu:
  the `statusError` / `statusMessage` state fields exist but the error rendering path when
  `fetchGitStatus` rejects is uncovered.
- [ ] Add OrchestratorTemplatesPanel tests for update, delete, and validation error flows:
  mocks for `updateOrchestratorTemplate` and `deleteOrchestratorTemplate` are set up but no test
  exercises them. Also cover the validation error display path when saving an invalid draft.
- [ ] Add OrchestratorTemplateLibraryPanel tests for fetch error and event-driven re-fetch:
  the error branch (`getErrorMessage`) and the `ORCHESTRATOR_TEMPLATES_CHANGED_EVENT` re-load
  path have no test coverage.
- [ ] Add backend orchestrator validation tests for self-loop, duplicate ID, and empty-sessions:
  `normalize_orchestrator_template_draft` rejects self-referencing transitions, duplicate
  session/transition IDs, and drafts with zero sessions, but only the unknown-target case is
  tested.
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
- [ ] Add unit tests for orchestrator geometry functions:
  `anchorPosition`, `nearestAnchorSide`, `nearestAnchorPosition`, `buildTransitionGeometry`,
  and `isValidAnchor` are pure deterministic functions with no DOM dependencies.
- [ ] Add backend regressions that transitions only fire on real completions:
  replace the current stop-session and runtime-error orchestration expectations with coverage that
  only the successful reply -> idle path triggers `OnCompletion` fan-out.
- [ ] Continue splitting backend modules as they grow:
  `src/main.rs` was split into `api.rs`, `state.rs`, `runtime.rs`, `turns.rs`, `remote.rs`, and
  `tests.rs`. Some of these modules (especially `state.rs` and `turns.rs`) are already large and
  could benefit from further decomposition as features stabilize.

## Later
