# Bugs & Known Issues

This file tracks reproduced, current issues. Resolved work, speculative refactors,
and cleanup notes do not belong here.

## Active Repo Bugs

## Late workspace hydration can still restore a stale control-panel side

**Severity:** Medium - a late workspace layout fetch can persist an obsolete
dock side after the user already resized the split locally.

The initial workspace-layout fetch now skips applying the fetched workspace tree
when `ignoreFetchedWorkspaceLayoutRef` is set, but it still applies
`nextLayout.controlPanelSide` unconditionally before that guard. If the
server-stored side differs from the local/bootstrap side, a late response can
reintroduce the stale dock side after a local resize and then persist that side
back out on the next layout save.

**Current behavior:**
- A manual divider drag during initial hydration protects the split ratio, but
  not the fetched `controlPanelSide`.
- A late server layout can still rewrite the dock side even though the rest of
  the fetched workspace layout was intentionally ignored.

**Proposal:**
- Treat layout-side fields such as `controlPanelSide` as part of the ignored
  layout when local edits have already claimed the initial hydration window.
- Add a regression test that starts with one local side, resolves a late server
  layout with the opposite side, and verifies the local side survives.

## Backend connection chip can keep a stale error after the browser comes back online

**Severity:** Medium - the top-bar connection chip can stay red with an
obsolete failure message while connectivity is already recovering.

`handleBrowserOnline()` updates `backendConnectionState` back to
`connecting`/`reconnecting`, but it does not clear `backendConnectionIssueDetail`.
Because the chip now treats any non-null issue detail as an error, an old
offline or sync-failure message can override the live connection state until a
later successful sync happens to clear it.

**Current behavior:**
- Going offline clears the issue detail, but coming back online does not.
- The connection chip can remain styled as an issue and keep showing stale text
  even after the app starts reconnecting.

**Proposal:**
- Clear `backendConnectionIssueDetail` when handling the browser's `online`
  event, or only render issue text for fresh failures tied to the current
  connection state.
- Add regression coverage for the offline -> online transition.

## Connection tooltip exposes raw backend error text

**Severity:** Low - backend failure text can be surfaced verbatim in the
always-visible connection tooltip.

The new connection tooltip stores `getErrorMessage(error)` in
`backendConnectionIssueDetail` and renders that text in the top bar. For
non-SSH failures, the current sanitizer mostly returns the backend error body as
received, which means stack traces, internal paths, or other low-level
diagnostic text can end up in the main workspace chrome.

**Current behavior:**
- Sync failures can populate the connection tooltip with raw backend error text.
- The detail is accessible from the primary workspace header instead of a more
  deliberate diagnostics surface.

**Proposal:**
- Replace raw backend failure text in this tooltip with a generic connection
  failure message or a sanitized subset.
- Keep detailed diagnostics behind an explicit debug affordance if they are
  still needed.

## Resolved

Connection status tooltip dismissing when mousing from chip to tooltip:
`BackendConnectionStatus` now keeps its hover/focus handlers on the parent
`.workspace-connection-status` container and keeps the tooltip mounted whenever
detail text exists. That lets the existing parent `:hover` / `:focus-within`
rules control visibility, so moving from the chip toward the tooltip no longer
collapses it and the fade-out transition can run. `aria-describedby` and
`aria-hidden` now track the visible state without removing the tooltip DOM.

Duplicated orchestrator action icon/button components:
pause/resume/stop now share `runtime-action-button.tsx`, which centralizes the
runtime action button wrapper plus the shared icon SVGs. `App.tsx` and
`OrchestratorTemplateLibraryPanel.tsx` keep their surface-specific labels and
class-name prefixes, but future icon or button behavior changes only need one
implementation.

Resize during hydration suppresses preferences: The
`ignoreFetchedWorkspaceLayoutRef` flag gated the entire `if (nextLayout)` block
in the workspace layout fetch handler, so a manual divider drag before the
layout fetch resolved would discard all fetched preferences -
`controlPanelSide`, theme, style, font sizes, density - not just the workspace
split ratios. Fixed by restructuring the fetch handler so preference fields are
always applied from the server layout; only the workspace state
(`setWorkspace` + `persistWorkspaceLayout`) is guarded behind the ignore flag.
The save effect handles persisting the merged state (server preferences + user's
manual split ratio) once `isWorkspaceLayoutReady` flips to `true`.

Control panel min-width clamped above default width:
`--control-panel-pane-min-width` exceeded the default
`--control-panel-pane-width`, so
`resolveStandaloneControlPanelDockWidthRatio()` clamped the dock to a width
larger than intended. Fixed by keeping the docked control-panel width and
minimum width aligned in both CSS and TypeScript fallbacks. The current tree
intentionally uses 40rem for both values because that is the minimum acceptable
docked control-panel width.

Mouse text selection resetting in the idle UI: `App.tsx` recreates
`onOpenSourceLink` on every parent render, and `MarkdownContent` used to rebuild
its entire `ReactMarkdown` subtree whenever that callback identity changed. That
replaced the DOM nodes the browser selection was anchored to, collapsing active
text selections even when message content itself was unchanged. Fixed by
memoizing the rendered markdown on content/search/workspace inputs, reading the
callback through a ref so identity-only callback changes do not rebuild the
subtree, and marking markdown links as non-draggable so native drag initiation
does not steal selection. Added tests covering inline code file references both
with and without `onOpenSourceLink`, plus a rerender regression that verifies
the inline code link DOM nodes survive callback identity churn.

Agent readiness mutex contention: `snapshot_from_inner` was recomputing
`collect_agent_readiness` (PATH scanning, dotenv/settings file reads) under the
app-state mutex on every snapshot. On Windows this meant ~648 stat() calls per
snapshot, blocking the async executor and compounding with the frontend resume
watchdog's periodic `/api/state` resyncs. Fixed by introducing `AgentReadinessCache`
with a 5-second TTL outside `state.inner`, double-checked locking refresh, explicit
invalidation on session creation and settings changes, and moving `GET /api/state`
to `run_blocking_api`. Handlers that already hold the `inner` lock use
`cached_agent_readiness()` which can serve stale readiness beyond the 5s TTL if
only `commit_locked` paths run (staleness persists until a `snapshot()` call
refreshes the cache) - this is a documented tradeoff, not a bug, since
filesystem I/O is not safe under the mutex.

## Implementation Tasks

- [ ] Expand the late workspace hydration regression:
  make the deferred layout response include `controlPanelSide`, theme, style,
  font size, editor font size, and density, then assert those preferences still
  merge correctly when a manual resize sets the ignore flag.
- [ ] Add pending-state coverage for `runtime-action-button`:
  hold a run/pause/resume/stop request unresolved long enough to assert
  `disabled`, `aria-busy`, and spinner rendering before the promise resolves.
- [ ] Add keyboard and ARIA coverage for the backend connection tooltip:
  test focus/blur-driven visibility and assert `aria-describedby` /
  `aria-hidden` wiring instead of relying only on hover and class-name checks.

## Known External Limitations

No currently tracked external limitations require additional TermAl changes.

Windows-specific upstream caveats are handled in-product:
- TermAl forces Gemini ACP sessions to use `tools.shell.enableInteractiveShell = false`
  through a TermAl-managed system settings override on Windows.
- TermAl surfaces WSL guidance before starting Codex on Windows because upstream
  PowerShell parser failures still originate inside Codex itself.
