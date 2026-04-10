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

Also fixed: a single non-JSON line on the shared Codex app-server's stdout
(log output, warnings, partial writes) no longer kills the reader thread. The
reader now skips non-JSON and empty lines with a `continue` instead of
`break`ing out of the loop, logging the skipped content to stderr. Previously
one bad line orphaned all pending JSON-RPC requests and hung the writer thread
indefinitely.

Also fixed: the shared Codex writer thread no longer blocks on JSON-RPC
responses. All requests except `initialize` (startup handshake) are now
fire-and-forget: the writer writes the request and immediately returns to
process the next command. Response waiting is handled by short-lived
per-request waiter threads. For prompt commands that need a new thread, the
waiter extracts the thread ID from the `thread/start` response and feeds a
`StartTurnAfterSetup` command back through the writer's command channel.
`model/list` pagination uses the same pattern via `RefreshModelListPage`
internal commands. This prevents one slow Codex response from blocking all
other sessions and commands on the shared runtime.

Also fixed: `fetchWorkspaceLayout` error message now uses the pre-built
`endpoint` variable (which already applies `encodeURIComponent`) instead of
interpolating the raw `workspaceId`, eliminating a cosmetic inconsistency
between the encoded request URL and the error text.

Also fixed: 504 Gateway Timeout is now classified as `backend-unavailable`
alongside 502 and 503 in `createResponseError`, so reverse-proxy timeouts
trigger the same auto-recovery path instead of falling through to the generic
`request-failed` classification.

Also fixed: `reportRequestError` parameter type simplified from
`unknown | string` to `unknown`. The `| string` branch was dead at the type
level since `unknown` is the top type; the existing `typeof error === "string"`
runtime guard handles the string case regardless.

Also fixed: killing one Codex session no longer tears down the entire shared
runtime. Session-scoped errors (e.g. "session not found" from a killed
session) in the reader thread are now logged and skipped instead of setting
`runtime_failure` and breaking the reader loop. `handle_shared_codex_start_turn`
catches missing-session errors from `record_codex_runtime_config` and returns
`Ok(())` instead of propagating with `?`. The thread-setup waiter checks
`session_matches_runtime_token` before sending `StartTurnAfterSetup`, silently
dropping if the session was killed during setup.

Also fixed: invalid UTF-8 on the shared Codex app-server's stdout no longer
kills the reader thread. `BufReader::read_line` (which returns `Err` on
non-UTF-8) was replaced with `read_until(b'\n')` plus
`String::from_utf8_lossy`. Invalid byte sequences are lossily replaced with
`U+FFFD` and the line proceeds to JSON parsing, where it is skipped as bad
JSON if necessary.

Also fixed: clean EOF on the shared Codex app-server's stdout now fails active
sessions. The reader thread always calls `fail_pending_codex_requests` and
`handle_shared_codex_runtime_exit` on exit, not just when `runtime_failure` is
set. Previously a graceful process exit left sessions stuck as "active" in the
UI with no events ever arriving.

Also fixed: `SharedCodexRuntime::kill()` now sends a `shutdown` JSON-RPC
notification to the app-server and waits up to 3 seconds for a clean exit
before escalating to `process.kill()`. Previously the app-server was
immediately killed with no chance to flush state or clean up resources.

Also fixed: shared Codex setup and turn-start timeouts are now generous
enough to accommodate slow cold starts, auth handshakes, and large thread
resumes. `initialize` and `thread/start`/`thread/resume` wait up to 180
seconds (previously 30), and `turn/start` waits up to 120 seconds
(previously 60). The earlier 30–60 second ceilings could fire on a healthy
but slow Codex session, treating the timeout as a transport failure that
tore down the shared runtime and dropped every attached session.

Also fixed: failed `StartTurnAfterSetup` rollback now restores
thread-discovery bookkeeping. `clear_external_session_id_if_runtime_matches`
calls `ignore_discovered_codex_thread` when the session agent supports Codex
prompt settings, symmetrically undoing the `allow_discovered_codex_thread`
that `set_external_session_id` performed during setup. Previously the
orphan thread remained absent from the ignored set, so the next
`import_discovered_codex_threads` call created a duplicate imported session
for the same Codex thread.

Also fixed: `handle_shared_codex_start_turn` no longer races with
`turn/started` notifications. The request ID is pre-allocated via
`start_codex_json_rpc_request_with_id` and `pending_turn_start_request_id`
is installed before the request hits the wire, so the reader thread always
sees the marker before it can process the corresponding `turn/started`
notification.

Also fixed: failed `StartTurnAfterSetup` handoff no longer orphans a shared
Codex session. The send result is now checked explicitly; on failure,
`forget_shared_codex_thread` rolls back the provisional thread/session
mapping, `clear_external_session_id_if_runtime_matches` removes the external
session ID, and the session is routed through the runtime-exit failure path.

Also fixed: stale shared Codex setup waiters can no longer persist runtime
config onto the wrong runtime. `handle_shared_codex_start_turn` now checks
`session_matches_runtime_token` before persisting `active_codex_*` fields or
mutating shared session state, returning early if the session has been
rebound to a different runtime.

Also fixed: undeliverable Codex server request rejection now sends a
protocol-valid JSON-RPC error response. A new `CodexJsonRpcResponsePayload`
enum separates `Result` and `Error` variants, and
`codex_json_rpc_response_message` generates the correct wire format for
each. The `-32001` rejection is now a top-level `error` object instead of a
nested object inside `result`.

Also fixed: `ApiRequestError.cause` now uses the standard ES2022 `Error`
options bag via `super(message, { cause: options?.cause })` with a
`declare readonly cause` annotation. Developer tooling that reads the
native `Error.cause` slot now correctly discovers the causal chain.

Also fixed: `clearRecoveredBackendRequestError` is now wrapped in
`useCallback` with an empty dependency array, making it a stable reference.
The `refreshWorkspaceSummaries` effect now explicitly lists it and
`setBackendConnectionState` as dependencies, removing the ESLint
suppression comment.

Also fixed: `fetchWorkspaceLayout` now detects HTML fallback bodies before
the 404 early return, so an incompatible backend returning an HTML 404 page
triggers the restart-required error path instead of silently returning
`null`.

Also fixed: `handle_shared_codex_start_turn` now records `active_codex_*`
settings through `record_codex_runtime_config_if_runtime_matches()`, which
holds the state lock across both the runtime-token check and the config
write. This closes the small TOCTOU gap between the old
`session_matches_runtime_token()` pre-check and the later config persist.

Also fixed: shared Codex model-list pagination is now capped at 50 pages.
`RefreshModelListPage` carries an explicit `page_count`, and both the
fire-and-forget runtime path and the blocking test helper bail with a clear
error instead of spawning unbounded waiter threads if `nextCursor` never
terminates.

Also fixed: the `online`/`offline` listener effect and the main SSE
`EventSource` effect in `App.tsx` now explicitly depend on
`clearRecoveredBackendRequestError` and `setBackendConnectionState`. The
callbacks remain stable, but the dependency arrays are now lint-clean and
future-proof if either callback ever gains real dependencies.

Also fixed: the best-effort shared-Codex stop path now suppresses
rediscovery of the detached thread. When `interrupt_and_detach()` fails
and the stop proceeds as best-effort, the cleared `external_session_id`
thread is added to `ignored_discovered_codex_thread_ids`. Without this,
the still-running detached thread would resurface as a new imported
session on the next `import_discovered_codex_threads` pass, even though
the user explicitly stopped the session.

Also fixed: `clear_external_session_id_if_runtime_matches` now takes a
`suppress_rediscovery` flag and only adds the cleared thread to the
ignored-discovery set when the flag is `true`. The `StartTurnAfterSetup`
rollback path passes `true` for `thread/start` (newly created orphan
thread) and `false` for `thread/resume` (pre-existing thread whose
discovery state should be preserved). Previously the rollback
unconditionally ignored the thread, which could permanently hide a valid
resumed thread from discovery.

Also fixed: shared Codex response timeouts no longer tear down the entire
shared runtime. `CodexResponseError` gains a `Timeout` variant, and
`wait_for_codex_json_rpc_response` now distinguishes
`RecvTimeoutError::Timeout` (slow operation) from
`RecvTimeoutError::Disconnected` (channel broken). The thread-setup and
turn-start waiters handle `Timeout` as a per-session failure (like
`JsonRpc`) — failing only the affected turn — instead of routing it
through `handle_shared_codex_runtime_exit` which would drop every attached
session. The `initialize` path still tears down on timeout because the
runtime cannot function without initialization.

Also fixed: the thread-setup waiter's runtime-token check and
`set_external_session_id` call are now atomic.
`set_external_session_id_if_runtime_matches` holds the state lock across
both the token check and the external session id write, following the same
pattern as `record_codex_runtime_config_if_runtime_matches`. Previously a
session could be stopped or rebound in the gap between the two calls,
allowing a stale waiter to resurrect an `external_session_id` on a session
that had already moved on.

Also fixed: late Codex final-output events are no longer dropped after
`turn/completed`. Shared-runtime and REPL sessions now retain just-completed
turn context long enough to accept late `codex/event/agent_message`,
`codex/event/item_completed`, and app-server `item/completed` agent-message
payloads that arrive immediately after turn completion, while still
ignoring unrelated non-message app-server items once the turn is finished.

Also fixed: post-prompt safety-net polling now correctly adopts server state
after a restart. The polling `adoptState` call passes `{ force: true,
allowRevisionDowngrade: true }` so a restarted server whose persisted
revision is lower than the browser's is still adopted. Previously the poll
called `adoptState(freshState)` with no options, which silently rejected the
snapshot when the revision had been downgraded by a restart. The premature
cancellation heuristic (`> revisionAtDispatch + 1`) was also removed — the
poll now stops only when the session is no longer active. The interval is
tracked in a ref so rapid prompts don't stack independent polls, and cleanup
runs on unmount.

## Active Repo Bugs

## `backendConnectionStateRef` can miss reconnects while React state is between commits

**Severity:** High - browser-online recovery can stall until a later event.

`backendConnectionStateRef` is now synchronized from `backendConnectionState`
inside `useLayoutEffect`, but `handleBrowserOnline()` still reads the ref
synchronously to decide whether to request a reconnect. That leaves a window
after `setBackendConnectionState(...)` where the committed state is still
catching up and the ref reports the previous value.

**Current behavior:**
- if the browser comes back online during the state-to-ref gap,
  `handleBrowserOnline()` can conclude the app is already connected
  and skip the reconnect request
- the UI can then stay stuck until a later SSE or polling event happens

**Proposal:**
- keep the ref in lockstep with the state transition path that online/offline
  handlers use, or stop making reconnect decisions from a synchronously read ref
- add a regression that fires `online` in the same turn as the state transition

## Shared Codex reader suppresses real app-server state/persistence failures

**Severity:** High - internal failures can be hidden while sessions stay half-mutated.

The shared Codex reader now logs and ignores every `Err` returned by
`handle_shared_codex_app_server_message`. That is correct for stale-session
cases like "session not found", but the same path can also surface real
commit, persistence, or internal state-update failures. Those errors should not
be downgraded to a benign warning.

**Current behavior:**
- commit or persistence failures are logged and skipped instead of failing
  the affected runtime/session
- the runtime can continue running with partially applied state and no clear
  recovery path for the user

**Proposal:**
- distinguish expected stale-session/runtime-mismatch errors from real
  internal failures
- keep skipping the former, but route the latter through the existing
  runtime-exit or session-failure path

## Shared Codex setup helpers swallow persistence failures as stale-session misses

**Severity:** Medium - prompt setup can fail silently while leaving stale runtime state behind.

`set_external_session_id_if_runtime_matches` and
`record_codex_runtime_config_if_runtime_matches` can fail after mutating
in-memory state. Their callers now treat any `Err` as if the session simply
vanished or no longer matched the runtime token, which collapses a real
persistence failure into a silent no-op.

**Current behavior:**
- thread setup or turn handoff can drop the user's prompt without surfacing
  the persistence error
- in-memory state may already be partly updated even though the operation
  reports success to the caller

**Proposal:**
- return a distinct outcome for lookup/runtime-mismatch versus commit/persist failure
- fail the session/runtime on commit or persistence errors instead of swallowing them

## Paginated Codex model refresh can hang until timeout if the continuation cannot queue

**Severity:** Medium - model refresh can appear frozen for 30 seconds before failing.

`fire_codex_model_list_page()` enqueues a follow-up `RefreshModelListPage`
command when the app-server returns `nextCursor`, but it currently ignores the
result of that `send()`. If the writer thread has already exited or is
shutting down, the continuation is dropped and the original response channel
is never resolved.

**Current behavior:**
- multi-page model refresh can sit until the 30-second caller timeout even
  though the runtime has already lost the continuation
- the user sees a hung refresh instead of a prompt failure

**Proposal:**
- check the enqueue result for the follow-up page request
- on failure, immediately resolve the original response channel with an error
  or route it through the runtime-exit path

## `completed_turn_id` keeps stale turn state alive indefinitely after turn completion

**Severity:** Medium - memory leak per turn and unbounded late-event acceptance window.

After `turn/completed`, `completed_turn_id` is set but `clear_codex_turn_state`
is deliberately not called (to accept late final-output events). The turn state
(`streamed_agent_message_text_by_item_id`, `streamed_agent_message_item_ids`,
etc.) persists until the next `turn/started` or error — an unbounded window.
During that window, stale turn state accumulates memory and a replayed event
matching the completed turn ID is silently accepted.

**Current behavior:**
- turn state maps grow with each completed turn and are only cleared on the
  next turn start
- a stray or replayed event from a completed turn is accepted without bound

**Proposal:**
- clear `completed_turn_id` and call `clear_codex_turn_state` after a bounded
  lifetime (e.g. N seconds or after the first non-agentMessage event)
- alternatively, clear on the next state adoption or the next `turn/started`
  for any session on the same thread

## `JsonRpcNotification` omits `"jsonrpc": "2.0"` version field

**Severity:** Medium - new `shutdown` notification may be silently rejected by a strict Codex app-server.

`CodexRuntimeCommand::JsonRpcNotification` writes `{"method": ...}` without
the `"jsonrpc": "2.0"` field required by the JSON-RPC 2.0 spec. The existing
`initialized` notification follows the same pattern and works empirically, but
a future app-server version with strict validation could reject the frame.

**Current behavior:**
- `shutdown` notification is sent as `{"method":"shutdown"}` without the
  version field
- empirically accepted by the current Codex app-server

**Proposal:**
- add `"jsonrpc": "2.0"` to the notification JSON, and audit existing
  notifications (`initialized`) for the same omission

## SSE hot path now emits unconditional `console.debug` noise

**Severity:** Low - high-volume sessions pay avoidable logging overhead in the browser.

Three new `console.debug` calls were added directly in the SSE `state` and
`delta` event handlers. Those paths run for every streamed event, so the extra
string formatting and console work will accumulate on busy sessions and clutter
production logs.

**Current behavior:**
- streamed sessions emit debug lines for normal state/delta traffic
- browser consoles become noisy and the hot path does extra work in production

**Proposal:**
- remove the logs before merge, or gate them behind an explicit dev-only flag
- keep any retained diagnostics off the per-event hot path

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

- [ ] Add app regression for workspace-layout restart-required recovery:
  fail `fetchWorkspaceLayout()` with restart guidance, assert the toast appears,
  then recover only that route and verify the matching toast clears.
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
- [ ] Add consecutive bad JSON line threshold to shared Codex reader:
  the `continue` on bad JSON can mask a dying Codex process writing garbage.
  After N consecutive unparseable lines (e.g. 5), treat it as a runtime
  failure instead of continuing indefinitely.
- [ ] Explicit `drop(stdin)` in shared Codex writer thread on exit:
  stdin is dropped implicitly when the writer thread exits, but the drop
  order relative to shared-state mutexes is uncontrolled. An explicit
  `drop(stdin)` before cleanup would signal intent and prevent future
  refactoring from delaying the pipe close.
- [ ] Add stdin write backpressure detection to shared Codex writer:
  `write_all` + `flush` block if the Codex process is frozen and the OS
  pipe buffer fills. A watchdog timer or non-blocking write mode would
  detect a stuck writer thread instead of silently blocking all commands.
- [ ] Pass `--listen stdio://` explicitly when spawning `codex app-server`:
  the app-server defaults to stdio but if the default ever changes, the
  spawn would break silently. Explicit flag for defense-in-depth.
- [x] Add max page count limit to `fire_codex_model_list_page`:
  pagination is now capped at 50 pages in both the runtime path and the
  blocking test helper, preventing unbounded waiter-thread churn if a
  misbehaving Codex app-server returns `nextCursor` indefinitely.
- [ ] Add `adoptState` revision downgrade guard test:
  assert `adoptState` returns `false` when `force` is true but
  `allowRevisionDowngrade` is false and `nextState.revision` is lower.
  Cover the complementary case where `allowRevisionDowngrade: true` permits
  the rollback.
- [ ] Add `completed_turn_id` window boundary test:
  run two consecutive turns, deliver a late `agentMessage` from the first
  turn after the second turn has started, and assert it is rejected.
- [ ] Add `shared_codex_event_matches_visible_turn` unit tests:
  cover matching active turn, matching completed turn, mismatched completed
  turn (should return false), and `None` event turn ID with a completed turn.
- [ ] Add `fetchWorkspaceLayout` JSON-body 404 test:
  verify that a standard JSON 404 returns `null` (not an error) to protect
  the reordered HTML-fallback check.
- [ ] Truncate raw child-process content in shared Codex reader stderr log:
  the `eprintln!` for non-JSON lines prints the full line verbatim; truncate
  to a bounded length (e.g. 200 chars) to avoid leaking secrets in logs.
- [ ] Cap `line_buf` growth in shared Codex reader thread:
  `read_until` buffers with no size limit; a line without a newline from a
  misbehaving child process could consume unbounded memory.

## Known External Limitations

No currently tracked external limitations require additional TermAl changes.

Windows-specific upstream caveats are handled in-product:
- TermAl forces Gemini ACP sessions to use `tools.shell.enableInteractiveShell = false`
  through a TermAl-managed system settings override on Windows.
- TermAl surfaces WSL guidance before starting Codex on Windows because upstream
  PowerShell parser failures still originate inside Codex itself.
