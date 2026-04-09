# Bugs & Known Issues

This file tracks reproduced, current issues. Resolved work, speculative refactors,
and cleanup notes do not belong here.

Also fixed in the current tree: reconnect backoff no longer resets to the
initial 400ms delay on repeated `EventSource.onerror` callbacks during the same
outage. The reconnect handler now only resets the cadence for a new failure
cycle after a confirmed reopen or an explicit manual retry, so same-outage
errors preserve the pending fallback timer instead of flattening the backoff.

Also fixed: em-dash characters in two CSS comments (`ui/src/styles.css`) that
were double-encoded into multi-byte mojibake sequences have been replaced with
ASCII `--` dashes.

Also fixed: cold-start fallback retry (`scheduleFallbackStateResyncRetry`) now
routes through `consumeReconnectStateResyncDelayMs()` for exponential backoff
instead of polling at a fixed 400ms rate.

Also fixed: backend reconnect behavior no longer depends on user-facing error
text. `api.ts` now throws a structured `ApiRequestError` with a discriminated
`kind` field (`"backend-unavailable"` vs `"request-failed"`), and `App.tsx`
uses `isBackendUnavailableError()` instead of string-pattern matching on
formatted messages.

Also fixed: late workspace hydration no longer restores a stale control-panel
side. `controlPanelSide` is now guarded behind `ignoreFetchedWorkspaceLayoutRef`
alongside the workspace tree, so a manual resize during hydration preserves
both the split ratios and the dock side.

Also fixed: the backend connection chip no longer keeps a stale error after the
browser comes back online. `handleBrowserOnline()` now clears
`backendConnectionIssueDetail` and any inline request error message before
requesting a reconnect.

Also fixed: cold-start reconnect retry path now has regression coverage. A new
test exercises the cold-start failure → actionable connecting indicator → click
retry flow.

Also fixed: connection tooltip no longer exposes raw backend error text. SSE
and fallback error handlers now use `describeBackendConnectionIssueDetail()`
which maps errors to fixed generic messages. A regression test asserts internal
paths like `C:\internal\server.ts` do not appear in the tooltip.

Also fixed: manual retry no longer cancels automatic fallback polling. The
failed one-shot probe from `requestBackendReconnectRef` now re-arms the
reconnect state resync timer via `scheduleReconnectStateResync()` in the catch
block when `preserveReconnectFallback` is false, `reconnectStateResyncTimeoutId`
is null, and the client has a hydrated state. Previously a failed click could
leave no `/api/state` retry armed until the next `EventSource.onerror`.

Also fixed: old-backend HTML fallback errors are now reported with the restart
instruction instead of the generic "Could not reach" message. `ApiRequestError`
gains a `restartRequired` flag set for HTML responses from an incompatible
backend. `describeBackendConnectionIssueDetail()` surfaces the original error
message (containing "Restart TermAl") for these errors.
`reportRequestError()` skips the auto-reconnect path so that a successful
`/api/state` probe on the old backend does not silently clear the restart
guidance. The state resync catch block skips both the fallback retry and the
newly added reconnect re-arm for restart-required errors.

Also fixed: offline backend-unavailable banners no longer become sticky after
recovery. The offline branch in `reportRequestError` now preserves the inline
request error marker (`setBackendInlineRequestError(message)` instead of
`null`) so that `clearRecoveredBackendRequestError` can match and clear the
`requestError` when `handleBrowserOnline` fires.

Also fixed: reconnect-success polling no longer self-sustains after watchdog or
wake-gap resyncs. Successful `/api/state` fetches now re-arm reconnect polling
only when the request explicitly came from a reconnect fallback or a manual
retry probe; generic resume/watchdog resyncs remain one-shot.

Also fixed: action-error recovery no longer pollutes SSE-level reconnect state.
`reportRequestError` now uses a dedicated `requestActionRecoveryResyncRef`
instead of `requestBackendReconnectRef`. The action-recovery path does a plain
one-shot `/api/state` probe without resetting `sawReconnectOpenSinceLastError`
or re-arming reconnect polling. Previously a one-off 502 on a non-stream
endpoint (e.g. send message) could cause permanent `/api/state` polling even
though the live SSE stream was healthy.

Also fixed: workspace layout hydration now surfaces restart-required errors.
When `fetchWorkspaceLayout` fails with a restart-required `ApiRequestError`,
the catch handler calls `reportRequestError(error)` so the user sees the
restart instruction instead of a silent degradation.

Also fixed: action-level recovery no longer mutates `backendConnectionState`.
`reportRequestError` no longer promotes a one-off 502 into the reconnecting
state. The connection badge reflects SSE transport state only, so a transient
action failure can no longer leave a permanent reconnect badge while the SSE
stream is healthy. The inline issue detail and error text are still set and
cleared by the action-recovery resync.

Also fixed: `clearRecoveredBackendRequestError` no longer enqueues no-op state
updates. An early return skips the `setState` calls when
`backendInlineRequestErrorMessageRef` is `null`, avoiding unnecessary re-render
work across the ~11 recovery call sites.

Also fixed: layout merge test precision restored. `toBeCloseTo(0.42, 1)` was
too loose (accepted ~0.35 to ~0.49). Updated to `toBeCloseTo(0.44, 4)` to
match the actual clamped ratio (the resize clamp nudges the raw 0.42 drag
target upward to respect the control panel minimum width).

Also fixed: restart-required guidance is now route-scoped and clears precisely
when the failing route recovers. `workspaceLayoutRestartErrorMessageRef`
records the exact error message set by the workspace-layout loader's
restart-required failure. The workspace-layout `.then` path compares the
current `requestError` against that ref and clears the toast only if it
matches — so unrelated `/api/state` success or other transport-level recovery
cannot dismiss the restart guidance. Conversely, the toast is no longer left
stale after the route actually recovers.

Also fixed: reconnect recovery from live SSE events no longer confirms
prematurely. In `handleStateEvent`, `confirmReconnectRecoveryFromLiveEvent()`
is called after `adoptState()` succeeds. In `handleDeltaEvent`, it is called
only in the three known-good branches (ignore, orchestratorsUpdated, applied).
The "resync" and fallthrough delta paths use `rearmOnFailure: true` so a
failed follow-up `/api/state` fetch re-arms polling instead of stalling. The
catch blocks in both handlers restore `backendConnectionState` to
`"reconnecting"` and re-arm fallback polling, so the retry affordance stays
available and recovery continues.

Also fixed: `performRequest` now preserves the original `fetch` exception on
the wrapped `ApiRequestError.cause`, so callers keep the raw failure object
without losing the structured error classification.

## Active Repo Bugs

## `backendConnectionStateRef` is mutated inside a functional state updater

**Severity:** High - React 18 can replay or discard updater functions, desynchronizing reconnect decisions from the committed connection state.

`setBackendConnectionState` now mirrors `backendConnectionState` into
`backendConnectionStateRef` so `handleBrowserOnline()` can synchronously decide
whether a reconnect request is needed. The functional-updater branch performs
that ref write inside `setBackendConnectionStateRaw((current) => ...)`, which
means the ref mutation runs during React's render scheduling, not after a
committed state transition. In React 18, functional updaters are expected to be
pure: they may be replayed, invoked more than once, or discarded under
concurrent/strict rendering. A replayed or abandoned updater can therefore
leave the ref out of sync with the UI state that actually committed.

**Current behavior:**
- `backendConnectionStateRef.current` is assigned inside the functional updater
  passed to `setBackendConnectionStateRaw`.
- `handleBrowserOnline()` reads that ref to decide whether to request a
  reconnect, so a replayed/discarded updater can spuriously trigger or suppress
  recovery work.

**Proposal:**
- Keep the functional updater pure and derive the next state only.
- Sync `backendConnectionStateRef.current` from committed state (for example in
  a `useEffect` keyed on `backendConnectionState`) or use another post-commit
  mechanism.

## `ApiRequestError.cause` bypasses the standard `Error` options bag

**Severity:** Medium - developer tooling (browser devtools "Caused by" chains, Sentry) may not see the wrapped cause.

`ApiRequestError` declares `readonly cause: unknown` and assigns it manually after `super(message)` instead of forwarding it through the standard ES2022 `Error` options bag (`super(message, { cause })`). The `super` call never receives the `cause`, so the native `Error.cause` slot is not set during construction. The explicit field assignment shadows the inherited property and works in practice, but error-aware tooling that reads `cause` from the prototype chain during construction will not pick it up.

**Current behavior:**
- `super(message)` is called without the `cause` option.
- `this.cause = options?.cause` manually assigns the field, shadowing `Error.cause`.

**Proposal:**
- Pass `cause` through the `super` call: `super(message, { cause: options?.cause })`.
- Remove the explicit `readonly cause: unknown` declaration and manual assignment, or keep the type annotation only.

## `clearRecoveredBackendRequestError` captured as stale closure in `handleBrowserOnline` effect

**Severity:** Low - latent fragility; safe today but breaks silently if the function body ever reads state directly.

The `useEffect` for `handleBrowserOnline`/`handleBrowserOffline` has an empty dependency array `[]` but captures `clearRecoveredBackendRequestError`, a plain function that re-creates each render. The function currently only reads from refs and uses stable setters, so the stale closure is safe. However, any future change that adds direct state reads (not via refs) would silently break without a lint warning.

**Current behavior:**
- `clearRecoveredBackendRequestError` is a plain function re-created each render, captured once by the `[]`-dep effect.
- Works correctly because its body only uses `backendInlineRequestErrorMessageRef` and stable `setState` calls.

**Proposal:**
- Wrap in a ref (consistent with `requestBackendReconnectRef` and `requestActionRecoveryResyncRef`) or extract to a `useCallback` with proper deps.

## `fetchWorkspaceLayout` treats HTML 404 fallbacks as missing layout

**Severity:** Low - an incompatible backend can still degrade silently on the workspace-layout route instead of surfacing restart guidance.

`fetchWorkspaceLayout` returns `null` immediately on any 404 before checking
whether the response body is an HTML fallback page. That means an old backend
that does not serve `/api/workspaces/:id` can answer with an HTML 404 page and
the client will interpret it as "layout missing" rather than a restart-required
incompatible backend. The new restart-guidance path therefore still has a route
shape that degrades silently.

**Current behavior:**
- Any 404 from `/api/workspaces/:id` returns `null` before `looksLikeHtmlResponse`
  runs.
- HTML fallback responses on that route do not become `restartRequired`
  `ApiRequestError`s, so the UI shows no restart guidance.

**Proposal:**
- Detect HTML fallback bodies before the 404 early return, or otherwise
  distinguish a missing workspace record from a route that is not served by the
  current backend.

## Unencoded workspace ID in `fetchWorkspaceLayout` error message

**Severity:** Low - cosmetic inconsistency; no XSS risk due to React text rendering.

`fetchWorkspaceLayout` passes the raw `workspaceId` to `formatUnavailableApiMessage` at line 341 while the actual request at line 326 uses `encodeURIComponent`. If a workspace ID contained unusual characters, they would appear verbatim in the error message while the encoded form was sent to the server.

**Current behavior:**
- The request URL uses `encodeURIComponent(workspaceId)`.
- The error message uses the raw `workspaceId` string.

**Proposal:**
- Use the pre-built `endpoint` variable in the `formatUnavailableApiMessage` call for consistency.

## 504 Gateway Timeout not classified as backend-unavailable

**Severity:** Medium - a common reverse-proxy outage path bypasses the reconnect/retry recovery flow entirely.

`createResponseError` in `api.ts` classifies 502 and 503 as `backend-unavailable` but omits 504 (Gateway Timeout). A 504 would be classified as `request-failed` with a generic message, missing the auto-recovery path.

**Current behavior:**
- Only 502 and 503 trigger the `backend-unavailable` classification.
- 504 falls through to the generic `request-failed` path.

**Proposal:**
- Add `|| status === 504` alongside the 502/503 check in `createResponseError`.

## `reportRequestError` parameter type `unknown | string` is redundant

**Severity:** Note - cosmetic type issue with no runtime impact.

The `reportRequestError` function in `App.tsx` is typed with `error: unknown | string`. Since `unknown` is the top type in TypeScript, `unknown | string` simplifies to just `unknown` — the `| string` branch is dead at the type level and misleading.

**Current behavior:**
- The type annotation suggests two distinct overloads, but TypeScript does not enforce the distinction.
- The `typeof error === "string"` runtime guard handles the string case correctly regardless.

**Proposal:**
- Simplify to `error: unknown` and rely on the existing runtime guard, or add a JSDoc comment documenting the string call shape.

## Resolved

Reconnect-success re-arm no longer creates a polling loop after authoritative
progress: reconnect-triggered `/api/state` fetches now only re-arm fallback
polling when the fetch resolves without advancing the held revision. If the
reconnect resync adopts a newer revision, the success path stops after that
recovery instead of scheduling another timer. This fixes
`resets the watchdog drift baseline after a long reconnect resync completes`.

Reconnect delta recovery no longer waits for a revision bump: once the stream
reopens, the first successfully parsed live SSE payload now confirms recovery
and clears the reconnect fallback timer even if the delta is ignored as stale
or already applied. That lets the connection badge settle on "Connected" and
fixes `cancels the reconnect fallback after a reconnect error when the first
reconnect delta is ignored`.

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

- [x] Preserve original error object in `performRequest` catch block:
  `performRequest` now passes the caught `fetch` error into
  `createBackendUnavailableError`, and `ApiRequestError` stores it on the
  wrapper instance as `cause`. The constructor still needs the standard
  `Error` options bag; see the active bug above.
- [ ] Add API test for rejected `fetch` cause propagation:
  make `fetch` reject in `performRequest`, assert the wrapper is classified as
  `backend-unavailable`, and verify `error.cause` preserves the original
  thrown exception.
- [ ] Add app regression for workspace-layout restart-required recovery:
  fail `fetchWorkspaceLayout()` with restart guidance, assert the toast appears,
  then recover only that route and verify the matching toast clears.
- [ ] Extend backend-unavailable status coverage in `api.test.ts`:
  cover HTTP 503 explicitly alongside 502, and add 504 coverage once
  `createResponseError` classifies gateway timeouts as backend-unavailable.
- [ ] Add pending-state coverage for `runtime-action-button`:
  hold a run/pause/resume/stop request unresolved long enough to assert
  `disabled`, `aria-busy`, and spinner rendering before the promise resolves.
- [ ] Add keyboard and ARIA coverage for the backend connection tooltip:
  test focus/blur-driven visibility and assert `aria-describedby` /
  `aria-hidden` wiring instead of relying only on hover and class-name checks.
- [ ] Extend reconnect backoff progression coverage:
  assert the full exponential series (400, 800, 1600, 3200, 5000ms cap),
  verify repeated `EventSource.onerror` callbacks do not reset the sequence
  during one outage, and confirm the cap holds at 5000ms instead of 6400ms.
- [ ] Remove dead `resetBackoff` option from `scheduleReconnectStateResync`:
  no call site passes it; all callers reset externally via
  `resetReconnectStateResyncBackoff()`.
- [ ] Wire `onRetry` on the workspace-bar `BackendConnectionStatus` render site:
  currently only the control-panel badge exposes the retry affordance; the main
  workspace-bar chip does not.
- [ ] Extend reconnect recovery coverage to clear inline backend error text:
  after a successful `/api/state` fallback, assert the inline banner / tooltip
  request error disappears together with any reconnect indicator changes.
- [ ] Add snapshot-vs-live-stream recovery contrast test:
  set up a disconnect, let a snapshot poll succeed at a stale revision (so
  `rearmOnSuccess` re-arms polling and the badge stays "reconnecting"), then
  receive a live delta after `onopen` and assert the connection transitions to
  "connected" and polling stops. Contrasts the two distinct code paths
  (`rearmOnSuccess` polling vs `confirmReconnectRecoveryFromLiveEvent`).
- [ ] Add test for state handler catch-block reconnecting restoration:
  simulate a reconnect recovery where the SSE stream reopens and delivers a
  state event that fails during `adoptState` (e.g., reducer throws). Assert
  `backendConnectionState` is restored to "reconnecting" (not stuck on
  "connected") and fallback polling re-arms via `scheduleReconnectStateResync`.
- [ ] Add test for action-recovery resync probe:
  trigger a backend-unavailable error from a user action (e.g., send message
  502) while the SSE stream is healthy. Assert `requestActionRecoveryResyncRef`
  fires a one-shot `/api/state` probe that does NOT re-arm reconnect polling
  or reset `sawReconnectOpenSinceLastError`.
- [ ] Consistent `vi.isFakeTimers()` guard in backend-connection tests:
  tests 4 and 6 call `vi.useRealTimers()` unconditionally in `finally`
  while test 3 uses the `if (vi.isFakeTimers())` guard. Align on the
  defensive pattern for clarity.
- [x] Restore `toBeCloseTo` precision in layout merge test:
  Updated to `toBeCloseTo(0.44, 4)` — the raw 0.42 target is nudged by the
  control panel minimum width clamp, so the assertion now matches the actual
  clamped ratio at precision 4.
- [ ] Remove redundant `headers` from `fetchWorkspaceLayout`:
  `fetchWorkspaceLayout` explicitly passes `Content-Type: application/json` to
  `performRequest`, which already sets the same default. If `performRequest`'s
  defaults grow, `fetchWorkspaceLayout` would silently drop them.
- [ ] Add data integrity assertion to bad-state-payload recovery test:
  the "accepts a later live delta on the same reopened stream after a bad state
  payload" test verifies connection status recovery but does not confirm the
  previously recovered session data survived the orphan delta.
- [ ] Add missing state fields to pending reconnect deferred in stale-detail test:
  the "clears stale backend issue detail when the browser reconnects" test
  resolves its deferred with a state response missing `orchestrators` and
  `workspaces` fields present in all other test state responses.

## Known External Limitations

No currently tracked external limitations require additional TermAl changes.

Windows-specific upstream caveats are handled in-product:
- TermAl forces Gemini ACP sessions to use `tools.shell.enableInteractiveShell = false`
  through a TermAl-managed system settings override on Windows.
- TermAl surfaces WSL guidance before starting Codex on Windows because upstream
  PowerShell parser failures still originate inside Codex itself.
