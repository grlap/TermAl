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

- [ ] Add Gemini as a first-class agent in the backend and UI:
  `Agent` enum, session creation, session rendering, and persistence need to stop assuming the
  world is only Claude or Codex.
- [ ] Implement a persistent Gemini runtime adapter:
  spawn `gemini` with `--output-format stream-json`, map stdout events into TermAl messages, and
  wire message dispatch through the same session runtime path used by Claude and Codex.
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
- [ ] Add post-edit diff preview from agent messages:
  when an agent reports that it updated a file, let the user open a new tab with a diff preview of
  those changes and include a link back to the originating conversation or message.

## P2

- [ ] Refresh the frontend dev toolchain to remove the Node 24 `util._extend` deprecation from
  Vite's proxy path; upgrade `vite`, `@vitejs/plugin-react`, and `vitest` together and verify
  `npm run dev` with the existing `/api` proxy config.
- [ ] Expand HTTP route tests for the axum API:
  Codex thread actions (archive, unarchive, fork, rollback) and interactive request submissions
  (user input, MCP elicitation, generic app requests) now have HTTP route tests via
  `tower::ServiceExt`. Still missing: session creation, message send, settings updates, Claude
  approvals, kill, and SSE state events.
- [ ] Add frontend reconcile tests for new interactive message types:
  `userInputRequest`, `mcpElicitationRequest`, and `codexAppRequest` messages are handled by the
  reconciler but have no test coverage verifying that state changes (e.g. pending → submitted)
  correctly produce new message references.
- [ ] Continue splitting backend modules as they grow:
  `src/main.rs` was split into `api.rs`, `state.rs`, `runtime.rs`, `turns.rs`, `remote.rs`, and
  `tests.rs`. Some of these modules (especially `state.rs` and `turns.rs`) are already large and
  could benefit from further decomposition as features stabilize.

## Later
