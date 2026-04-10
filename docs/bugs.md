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

## Shared Codex watchdog startup can orphan the app-server child

**Severity:** Medium - failed post-spawn watchdog setup can leave a stray Codex
process running without an owning runtime.

`spawn_shared_codex_runtime()` starts `codex app-server`, captures its pipes,
wraps the child in `SharedChild`, and only then calls
`spawn_shared_codex_stdin_watchdog(...)`. If that watchdog thread spawn fails,
the function returns the error without explicitly killing and waiting on the
already-started child. `SharedChild` does not provide an owner-side kill-on-drop
guarantee for this setup failure path.

**Current behavior:**
- a watchdog spawn failure after `Command::spawn()` returns from
  `spawn_shared_codex_runtime()` with an error
- the already-started `codex app-server` can survive without the matching
  TermAl runtime, writer, reader, or waiter ownership

**Proposal:**
- explicitly kill and wait on the child on every post-spawn initialization
  failure before returning
- alternatively, move fallible watchdog setup before child startup or introduce
  a centralized post-spawn cleanup guard

## Shared Codex stdin watchdog drops teardown failures

**Severity:** Medium - timeout recovery can silently fail and leave the blocked
runtime alive.

When the shared Codex stdin watchdog detects a write or flush that exceeded the
timeout, it calls `handle_shared_codex_runtime_exit(...)` and discards the
result. If the runtime-exit path fails while updating state or persistence, the
watchdog exits anyway and there is no fallback kill/cleanup path for the
blocked writer or child process.

**Current behavior:**
- timeout detection logs the generic timeout detail and calls runtime exit
- any error returned by runtime exit is ignored
- the watchdog stops even if teardown did not actually complete

**Proposal:**
- log `handle_shared_codex_runtime_exit(...)` failures in the watchdog thread
- add a fallback process cleanup path so the timeout still guarantees runtime
  shutdown when state/persistence teardown fails

## Delta-event reconnect recovery lacks direct regression coverage

**Severity:** Medium - a modified reconnect recovery path can regress without a
test failure.

`handleDeltaEvent` now mirrors the state-event catch path by restoring
`backendConnectionState` to `"reconnecting"` and re-arming fallback polling
after a parse or reducer failure during reconnect recovery. The added frontend
coverage exercises malformed reopened `state` payloads, but it does not send a
malformed or crashing `delta` payload through the changed catch path.

**Current behavior:**
- malformed reopened state payload recovery is covered
- malformed delta or delta-reducer failure recovery is not directly covered
- a regression could leave the UI stuck on `"connected"` with fallback polling
  not re-armed

**Proposal:**
- add a mirrored reconnect regression that reopens SSE, dispatches a malformed
  delta payload, and asserts the UI returns to `"reconnecting"`
- assert the fallback `/api/state` probe remains armed after the delta failure

## Resolved

Also fixed: workspace-layout restart-required recovery now has precise helper
coverage. `resolveRecoveredWorkspaceLayoutRequestError(...)` clears only the
exact tracked restart-required toast after the layout route recovers, while
leaving unrelated request errors visible.

Also fixed: `handleDeltaEvent` catch block now decouples
`setBackendConnectionState("reconnecting")` from the timer-pending guard,
matching the pattern already applied to `handleStateEvent`. Previously a
delta handler crash during reconnect recovery with an armed timer would
leave the UI stuck on "connected".

Also fixed: stale-snapshot reconnect test now matches production behavior.
Since `onopen` unconditionally sets "connected", the test no longer asserts
the reconnecting indicator persists. Instead it verifies that polling
remains armed (another fallback fetch fires at the next interval), and that
a live delta with a fresh revision confirms recovery and stops polling.

Also fixed: action-recovery test expects the correct `fetchState` call
count (1 instead of 2). Bootstrap state arrives via SSE, not `fetchState`,
so only the action-recovery one-shot probe fires.

Also fixed: `shared_codex_stdin_timeout_detail` now logs the internal
detail ("writer thread blocked on stdin") to stderr and returns a generic
"Agent communication timed out." message for the user-facing session error,
consistent with the persistence-failure sanitization pattern.

Also fixed: reconnect backoff progression test now returns a stable
`revision: 1` from the fetch mock instead of incrementing on each call.
The production rearm condition requires the revision to be unchanged (no
live SSE event adopted newer state), so the incrementing mock broke the
backoff timer chain after the second fetch.

Also fixed: `shared_codex_bad_json_streak_failure_detail` no longer
embeds a raw child stdout preview in the user-facing failure detail. The
per-line previews are still logged to stderr as they arrive; the session
error message is now a generic count of consecutive bad-JSON lines.

Also fixed: shared Codex persistence failures no longer expose absolute
filesystem paths to users. Both setup-failure sites (`thread registration`
and `runtime config`) now log the full error (with paths) to stderr and
surface a generic "Failed to save session state" message in the session
error.

Also fixed: the reconnect same-tick regression test now captures the fetch
count before the reconnect sequence and asserts it increased, proving the
`online` handler triggered an immediate fetch rather than relying on the
count being satisfied by bootstrap fetches alone.

Not a bug: `fail_turn_if_runtime_matches` persistence fallback does NOT
publish at a stale revision. `bump_revision_and_persist_locked` increments
`inner.revision` (line 869) *before* calling `persist_internal_locked`,
so when persistence fails and `commit_locked` returns `Err`, the in-memory
revision is already advanced. The fallback `publish_state_locked` publishes
at the bumped revision, which SSE clients adopt normally. The regression
test at `tests.rs:15298` asserts `snapshot.revision == baseline_revision + 1`
and at line 15317 confirms the published SSE event carries the same
advanced revision.

Also fixed: `api.test.ts` JSON 404 test assertion corrected from
`Accept: "application/json"` to `"Content-Type": "application/json"` to
match what `performRequest` actually sends.

Also fixed: ACP `session/set_config_option` requests now include
`"jsonrpc": "2.0"`. Both inline construction sites (`state.rs`, model
option and cursor mode) now emit the required version field.

Also fixed: ACP JSON-RPC responses now include `"jsonrpc": "2.0"`. The three
missing sites — automatic approval (`runtime.rs`), unsupported-method error
(`runtime.rs`), and user-initiated approval (`state.rs`) — now emit the
required version field, matching the systematic Codex fix.

Also fixed: `fail_turn_if_runtime_matches` now publishes state to the
frontend even when persistence fails. Previously a disk error during
`commit_locked` would leave in-memory state updated (status → Error,
error message pushed) but never published, so the frontend would stay
stuck showing an active turn.

Also fixed: the shared Codex stdout line cap was raised from 64 KB to
16 MB. Legitimate large JSON-RPC payloads (e.g. `aggregatedOutput` from
long command executions) can easily exceed 64 KB. The drain-and-skip
behavior for lines exceeding the cap still protects against pathological
no-newline streams.

Also fixed: the `backendConnectionStateRef` lockstep regression now has a
passing fake-timer test. The reconnect test advances fake timers explicitly
instead of waiting on a frozen `waitFor(...)` polling interval.

Also fixed: `shared_codex_app_server_error_is_stale_session` now matches only
the exact `session `{id}` not found` shape. Sub-entity misses such as
`message `{id}` not found` and `anchor message `{id}` not found` are no longer
downgraded to benign stale-session noise.

Also fixed: persistence failures while registering a shared Codex thread or
recording shared-runtime config no longer tear down the entire shared Codex
runtime. Those failures now stay session-scoped and only attempt to fail the
affected turn.

Also fixed: shared Codex completed-turn cleanup now runs through a single
runtime-owned cleanup worker instead of spawning one detached sleeper thread
per completed turn. Cleanup work naturally stops when the shared session map
is dropped.

Also fixed: the eager `backendConnectionStateRef` write in
`setBackendConnectionState(...)` now fully replaces the old layout-effect sync
path. The redundant `useLayoutEffect` mirror was removed and the eager write
is documented inline.

Also fixed: REPL-mode Codex app-server responses now route through the shared
JSON-RPC response formatter, so result frames include `"jsonrpc": "2.0"` along
with the already-audited shared-runtime request/response sites.

Also fixed: oversized JSON-RPC lines on shared Codex app-server stdout no
longer tear down the shared runtime. `read_capped_child_stdout_line` now
drains lines that exceed the 64 KB safety cap and discards them with a
stderr warning, keeping the reader aligned with the next newline-delimited
message. Previously a legitimate large payload (e.g. `aggregatedOutput`
from a long `git diff` or build log) would trip the cap and kill every
attached session.

Also fixed: `backendConnectionStateRef` now stays in lockstep with
`setBackendConnectionState(...)` so back-to-back `offline` / `online` events
cannot skip the reconnect request before `useLayoutEffect` runs.

Also fixed: the shared Codex reader now only downgrades stale missing-session
errors. Real app-server handling failures set `runtime_failure`, fail pending
work, and route the runtime through the normal exit path.

Also fixed: `set_external_session_id_if_runtime_matches` and
`record_codex_runtime_config_if_runtime_matches` now return distinct
lookup/runtime-mismatch outcomes while preserving real commit/persistence
failures as `Err`. Shared Codex setup callers now tear down the affected
runtime on those failures instead of silently dropping the prompt.

Also fixed: paginated Codex model refresh now checks the continuation
`send()` result. If the next `RefreshModelListPage` cannot queue, the original
caller receives an immediate error instead of hanging until timeout.

Also fixed: shared Codex `completed_turn_id` state now has a bounded cleanup
window. Late final-output events are still accepted briefly after
`turn/completed`, then a short cleanup clears `completed_turn_id` and the
residual turn-state maps so stale events are no longer accepted indefinitely.

Also fixed: shared Codex notifications now include `"jsonrpc": "2.0"`, and the
same version field is now emitted for the audited `initialized` notification,
Codex requests, and JSON-RPC responses for protocol consistency.

Also fixed: unconditional `console.debug` calls were removed from the SSE
`state` / `delta` hot path so production sessions no longer pay per-event
logging overhead or spam the browser console.

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

- [x] Add app regression for workspace-layout restart-required recovery:
  replaced the brittle StrictMode call-count test with direct coverage for
  `resolveRecoveredWorkspaceLayoutRequestError(...)`. The regression now proves
  a recovered layout success clears only the exact tracked restart-required
  toast while leaving unrelated request errors untouched.
- [x] Add pending-state coverage for `runtime-action-button`:
  `App.test.tsx` now holds an orchestrator pause request unresolved long enough
  to assert `disabled`, `aria-busy`, and spinner rendering before the response
  transitions the group to its paused state.
- [x] Add keyboard and ARIA coverage for the backend connection tooltip:
  `backend-connection.test.tsx` now drives both tooltip components through
  focus/blur and asserts the visible/hidden role transitions together with
  `aria-describedby` and `aria-hidden` wiring.
- [x] Extend reconnect backoff progression coverage:
  the fetch mock now returns a stable revision so the rearm condition passes
  and the full 400→800→1600→3200→5000→5000ms backoff series completes.
- [x] Remove dead `resetBackoff` option from `scheduleReconnectStateResync`:
  `scheduleReconnectStateResync()` is now a no-argument helper. Backoff resets
  remain explicit at the existing `resetReconnectStateResyncBackoff()` call
  sites instead of flowing through an unused option bag.
- [x] Extend reconnect recovery coverage to clear inline backend error text:
  the reconnect regression now resolves the successful `/api/state` fallback
  before asserting the stale backend-unavailable tooltip text is gone, then
  confirms the later SSE `onopen` clears the reconnect indicator itself.
- [x] Add snapshot-vs-live-stream recovery contrast test:
  the test now matches production behavior — `onopen` clears the badge, but
  polling remains armed after a stale snapshot. A live delta with a fresh
  revision confirms recovery and stops further polling.
- [x] Harden same-tick offline/online reconnect regression coverage:
  the reconnect regression now dispatches `EventSource.onerror` together with
  browser `offline` / `online` in one synchronous turn before React flushes,
  then asserts the extra `/api/state` probe is already present before the
  400ms fallback timer window elapses.
- [x] Add test for state handler catch-block reconnecting restoration:
  `backend-connection.test.tsx` now sends a malformed reconnect state payload
  after `onopen`, and `App.tsx` restores `"reconnecting"` immediately even when
  the original fallback timer is still armed, then proves the 400ms fallback
  `/api/state` probe still fires.
- [ ] Add test for delta handler catch-block reconnecting restoration:
  mirror the malformed reopened state payload test with a malformed delta event,
  then assert the UI returns to `"reconnecting"` and fallback `/api/state`
  polling remains armed.
- [x] Add test for action-recovery resync probe:
  fixed to expect 1 `fetchState` call (bootstrap comes via SSE). The test
  drives a create-session 502, asserts the one-shot probe fires, clears the
  error on success, and does not arm reconnect polling.
- [x] Consistent `vi.isFakeTimers()` guard in backend-connection tests:
  the reconnect test cleanup now consistently guards `vi.useRealTimers()` with
  `vi.isFakeTimers()` so the suite does not assume fake timers were enabled in
  every branch before `finally` runs.
- [x] Restore `toBeCloseTo` precision in layout merge test:
  Updated to `toBeCloseTo(0.44, 4)` — the raw 0.42 target is nudged by the
  control panel minimum width clamp, so the assertion now matches the actual
  clamped ratio at precision 4.
- [x] Remove redundant `headers` from `fetchWorkspaceLayout`:
  `fetchWorkspaceLayout` now delegates directly to `performRequest(endpoint)`,
  preserving the shared default headers in one place.
- [x] Add data integrity assertion to bad-state-payload recovery test:
  the reconnect regression now reasserts that the recovered session name and
  preview survive the later orphan delta, and that the ignored delta does not
  leak its preview into the UI.
- [x] Add missing state fields to pending reconnect deferred in stale-detail test:
  the deferred reconnect response now uses the standard `makeBackendStateResponse`
  fixture so `orchestrators` and `workspaces` stay aligned with the rest of the
  backend-connection suite.
- [x] Add consecutive bad JSON line threshold to shared Codex reader:
  after 5 consecutive unparseable stdout lines, the shared Codex runtime now
  treats the stream as failed instead of continuing indefinitely.
- [x] Explicit `drop(stdin)` in shared Codex writer thread on exit:
  the writer now closes the pipe explicitly before thread teardown, and also
  drops it immediately on initialization failure.
- [x] Add stdin write backpressure detection to shared Codex writer:
  the shared Codex stdin writer is now wrapped with activity tracking and a
  watchdog thread; if a write or flush stays blocked past the timeout, TermAl
  tears down the shared runtime instead of hanging the writer loop forever.
- [x] Pass `--listen stdio://` explicitly when spawning `codex app-server`:
  both the shared runtime and REPL Codex spawn paths now pass the stdio
  listener explicitly for defense-in-depth.
- [x] Add max page count limit to `fire_codex_model_list_page`:
  pagination is now capped at 50 pages in both the runtime path and the
  blocking test helper, preventing unbounded waiter-thread churn if a
  misbehaving Codex app-server returns `nextCursor` indefinitely.
- [x] Add `adoptState` revision downgrade guard test:
  the guard now lives in the pure `shouldAdoptSnapshotRevision` helper, and
  `state-revision.test.ts` covers forced equal-revision adoption, forced
  downgrade rejection without `allowRevisionDowngrade`, and the explicit
  rollback case when downgrades are allowed.
- [x] Add `completed_turn_id` window boundary test:
  the shared-Codex regression suite now covers the handoff from one turn to the
  next and asserts that a late first-turn `agentMessage` is rejected once the
  second turn has started and cleared `completed_turn_id`.
- [x] Add `shared_codex_event_matches_visible_turn` unit tests:
  full branch coverage — matching active turn, matching completed turn,
  mismatched completed turn rejection, `None` event turn ID with a
  completed turn, orphan event with no active/completed turn, and
  completed-turn match blocked by active-turn priority.
- [x] Add `fetchWorkspaceLayout` JSON-body 404 test:
  `api.test.ts` now covers the non-HTML 404 path and asserts the call resolves
  to `null` while still using the encoded workspace route.
- [x] Truncate raw child-process content in shared Codex reader stderr log:
  non-JSON stdout log previews are now capped at 200 characters before they
  are emitted to stderr.
- [x] Cap `line_buf` growth in shared Codex reader thread:
  the reader now uses a capped incremental line read instead of unbounded
  `read_until`, so a child that never emits a newline cannot grow memory
  without bound.
- [x] Add wrapped-cause test case for stale-session error classifier:
  `shared_codex_app_server_error_classifier_only_ignores_missing_sessions` now
  covers both wrapped stale-session errors and wrapped sub-entity misses, so
  the `err.chain().any(...)` traversal stays regression-tested.
- [x] Remove unused functional-updater overload from `setBackendConnectionState`:
  the setter now accepts only concrete `BackendConnectionState` values, which
  matches every call site and removes the misleading React-style updater shape.
- [x] Add `"jsonrpc": "2.0"` to ACP JSON-RPC response construction sites:
  all three ACP response sites now include `"jsonrpc": "2.0"` — automatic
  approval, unsupported-method error, and user-initiated approval.
- [x] Assert session error state in persistence-failure tests:
  both shared-Codex persistence-failure tests now assert the session ends in
  `SessionStatus::Error`, the preview carries the persistence-failure detail,
  and a `Turn failed: ...` assistant message is present.
- [x] Consider typed JSON-RPC message structs to prevent `"jsonrpc"` omissions:
  JSON-RPC request, notification, result-response, and error-response builders
  now flow through typed message structs, and the direct helper tests lock in
  the required `"jsonrpc": "2.0"` field for each message shape.
- [x] Add test for `fail_turn_if_runtime_matches` persistence fallback:
  the new regression test forces synchronous persistence failure and asserts
  the session still publishes an Error snapshot with the turn-failure message.
- [x] Add `codex_json_rpc_response_message` Error variant unit test:
  direct formatter coverage now checks the `Error` payload in addition to the
  existing `Result` payload and indirect server-request rejection test.
- [x] Add `"jsonrpc": "2.0"` to ACP `session/set_config_option` sites:
  both inline construction sites now include the version field.
- [x] Fix `api.test.ts` JSON 404 header assertion:
  corrected from `Accept` to `Content-Type` to match `performRequest`.
- [x] Migrate pre-existing ACP test to typed JSON-RPC builder:
  the prompt-loop ACP regression now uses `json_rpc_result_response_message`
  and asserts the serialized writer output still carries `"jsonrpc":"2.0"`.
- [x] Add `shouldAdoptSnapshotRevision` non-force and null-revision test cases:
  `state-revision.test.ts` now covers no-options delegation, `force: false`
  delegation back to `shouldAdoptStateRevision`, and `currentRevision === null`
  with `force: true`.

## Known External Limitations

No currently tracked external limitations require additional TermAl changes.

Windows-specific upstream caveats are handled in-product:
- TermAl forces Gemini ACP sessions to use `tools.shell.enableInteractiveShell = false`
  through a TermAl-managed system settings override on Windows.
- TermAl surfaces WSL guidance before starting Codex on Windows because upstream
  PowerShell parser failures still originate inside Codex itself.
