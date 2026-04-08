# Bugs & Known Issues

This file tracks reproduced, current issues. Resolved work, speculative refactors,
and cleanup notes do not belong here.

Also fixed in the current tree: reconnect backoff no longer resets to the
initial 400ms delay on repeated `EventSource.onerror` callbacks during the same
outage. The reconnect handler now only resets the cadence for a new failure
cycle after a confirmed reopen or an explicit manual retry, so same-outage
errors preserve the pending fallback timer instead of flattening the backoff.

## Active Repo Bugs

## Manual reconnect retry can stop fallback polling

**Severity:** High - clicking the advertised retry action during an outage can leave recovery stalled after one failed `/api/state` request.

The new manual retry path clears both reconnect timers and resets the backoff before issuing a one-shot `requestStateResync()`. That resync does not preserve the reconnect fallback, so if the forced `/api/state` fetch fails, the catch path records the error and exits without scheduling another timer. A user can therefore click "retry now" while SSE is still down and accidentally cancel the automatic snapshot polling that was keeping recovery alive.

**Current behavior:**
- Manual retry cancels the existing reconnect timeout before firing an immediate `/api/state` fetch.
- If that fetch fails, the reconnect loop is no longer armed and recovery depends on a later `EventSource.onerror`, browser online event, or another manual action.

**Proposal:**
- Preserve or immediately re-arm the reconnect fallback when manual retries call `requestStateResync()`.
- Add a regression that clicks the reconnect badge, forces the manual fetch to fail, and asserts automatic polling remains armed until live recovery is confirmed.

## Em-dash characters in CSS comments corrupted to UTF-8 mojibake

**Severity:** Medium - signals a broken encoding pipeline that will corrupt functional CSS if it reaches a property value or `content:` string.

Two em-dash characters (`—`, U+2014) in `ui/src/styles.css` comments (around lines 147 and 8443) were double-encoded into multi-byte mojibake sequences (`ÃƒÆ'Ã‚Â¢…`). The corruption is a classic UTF-8 → Latin-1 → UTF-8 round-trip artifact, likely introduced by a line-ending conversion step (`core.autocrlf`), an editor re-save, or a pre-commit hook.

**Current behavior:**
- Two CSS comments contain garbled multi-byte sequences instead of em-dashes.
- The corruption is non-functional (comments only) but will accumulate on each re-save if the root cause is not identified.

**Proposal:**
- Restore the original `—` (U+2014) in both comments, or replace with `--`.
- Identify and fix the encoding pipeline step that caused the corruption (check `core.autocrlf`, editor encoding settings, and any file-processing hooks).

## Cold-start fallback retry bypasses exponential backoff

**Severity:** Medium - the initial-hydration and `preserveReconnectFallback`
retry paths poll `/api/state` at a fixed 400ms rate with no backoff.

`scheduleFallbackStateResyncRetry` always uses the fixed
`RECONNECT_STATE_RESYNC_DELAY_MS` constant instead of
`consumeReconnectStateResyncDelayMs()`. The new exponential backoff only applies
to `scheduleReconnectStateResync`, so cold-start retries or failed
fallback-marked resyncs poll at 2.5 req/sec indefinitely if the backend stays
down.

**Current behavior:**
- `scheduleFallbackStateResyncRetry` fires at 400ms regardless of how many
  times it has already retried.
- Only `scheduleReconnectStateResync` uses the exponential backoff series.

**Proposal:**
- Route `scheduleFallbackStateResyncRetry` through the same
  `consumeReconnectStateResyncDelayMs()` backoff, or add an independent backoff
  for the cold-start path.
- Document any intentional deviation (e.g., "fast retries during initial load
  are acceptable because they only fire on first page load").

## Backend reconnect behavior is driven by user-facing error text

**Severity:** Medium - transport recovery now depends on parsing formatted error strings in the UI layer.

`App.tsx` classifies backend-unavailable failures through `shouldInlineControlPanelBackendIssue()` and then mutates `backendConnectionState` / `backendConnectionIssueDetail` from the generic request error path. That makes reconnect behavior depend on the exact wording returned by `api.ts` and the Vite proxy, instead of a structured transport error from the API layer. If the message text changes, the reconnect UI can stop firing even though the underlying failure mode is unchanged.

**Current behavior:**
- Generic request catch blocks can promote a user-facing error message into the reconnect state machine.
- The transport state is inferred from formatted text such as `Request failed with status 502.` or `The TermAl backend is unavailable.`

**Proposal:**
- Return a structured backend-unavailable error from `api.ts` instead of inferring transport state from display text.
- Keep the reconnect decision in the transport layer and let UI copy use the structured error, not the other way around.

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

## Cold-start reconnect retry path lacks regression coverage

**Severity:** Low - the actionable `connecting` retry branch can regress
without a test catching it.

The new retry affordance is wired for both `connecting` and `reconnecting`, but
the added tests only exercise the `reconnecting` path. The cold-start branch
uses a different state transition (`latestStateRevisionRef.current === null`)
and only appears after an initial fetch failure, so it can break without any
current regression failing.

**Current behavior:**
- Tests cover clicking retry from the reconnecting state only.
- No regression test verifies that a cold-start failure exposes the actionable
  `connecting` indicator and that clicking it triggers the expected retry flow.

**Proposal:**
- Add an app-level regression that starts with no hydrated revision, forces the
  initial `/api/state` request to fail, clicks the connecting retry indicator,
  and asserts the retry flow runs.
- Keep the reconnecting-path test, but cover the cold-start branch too so both
  user-visible retry states are exercised.

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

Reconnect fallback no longer stops after a no-op `/api/state` response:
successful fallback snapshots now keep reconnect polling armed until
`EventSource.onopen` or a confirmed live SSE event proves the stream is back,
even when the fetched snapshot is stale or redundant. Covered by the reconnect
polling regression in `ui/src/backend-connection.test.tsx`.

Successful `/api/state` fallback no longer reports the backend as connected before SSE reopens: reconnect polling stays armed until `EventSource.onopen` or a confirmed live event proves the stream is back. The regression is now covered by the reconnect-state test in `ui/src/backend-connection.test.tsx`.

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
- [ ] Add structured-error coverage for backend reconnect handling:
  prove the reconnect UI still fires when the proxy/backend-unavailable message
  changes, and cover the dev-proxy `502` path plus the `reportRequestError()`
  reconnect side effect so the retry path no longer depends on a fragile text
  match or an untested Vite proxy error.
- [ ] Extend reconnect backoff progression coverage:
  assert the full exponential series (400, 800, 1600, 3200, 5000ms cap),
  verify repeated `EventSource.onerror` callbacks do not reset the sequence
  during one outage, and confirm the cap holds at 5000ms instead of 6400ms.
- [ ] Add failed manual-retry coverage for the reconnect badge:
  click retry while SSE remains disconnected, force the immediate `/api/state`
  request to fail, and assert the automatic reconnect fallback stays armed.
- [ ] Add cold-start retry coverage for the connecting badge:
  start with no hydrated revision, force the initial `/api/state` request to
  fail, click the actionable connecting indicator, and assert the retry flow
  runs.
- [ ] Remove dead `resetBackoff` option from `scheduleReconnectStateResync`:
  no call site passes it; all callers reset externally via
  `resetReconnectStateResyncBackoff()`.
- [ ] Wire `onRetry` on the workspace-bar `BackendConnectionStatus` render site:
  currently only the control-panel badge exposes the retry affordance; the main
  workspace-bar chip does not.
- [ ] Extend reconnect recovery coverage to clear inline backend error text:
  after a successful `/api/state` fallback, assert the inline banner / tooltip
  request error disappears together with any reconnect indicator changes.

## Known External Limitations

No currently tracked external limitations require additional TermAl changes.

Windows-specific upstream caveats are handled in-product:
- TermAl forces Gemini ACP sessions to use `tools.shell.enableInteractiveShell = false`
  through a TermAl-managed system settings override on Windows.
- TermAl surfaces WSL guidance before starting Codex on Windows because upstream
  PowerShell parser failures still originate inside Codex itself.
