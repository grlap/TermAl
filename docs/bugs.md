# Bugs & Known Issues

This file tracks only reproduced, current issues and open review follow-up
tasks. Resolved work, fixed-history notes, speculative refactors, cleanup notes,
and external limitations do not belong here. Review follow-up task items live in
the Implementation Tasks section.

## Active Repo Bugs

## Marker navigator logic is embedded in AgentSessionPanel

**Severity:** Low - marker grouping, sorting, DOM lookup, navigation state, and chip rendering are growing an already large panel component.

`ui/src/panels/AgentSessionPanel.tsx:653` now hosts a distinct marker navigator/chip subsystem. The file is past the architecture review threshold, and the marker navigation feature has a clear extraction boundary with its own state, helpers, DOM lookup, and tests.

**Current behavior:**
- Marker navigator logic lives directly in `AgentSessionPanel.tsx`.
- The panel owns marker grouping, sorting, DOM slot lookup, navigation state, and rendering.
- Future marker UI changes are coupled to the broader session panel.

**Proposal:**
- Extract a focused conversation-marker panel/helper module.
- Move marker-specific tests next to that module while leaving `AgentSessionPanel` to wire data and callbacks.

## Mermaid dynamic import fallback lacks import-failure coverage

**Severity:** Medium - the first fallback branch can regress when the optimized Mermaid module chunk fails to load.

`ui/src/message-cards.tsx:230` catches dynamic `import("mermaid")` failures and falls back to the bundled script path. Current coverage exercises fallback after the module import succeeds and rendering fails, but not the branch where the dynamic import itself rejects.

**Current behavior:**
- Render-failure fallback after a successful Mermaid import is covered.
- Dynamic import fetch failure is not forced in tests.
- A broken first fallback branch could pass the current test suite.

**Proposal:**
- Add an isolated Vitest case that forces `import("mermaid")` to reject with a dynamic import fetch error.
- Assert the bundled script path renders successfully.

## Conversation overview segment cap does not bound homogeneous runs

**Severity:** Low - `maxItemsPerSegment` reads as a hard cap but same-kind message runs can bypass it.

`ui/src/panels/conversation-overview-map.ts:422` merges same-visual-class messages before enforcing the item cap. Homogeneous transcript runs can therefore collapse into a single unbounded segment, which weakens keyboard and accessibility navigation granularity and makes the option contract misleading.

**Current behavior:**
- Mixed-kind runs are split according to the configured segment cap.
- Same-kind runs can exceed `maxItemsPerSegment`.

**Proposal:**
- Apply the item cap before the same-visual-class fast path, or rename/comment the option as a mixed-run cap.
- Add focused coverage for a long homogeneous run.

## Mermaid fallback loader lives in the message card renderer

**Severity:** Low - Mermaid fallback loading and cache ownership are mixed into an already large rendering component.

`ui/src/message-cards.tsx:182` now owns dynamic import failure classification and global fallback script loading, while `mermaid-render.ts` already owns Mermaid rendering configuration and queueing.

**Current behavior:**
- Mermaid fallback loader/cache logic lives in `message-cards.tsx`.
- Rendering, fallback loading, and message-card composition are coupled in the same large module.

**Proposal:**
- Move fallback loading into `mermaid-render.ts` or a small `mermaid-loader.ts`.
- Keep message cards calling a single render helper.

## Markdown diff change-block grouping rules duplicated between renderer and index builder

**Severity:** Medium - the change-navigation index walker copies the renderer's grouping rules; future drift between the two will silently desynchronize navigation stops from rendered blocks.

`ui/src/panels/markdown-diff-view.tsx:508-526` and `ui/src/panels/markdown-diff-change-index.ts:60-87`. Both walks have identical logic: skip `normal`, gather consecutive non-`normal` segments, break at the same `current.kind === "added" && next.kind === "removed"` boundary, and produce identical id strings (`segments.map(s => s.id).join(":")`). The renderer then re-derives the same id and looks it up in a `Map<id, index>` the navigation code built from the index walker's output. The header comment in `markdown-diff-change-index.ts:46-54` explicitly acknowledges "the rule is duplicated here so the navigation index does not drift from what the user sees" — i.e., the only thing keeping the two walks in sync is the test suite.

**Current behavior:**
- Renderer (`renderMarkdownDiffSegments`) and index builder (`computeMarkdownDiffChangeBlocks`) walk the same segment array twice with identical grouping rules.
- The navigation index is recovered by id-lookup against a Map built from the index walker's output.
- Any future change to the grouping rules (e.g., a third break-rule for a new segment kind) must be made in both places.

**Proposal:**
- Have `renderMarkdownDiffSegments` consume the precomputed `changeBlocks` directly. Iterate (`normal` segment OR `changeBlocks[changeBlockCursor]`); the renderer emits the editable section for normals and the `<section>` wrapper for the next change-block, advancing `changeBlockCursor` after each.
- Single source of truth for grouping rules in `computeMarkdownDiffChangeBlocks`; the navigation index becomes the literal cursor position, no Map lookup needed, and the renderer's per-render Map allocation goes away.

## Concurrent shutdown callers can flip `persist_worker_alive` before the join owner finishes

**Severity:** Medium - the documented "flag flips only after worker join" contract is not true when two `AppState` clones call `shutdown_persist_blocking()` concurrently.

`shutdown_persist_blocking()` takes the worker handle out of `persist_thread_handle`, releases that mutex, and then blocks in `handle.join()`. A second concurrent caller can enter while the first caller is still joining, see `None`, and run the idempotent branch that stores `persist_worker_alive = false`. That lets a concurrent `commit_delta_locked()` take the synchronous fallback while the worker may still be doing its final drain/write, reopening the dual-writer persistence race the round-13 ordering was meant to close.

**Current behavior:**
- The first shutdown caller owns the join handle but does not hold the handle mutex while joining.
- A second shutdown caller treats `None` as "already stopped" even if the first caller is still waiting for the worker to stop.
- The second caller can publish `alive == false` before the worker has actually exited.

**Proposal:**
- Serialize the full shutdown transition so no caller can observe the stopped state until the join owner has returned from `handle.join()` and stored `persist_worker_alive = false`.
- Alternatively replace the `Option<JoinHandle>` state with an explicit `Running` / `Stopping` / `Stopped` state so only the join owner can transition from stopping to stopped.

## Rendered diff regions reset document-level Mermaid/math budgets

**Severity:** Medium - splitting rendered diff preview into one `MarkdownContent` per region weakens existing browser-side render-budget guards.

The rendered diff view now maps every renderable region to its own `MarkdownContent`. `MarkdownContent` counts Mermaid fences and math expressions per rendered document, so this split resets `MAX_MERMAID_DIAGRAMS_PER_DOCUMENT` and `MAX_MATH_EXPRESSIONS_PER_DOCUMENT` for each region instead of for the full diff preview. A crafted or simply large diff with many Mermaid/math regions can render far more expensive diagrams/equations than the previous single synthetic-document path allowed.

**Current behavior:**
- Each rendered diff region gets an independent Mermaid/math budget.
- The whole rendered diff preview no longer has one aggregate render cap.

**Proposal:**
- Compute aggregate Mermaid/math counts before mapping regions and apply a document-level fallback when the aggregate exceeds the cap.
- Or pass a shared render-budget context/override into each region-level `MarkdownContent`.

## Post-shutdown persistence writes still leave a post-collection-pre-join window

**Severity:** Medium - round-13 closed the dual-writer file race, but a narrow gap remains between the worker's final `collect_persist_delta` and `handle.join()` returning.

Round 13 moved the `persist_worker_alive` flip from BEFORE the Shutdown signal to AFTER `handle.join()` returns. That closed the dual-writer hazard (concurrent fallback writes racing the worker's still-in-progress final drain on the same persistence path). However, after the worker captures its final delta but before `handle.join()` returns, a concurrent `commit_delta_locked` will observe `alive == true`, bump `inner.mutation_stamp`, and return without persisting. That mutation is not picked up by the worker (already past collection) nor by the sync fallback (flag still true). `commit_locked` and `commit_persisted_delta_locked` are unaffected because they call `persist_internal_locked` which itself errors and falls back when the channel becomes disconnected.

**Current behavior:**
- The dual-writer file race is closed by round 13.
- A narrow window between the worker's final `collect_persist_delta` and `handle.join()` returning still exists; `commit_delta_locked` calls in that window observe `alive == true` and return without persisting.
- `commit_locked` / `commit_persisted_delta_locked` infer fallback from `persist_tx.send` failure, but `persist_tx` only disconnects when the LAST `AppState` clone drops its sender; with multiple clones (which is the production shape), `send` succeeds silently into a worker that has exited.

**Proposal:**
- Either (a) serialize the worker's final drain with sync fallback by holding `inner` for the worker's collect-and-write final iteration, or (b) require callers to quiesce non-HTTP producers before invoking `shutdown_persist_blocking`.
- Add a regression that races a late `commit_delta_locked` with the worker's final collection and proves the final persisted state is the latest `StateInner`.
- Add an explicit `persist_worker_alive` Acquire check to `persist_internal_locked` so all four commit variants share one shutdown contract.

## Duplicate remote delta hydrations fall through to unloaded-transcript delta application

**Severity:** Medium - duplicate in-flight hydration callers receive `Ok(false)`, which every delta handler treats as "no repair happened; continue applying the delta".

The in-flight map suppresses duplicate `/api/sessions/{id}` fetches, but it does not coordinate the waiting delta handlers. For a summary-only remote proxy, a concurrent text delta or replacement can still run against missing messages and trigger a broad `/api/state` resync; a message-created delta can partially mutate an unloaded transcript before the first full hydration finishes.

**Current behavior:**
- The first delta for an unloaded remote session starts full-session hydration.
- A duplicate delta for the same remote/session sees the in-flight key and returns `Ok(false)`.
- Callers continue into the narrow delta path as if no hydration was needed.

**Proposal:**
- Return a distinct outcome such as `HydrationInFlight`, or have duplicates wait/queue behind the first hydration.
- After the first hydration completes, re-check the session transcript watermark before applying queued or retried deltas.
- Add burst/concurrent same-session delta coverage proving only one remote fetch occurs and duplicate deltas do not mutate unloaded transcripts.

## Text-repair hydration lacks live rendering regression coverage

**Severity:** Medium - the lower-revision text-repair adoption path is covered only by a classifier unit test.

The new adoption rule is intended to fix the user-visible bug where the latest assistant message stays hidden until an unrelated focus, scroll, or prompt rerender. The current coverage proves the pure classifier returns `adopted`, but it does not prove the live hook requests the flagged hydration, adopts the lower-revision session response after an unrelated newer live revision, flushes the session slice, and renders the repaired text immediately.

**Current behavior:**
- `classifyFetchedSessionAdoption` has a unit test for divergent text repair after a newer revision.
- No hook or app-level regression drives `/api/sessions/{id}` through the live-state path and asserts immediate transcript rendering.

**Proposal:**
- Add a `useAppLiveState` or `App.live-state.reconnect` regression where text-repair hydration is requested, a newer unrelated live event advances `latestStateRevisionRef`, the session response resolves at the original request revision, and the active transcript updates without any extra user action.

## Timer-driven reconnect fallback can stop after `/api/state` progress before SSE proves recovery

**Severity:** Medium - a fallback snapshot can refresh visible UI while the live EventSource transport is still unhealthy.

`ui/src/app-live-state.ts:2068` disables `rearmUntilLiveEventOnSuccess` when a same-instance `/api/state` response makes forward revision progress, unless the recovery path is the manual-retry variant. A successful `/api/state` fetch proves that polling can reach the backend and can repair visible state, but it does not prove the SSE stream has reopened or can deliver later assistant deltas. If the transport remains broken, a later live message can stay hidden until another reconnect/error/user action restarts recovery.

**Current behavior:**
- Timer-driven reconnect fallback asks to keep polling until live-event proof.
- Same-instance `/api/state` forward progress disables that live-proof rearm path for non-manual recovery.
- UI state can look refreshed while the EventSource transport is still unconfirmed.

**Proposal:**
- Split "snapshot refreshed UI" from "transport recovered" in the reconnect state machine.
- Keep reconnect polling armed until `confirmReconnectRecoveryFromLiveEvent()` runs from a data-bearing SSE event, unless a cause-specific recovery path intentionally documents a different contract.
- Add a regression that adopts same-instance `/api/state` progress through the timer-driven reconnect path, keeps SSE unopened/unconfirmed, advances timers, and asserts another fallback poll is scheduled.

## Remote hydration in-flight cleanup can race with the RAII guard

**Severity:** Low - clearing `remote_delta_hydrations_in_flight` by key can remove or later invalidate a newer in-flight hydration for the same remote/session.

The remote hydration guard removes its `(remote_id, session_id)` key on drop. `clear_remote_applied_revision` can also remove keys for a remote while an older hydration guard is still alive. If a later hydration inserts the same key after that cleanup, the older guard can drop afterward and remove the newer marker, allowing duplicate hydrations despite the guard.

**Current behavior:**
- In-flight hydration entries are keyed only by `(remote_id, session_id)`.
- Remote continuity cleanup can remove a live key while the guard that owns it is still alive.
- A stale guard drop cannot distinguish its own entry from a newer entry with the same key.

**Proposal:**
- Store a unique token or generation per in-flight entry and remove only when the token still matches.
- Or avoid clearing live in-flight markers during remote continuity reset; let the owning guard retire its own marker.
- Add cleanup tests covering overlapping guards and per-remote cleanup.

## Lagged force-adopt marker clearing on EventSource reconnect lacks coverage

**Severity:** Low - the frontend now clears an armed lagged recovery marker on EventSource error/reconnect, but no test pins that boundary.

The new baseline guard covers same-stream stale recovery after a newer delta, but a separate hazard is an old `lagged` marker surviving across a closed EventSource into a new stream. The implementation clears the marker on reconnect/error cleanup, yet no regression proves a stale lower/same-instance state on the new stream cannot be force-adopted.

**Current behavior:**
- `clearForceAdoptNextStateEvent` runs during EventSource error/reconnect cleanup.
- Existing lagged tests do not cross an EventSource boundary.

**Proposal:**
- Add a reconnect test that dispatches `lagged`, triggers `error`, opens a new EventSource, and sends a lower/same-instance state that must not be force-adopted.

## Remote hydration dedupe coverage bypasses the production burst path

**Severity:** Low - the current duplicate-hydration test manually seeds the in-flight map instead of driving real bursty remote deltas.

The test pins the duplicate branch, but it would not catch a regression where the first real hydration leaks the guard, where a successful hydration does not clear the marker, or where multiple actual same-session delta handlers still issue duplicate remote session fetches.

**Current behavior:**
- The test inserts an in-flight key directly.
- It does not prove the first production hydration inserts and clears the guard.
- It does not prove bursty same-session deltas issue only one remote session fetch.

**Proposal:**
- Add coverage for a successful hydration path that asserts the guard is removed afterward.
- Add a burst/concurrent same-session delta case that asserts only one remote session fetch is issued.

## `apply_remote_state_if_newer_locked` `force: bool` parameter is unnamed at call sites

**Severity:** Low - seven call sites pass `false` and one passes `true`; readers cannot tell what `force` means without consulting the function signature.

`apply_remote_state_if_newer_locked` was extended with a `force: bool` parameter so that `apply_remote_lagged_recovery_state_snapshot` can bypass the same-revision replay gate. The parameter is correct, but the convention scales poorly: a future caller that copies a neighbouring `false` from any of the seven existing sites will inherit the gated behaviour without realising the parameter exists, and a future maintainer who needs the bypass at a different site will have to re-derive what the boolean means.

**Current behavior:**
- `apply_remote_state_if_newer_locked(&mut inner, remote_id, &remote_state, None, false)` appears at seven call sites.
- One new call site passes `true` for lagged-recovery force-apply.
- The doc-comment on the function explains the parameter, but the call sites do not self-document.

**Proposal:**
- Replace `force: bool` with a typed `enum SnapshotApplyMode { GateBySnapshotRevision, ForceApplyAfterLagged }` (or similar). All existing call sites become `SnapshotApplyMode::GateBySnapshotRevision`; the lagged-recovery site reads `SnapshotApplyMode::ForceApplyAfterLagged` and self-documents.
- Optional: also push the bypass-gate into a tiny inline comment at the lagged-recovery site naming the upstream invariant (`api_sse.rs::state_events` yields `state` immediately after `lagged` within one `tokio::select!` arm).

## SSE recreation control plane is split between `sseEpoch` state and `pendingSseRecreateOnInstanceChangeRef`

**Severity:** Medium - two coordination mechanisms for one concern, increases regression risk and reduces debuggability.

`forceSseReconnect()` sets `pendingSseRecreateOnInstanceChangeRef.current = true` synchronously and the consume happens inside `adoptState` only when `fullStateServerInstanceChanged` is true. This adds a second control plane for SSE reconnection alongside the existing `sseEpoch` state, with the ref-vs-state ordering being load-bearing (synchronous `setSseEpoch` would tear down the in-flight probe). The pattern is documented inline, but ref state is not visible in React DevTools or in any state diff, so a subsequent maintainer reading the SSE transport effect cannot see the gate that determines whether the effect re-runs. The Round 8 comment in the doc-block notes this exact split-plane pattern was reverted before for the same reason. There is also no clear-on-no-instance-change reset path: if `forceSseReconnect()` fires but the recovery probe response comes back as same-instance (false alarm), the flag stays armed and could fire on a much later legitimate restart.

**Current behavior:**
- `forceSseReconnect()` mutates a ref invisible to DevTools.
- The flag is consumed only inside the `fullStateServerInstanceChanged` branch of `adoptState`.
- A successful recovery probe with no instance change leaves the flag armed indefinitely.
- The flag-on-adopt ordering relative to `setSseEpoch` is not pinned by a load-bearing test (current tests assert the recreate happens but not the ordering).

**Proposal:**
- Lift the gate into a state-driven shape (e.g., a single `sseReconnectReason` state with `instanceChangeAfterAdopt` as one of its values), so the SSE reconnection trigger is visible in React DevTools.
- Or add a load-bearing test that fails if the consume-on-adopt ordering is reversed.
- Either way, clear `pendingSseRecreateOnInstanceChangeRef` on any `adoptState` success that does not change the instance, so a false-alarm `forceSseReconnect()` cannot fire on a later legitimate restart.

## Sticky shutdown tests bypass `/api/events` stream wiring

**Severity:** Medium - helper-level tests can pass while the production SSE handler still hangs during shutdown.

The new tests validate the sticky `watch` shutdown helper directly, but they do not exercise `state_events` or the `/api/events` route using that signal. A future regression in the stream's pre-loop checks or select wiring could keep long-lived SSE connections open and block graceful shutdown while the helper tests still pass.

**Current behavior:**
- Tests cover the shutdown signal helper before/after registration.
- They do not hold or open `/api/events` streams and assert termination through the route handler.

**Proposal:**
- Add route-level SSE shutdown tests for shutdown-before-connect and shutdown-after-initial-state.
- Wrap both in timeouts so missed shutdown delivery fails loudly.

## Shutdown signal registration errors can look like real shutdown

**Severity:** Medium - `src/main.rs:147-166`. The new `shutdown_signal()` helper ignores `tokio::signal::ctrl_c().await` errors, and on Unix the SIGTERM branch completes immediately if `tokio::signal::unix::signal(...)` returns `Err`.

Those error paths should be diagnostics or startup failures, not successful shutdown triggers. If signal registration fails, the server can exit immediately after startup with little context.

**Current behavior:**
- Ctrl+C signal errors are discarded with `let _ = ...`.
- Unix SIGTERM registration failure makes the `terminate` future complete.
- The `tokio::select!` cannot distinguish a real shutdown signal from a signal-listener setup failure.

**Proposal:**
- Make signal setup fallible during startup and return an error if registration fails.
- Or log the registration/await error and park that branch with `std::future::pending::<()>().await` so it cannot trigger shutdown.

## Final shutdown persist failure exits without retry

**Severity:** Medium - `src/app_boot.rs:270-275`. The normal persist worker records failures and retries with backoff, but a shutdown tick sets `should_exit_after_tick` and breaks after the first final attempt even if that attempt failed.

A transient SQLite lock, disk hiccup, or I/O error during graceful shutdown can still drop pending mutations. The new drain logs the failure, but the process continues toward exit as though the final state reached disk.

**Current behavior:**
- `retry_state.record_result(&result)` records the final failure.
- `should_exit_after_tick` still breaks the loop immediately.
- Pending changed sessions can remain only in memory when the process exits.

**Proposal:**
- On shutdown, exit only after a successful final persist.
- Or use a bounded retry/timeout policy and return/log a shutdown failure outcome that clearly says durability was not confirmed.
- Add a test covering `Err` followed by `Ok` after `PersistRequest::Shutdown`.

## Triplicate `requestStateResync + startSessionHydration` recovery pattern in delta handler

**Severity:** Low - `ui/src/app-live-state.ts:2329, 2421, 2437`. Three near-identical recovery sites within ~110 lines of the same handler perform the same `requestStateResync({ rearmOnFailure: true }) + startSessionHydration(delta.sessionId)` pair. The `appliedNeedsResync` branch knows `delta.sessionId` is statically a string; the other two branches add a runtime guard (`"sessionId" in delta && typeof delta.sessionId === "string"`) — the type narrowing is subtly different at each site.

A future fourth recovery branch would need to update three sites; collapsing into a helper subsumes the gate and centralizes the contract comment.

**Proposal:**
- Extract `function triggerRecoveryForDelta(delta: DeltaEvent)` that performs the resync and conditional hydration.
- Replace the three call sites with the helper. Centralize the contract comment.

## Two backend Lagged branches duplicate the lagged-marker emission

**Severity:** Low - `src/api_sse.rs:182-200, 204-215`. The state-receiver and delta-receiver Lagged branches now both yield `lagged` followed by a recovery state snapshot built via `state_snapshot_payload_for_sse(state.clone()).await`. The branches are byte-identical apart from comments. The third Lagged branch (`file_receiver` at line 221) deliberately doesn't recover — so a 2-of-3 helper is still warranted for the asymmetric maintenance risk: a future change that grows one branch (e.g., a tracing log, structured `data` body, or `revision` hint on the marker) needs to be mirrored manually on the other.

**Proposal:**
- Extract a helper that yields the marker + recovery snapshot. The `async_stream::stream!` macro doesn't compose cleanly with helpers that themselves yield, so consider a named local closure or document the invariant explicitly.
- Or, accept the duplication and add cross-referencing comments naming both branches.

## Per-session hydration burst has no cooldown beyond in-flight deduplication

**Severity:** Low - `ui/src/app-live-state.ts:2329, 2421, 2437`. The new `startSessionHydration(delta.sessionId)` calls trigger `GET /api/sessions/{id}` (full transcript fetch) on every problematic delta. `hydratingSessionIdsRef` deduplicates concurrent fetches per session, but it does not rate-limit successive fetches: once a hydration completes, the next problematic delta on the same session immediately schedules another full transcript fetch. On a flaky network with bursty deltas, a hydration→delta→hydration loop is possible, each iteration shipping the entire transcript over the wire.

**Current behavior:**
- In-flight dedup via `hydratingSessionIdsRef` collapses simultaneous calls to one round-trip.
- After completion, the next problematic delta immediately schedules another fetch with no cooldown.
- Phase-1 local-only deployment makes this practically free; future remote-host or flaky-network use exposes the storm risk.

**Proposal:**
- Add a per-session cooldown timestamp ("don't re-hydrate the same session within Nms of the last completed hydration unless the new delta carries a revision strictly greater than the one that started the previous hydration").
- Or document the burst as intentional given the local-only deployment cost; add a comment naming the trade-off so future reviewers don't keep flagging it.

## Watchdog-inversion tests don't assert the "Waiting for the next chunk of output…" affordance state

**Severity:** Low - `ui/src/App.live-state.deltas.test.tsx:3439` and `ui/src/App.live-state.watchdog.test.tsx:625`. The two recent inverted tests assert that the recovered text becomes visible, but say nothing about the "Waiting for the next chunk of output…" affordance. After the recovery snapshot adopts (the deltas test's snapshot has `status: "idle"`, the watchdog test's stays `status: "active"`), the affordance state is the most user-visible signal of whether recovery actually replaced the wedged UI vs just rendered the recovered text somewhere on the page.

**Proposal:**
- In `deltas.test.tsx`: add `expect(screen.queryByText("Waiting for the next chunk of output...")).not.toBeInTheDocument();` after the assertion that the recovered chunk is visible (recovery snapshot is idle, affordance should disappear).
- In `watchdog.test.tsx`: add an assertion clarifying expected affordance state for the still-active recovery (the assistant chunk now sits at the boundary, so the affordance should NOT be present).

## Rendered Markdown diff navigation does not scroll when there is exactly one change

**Severity:** Low - prev/next buttons can appear to do nothing for the common "one changed block" case.

`MarkdownDiffView` and `RenderedDiffView` intentionally skip the initial scroll so restored parent scroll position is preserved. Navigation scrolls from a `useEffect` keyed on the current index. When there is exactly one change/region, pressing next or previous computes the same index, React bails out of the state update, and the scroll effect does not run. The controls remain enabled but cannot bring the lone target into view.

**Current behavior:**
- Initial mount skips scrolling by design.
- One-change/one-region navigation resolves to the same index.
- No separate "navigation requested" signal exists to force a scroll to the same target.

**Proposal:**
- Drive the scroll side effect from a navigation request counter, or call an explicit scroll helper from the prev/next handlers.
- Cover both `MarkdownDiffView` and `RenderedDiffView` one-target cases.

## Rendered diff region navigation has no explicit scroll-container layout contract

**Severity:** Low - the new region-navigation ref may target a wrapper that is not the actual scroll container.

`RenderedDiffView` introduces `.diff-rendered-view-scroll` and queries it for `data-rendered-diff-region-index` targets, but the changed CSS does not give that wrapper an explicit flex/overflow contract, and the component does not adopt the existing `source-editor-shell source-editor-shell-with-statusbar` layout used by the Monaco and Markdown diff modes. If the parent remains the real scroller, `scrollIntoView()` may work inconsistently and the statusbar can diverge from the rest of the diff editor surface.

**Current behavior:**
- `RenderedDiffView` owns a new internal scroll ref.
- `.diff-rendered-view-scroll` has no explicit overflow/flex sizing.
- The rendered diff footer is not wrapped in the established editor shell/statusbar structure.

**Proposal:**
- Either adopt the existing editor-shell/statusbar layout contract or add explicit CSS that makes `.diff-rendered-view-scroll` the intended scroll container.
- Add a focused layout/navigation regression for rendered-region scrolling.

## Post-commit hardening helpers have no automated production-path coverage

**Severity:** Low - `src/persist.rs:213-227`. `verify_persist_commit_integrity` is `#[cfg(not(test))]`-only because it depends on production SQLite path hardening. The post-commit contract - redirection remains fatal, owner-only chmod/mode verification remains fatal unless `TERMAL_ALLOW_INSECURE_STATE_PERMISSIONS` is set - has no direct automated coverage.

**Proposal:**
- Expose a testable seam (e.g., inject the hardening function via a closure or trait), OR
- Add a Linux-only integration test that creates a real chmod-failing scenario.

## Watchdog wake-gap stops-after-progress invariant is not pinned

**Severity:** Low - `ui/src/backend-connection.test.tsx`. No direct negative-case test pins that watchdog wake-gap reconnect probes (which do NOT set `pendingBadLiveEventRecovery`) STOP after same-instance snapshot progress without a data-bearing SSE event. The cause-specific flag's whole premise is that wake-gap probes can stop while parse/reducer-error probes keep polling, but only the polling-continues side is pinned.

**Proposal:**
- Add a regression that triggers a watchdog wake-gap reconnect (no parse/reducer error), receives a same-instance progressed `/api/state` snapshot, advances `RECONNECT_STATE_RESYNC_MAX_DELAY_MS`, and asserts `countStateFetches()` did not increment.





## `app-live-state.ts` reconnect state machine continues to grow

**Severity:** Low - `ui/src/app-live-state.ts:2504 lines`. TS utility threshold (1500) exceeded; new `pendingBadLiveEventRecovery` adds another flag-shaped piece of reconnect bookkeeping. The reconnect/resync state machine inside `useEffect` now coordinates 6+ pieces of cross-cutting state.

**Proposal:**
- Extract a `ReconnectStateMachine` (or similar) module that owns the flag set + transitions and exposes named events (`onSseError`, `onSseReopen`, `onBadLiveEvent`, `onSnapshotAdopted`, `onLiveEventConfirmed`).
- Defer to a pure code-move commit per CLAUDE.md.


## `select_visible_session_hydration_fallback_error` lacks integration coverage

**Severity:** Low - `src/state_accessors.rs:351-369`. Unit tests pin the helper and typed local-miss fallback in isolation, but no integration-style test asserts the public `get_session` path returns 404 to the caller when a recoverable remote error is followed by a `not_found` fallback. A future refactor that drops the selector call from the `or_else` chain would not be caught.

**Current behavior:**
- Selector is unit-tested but the wiring that makes the new behavior reach a caller is not pinned.

**Proposal:**
- Add an integration-style test that drives the public `AppState::get_session` path through a recoverable remote hydration miss followed by a vanished cached summary, and asserts the response is `404 session not found` / `LocalSessionMissing` rather than the original recoverable remote error.

## `useAppSessionActions` ref cluster has grown from 1 to 4 to feed the rejected-action classifier

**Severity:** Medium - `ui/src/app-session-actions.ts:316-356`. `useAppSessionActions` now requires `latestStateRevisionRef`, `lastSeenServerInstanceIdRef`, `projectsRef`, and `sessionsRef` because of the inline `classifyRejectedActionState` call site. The ref count grew from 1 → 4 in a few rounds, all to feed one classifier function.

**Current behavior:**
- Every new evidence dimension for stale action snapshots pushes another ref into this hook.
- App.tsx, the test harness, and the hook signature all need editing whenever a new dimension is added.
- Same anti-pattern the resync-options ref cluster had before extraction.

**Proposal:**
- Pass a single `actionStateClassifierContextRef: MutableRefObject<{ revision, serverInstanceId, projects, sessions }>` (or a memoized snapshot getter) so adding a new evidence dimension does not require touching the hook signature, the caller, and the test harness.
- Defer to a dedicated commit per CLAUDE.md.

## `connectionRetryDisplayStateByMessageId` two-stage memoization is correct but threaded through ~4 stability hops

**Severity:** Medium - `ui/src/SessionPaneView.tsx:858-895`. The retry-display memoization now uses `signature → ref-cached map → useCallback wrapper → useSessionRenderCallbacks deps → MessageCard renderer identity`. The map identity stability invariant is load-bearing for `SessionBody` memoization but only documented sparsely. A future change to retry-display semantics needs to be threaded through ~4 separate stability hops.

**Current behavior:**
- Hand-rolled signature-stable memo bridges to a renderer that already had its own deps tax.
- Reviewers have flagged this as "complex invariant without nearby comments" several rounds in a row.

**Proposal:**
- Extract the signature-stable memo into a small `useStableMapBySignature` hook in a sibling utility module so the pattern is reusable and named.
- Or memoize directly on `(messages, status)` and accept one rebuild per message-list change — `MessageCard` is already memoized below the `SessionBody` memo gate.

## Directory-level state hardening retains a TOCTOU window after symlink check

**Severity:** Low - `src/persist.rs:146-149`. Round-15 carryover. `harden_local_state_directory_permissions` calls `reject_existing_state_directory_redirection_unix` (which uses `fs::symlink_metadata`), then `harden_local_state_permissions(path, 0o700)` — which uses path-based `fs::set_permissions` and `fs::metadata`, both of which follow symlinks. An attacker able to replace the directory between the two calls would get the chmod redirected through the symlink. The matching file path now uses `O_NOFOLLOW + fchmod`, but the directory path has not been migrated.

**Current behavior:**
- File-level chmod is symlink-safe (O_NOFOLLOW + fchmod).
- Directory-level chmod is not.
- Mitigated by Phase-1 single-user threat model (only the user controlling `~/` could plant the symlink).

**Proposal:**
- Open the directory with `O_DIRECTORY | O_NOFOLLOW`, then `fchmod` on the resulting fd; or use `fchmodat(AT_FDCWD, path, mode, AT_SYMLINK_NOFOLLOW)`.

## `AgentSessionPanel.test.tsx` new tests duplicate ~70 lines of harness setup

**Severity:** Low - `ui/src/panels/AgentSessionPanel.test.tsx:369-505`. The new "refreshes command/diff cards when only the renderer changes" tests replicate ~70 lines of harness setup each. Boilerplate increases drift risk.

**Proposal:**
- Extract a `renderAgentSessionPanelHarness` helper that takes only the props that vary (viewMode, message arrays, renderer overrides) and supplies the noop defaults internally.

## `App.live-state.reconnect.test.tsx` does not pin the "latest assistant message stays hidden until another action" invariant

**Severity:** Low - The closest coverage at lines 1505 and 1582 pins reconnect polling/disarm timing, but does not assert the visibility side. A regression that exposes the latest assistant message before SSE confirms could slip in.

**Proposal:**
- Add a test that dispatches a fallback `_sseFallback` snapshot containing only the user prompt (no assistant reply yet), confirms the assistant message is not shown, then either adopts a fresh SSE state event with the assistant text or simulates "another action" and asserts visibility.

## Non-optimistic user-prompt display causes 100-300ms felt lag on every Send

**Severity:** Medium - `ui/src/app-session-actions.ts:851-895` and `ui/src/app-live-state.ts:1283-1385`. The composer is non-optimistic: clicking Send clears the textarea, fires `await sendMessage(...)`, and then runs `adoptState(state)` against the full `StateResponse` returned by the POST. The "you said X" card only appears after the round-trip plus the heavy `adoptState` walk completes.

`adoptState` re-derives codex, agentReadiness, projects, orchestrators, workspaces, and walks transcripts on the main thread. On a focused active session this lands in the 100-300ms range every send (longer when an active turn is mid-stream). The codebase has already self-diagnosed the path in `docs/prompt-responsiveness-refactor-plan.md` but no optimistic-insert fix has landed.

The lag compounds with two existing tracked bugs ("Focused live sessions monopolize the main thread during state adoption", "Composer drafts have three authoritative stores") but is itself a separable contributor.

**Current behavior:**
- User clicks Send -> textarea clears -> POST fires -> response returns -> `adoptState` walks -> card paints.
- Total delay: round-trip (typically 30-100ms locally) + adoptState (50-200ms on focused live sessions) = visible 100-300ms gap.
- During the gap the session shows neither the user prompt nor the composer text.

**Proposal:**
- Insert an optimistic user-message card in `handleSend` before `await sendMessage(...)`, keyed by a temp id.
- When the POST response arrives or the SSE `messageCreated` delta lands (whichever is first), reconcile by id (swap temp id for server-assigned `messageId`).
- This collapses the round-trip and the adoptState walk out of the felt-lag path simultaneously.
- Cross-link to `docs/prompt-responsiveness-refactor-plan.md` and decide whether this is a standalone fix or folds into the larger refactor.

## `applyDeltaToSessions` duplicates the "lookup first, metadata-only fallback when missing" pattern across five non-created delta types

**Severity:** Low - `ui/src/live-updates.ts:329-599`. The reordered `messagesLoaded === false` branch (apply to in-memory message when present, fall back to metadata-only only when `messageIndex === -1`) is now repeated five times across `messageUpdated`, `textDelta`, `textReplace`, `commandUpdate`, and `parallelAgentsUpdate`. The previous code had the same shape duplicated five times in the wrong order; the new code is now the right order duplicated five times. A future sixth retained-non-created delta type will need to re-derive the same flow.

**Current behavior:**
- Each branch independently re-implements `findMessageIndex` -> `if (-1 && !messagesLoaded) metadata-only` -> `if (-1) needsResync` -> type-narrow -> apply.
- The existing duplication is what let the fallback land in the wrong order originally; the next protocol addition has the same cliff.

**Proposal:**
- Extract a `tryApplyMetadataOnlyFallbackForMissingTarget(session, sessionIndex, sessions, delta)` helper (or similar) that centralizes the missing-target/unhydrated decision so each delta type calls a single helper instead of inlining the same branch.
- Defer to a dedicated pure-code-move commit per CLAUDE.md.

## `live-updates.test.ts` "applies retained non-created deltas" bundles five delta types into one `it()` block

**Severity:** Low - `ui/src/live-updates.test.ts:1655-1888`. The new "applies retained non-created deltas while the transcript is marked unhydrated" test covers `messageUpdated`, `textDelta`, `textReplace`, `commandUpdate`, and `parallelAgentsUpdate` serially within a single `it(...)`. If an early assertion fails, downstream cases never run and the failure trace doesn't pinpoint which delta type regressed.

**Current behavior:**
- Five distinct scenarios share one `it` block with sequential `expect` blocks.
- The companion metadata-only-fallback test at lines 1890-1929 covers only `textDelta` for the missing-target path (already tracked as a P2 backlog item).

**Proposal:**
- Split into five `it(...)` blocks (one per delta type) or use `it.each(...)` with a table that mirrors the existing P2 missing-target task.
- Pure mechanical change.

## Production SQLite persistence is bypassed in the test build

**Severity:** Medium - `src/app_boot.rs:229`. The runtime persistence changes now depend on SQLite schema setup, startup load, metadata writes, per-session row updates, tombstone cleanup, and cached delta persistence, but `#[cfg(test)]` still routes the background persist worker through the old full-state JSON fallback.

Many production SQLite helpers in `src/persist.rs` are `#[cfg(not(test))]`, so existing persistence tests can pass while the real runtime SQLite write/load/delete behavior remains unexercised. The newest post-commit hardening policy (`verify_persist_commit_integrity`, fatal owner-only permission verification, cache invalidation reset, and fatal pre-transaction redirection checks) is part of that production-only surface.

**Current behavior:**
- Test builds bypass `persist_delta_via_cache` and related SQLite write paths.
- Production SQLite load/save helpers are mostly compiled out under `cargo test`.
- Current tests cover retry bookkeeping and legacy JSON fixtures, but not the runtime SQLite persistence contract or the post-commit hardening decisions.

**Proposal:**
- Make the SQLite persistence path testable under `cargo test` with temp database files.
- Add coverage for full snapshot save/load, delta upsert, metadata-only update, hidden/deleted session row removal, and startup load from SQLite.
- Add coverage for post-commit permission failures, cache invalidation reset, and fatal redirection/reparse checks.
- Keep legacy JSON fixture tests separate from production runtime persistence tests.

## `SessionPaneView.tsx` and `app-session-actions.ts` past architecture file-size thresholds

**Severity:** Low - `ui/src/SessionPaneView.tsx` is now 3,160 lines and `ui/src/app-session-actions.ts` is 1,968 lines, both past the architecture rubric §9 thresholds (~2,000 for TSX components, ~1,500 for utility modules). The round-11 extractions of `connection-retry.ts`, `app-live-state-resync-options.ts`, `session-hydration-adoption.ts`, and `SessionPaneView.render-callbacks.tsx`, plus the later `action-state-adoption.ts` split, reduced these files but left them over their respective thresholds.

The companion `app-live-state.ts` entry already exists; this captures the two related Phase-2 candidates that emerged after the round-11 splits.

**Current behavior:**
- `SessionPaneView.tsx` mixes pane orchestration with reconnect-card / waiting-indicator / retry-display orchestration.
- `app-session-actions.ts` still mixes action handlers with optimistic-update and adoption-outcome side-effect wiring.
- Both files now have natural extraction boundaries with their own existing direct unit-test coverage.

**Proposal:**
- Pure code move per CLAUDE.md, in dedicated split commits (one per file).
- For `SessionPaneView.tsx`: candidate is the reconnect-card / waiting-indicator computation cluster.
- For `app-session-actions.ts`: candidate is the optimistic-update + adoption-outcome side-effect cluster now that pure stale target evidence has moved out.

## `App.live-state.deltas.test.tsx` past 2,000-line review threshold

**Severity:** Low - `ui/src/App.live-state.deltas.test.tsx`. File is now 3,435 lines and 18 `it` blocks after this round's cross-instance regression coverage, well past the architecture rubric §9 ~2,000-line threshold for TSX files. The header already lists three sibling files split out (`reconnect`, `visibility`, `watchdog`), establishing the per-cluster split pattern.

The newest tests still cluster around hydration/restart races and cross-instance recovery, which is a coherent split boundary. Pure code move per CLAUDE.md.

**Current behavior:**
- Single test file mixes hydration races, watchdog resync, ignored deltas, orchestrator-only deltas, scroll/render coalescing, and resync-after-mismatch flows.
- 18 `it` blocks; the newest coverage adds another cross-instance state-adoption scenario.
- Per-cluster grep tax growing.

**Proposal:**
- Pure code move: extract the 4–5 hydration-focused tests into `ui/src/App.live-state.hydration.test.tsx`, mirroring the sibling-split pattern.
- Defer to a dedicated split commit; do not couple with feature changes.

## `app-live-state.ts` past 1,500-line review threshold for TypeScript utility modules

**Severity:** Low - `ui/src/app-live-state.ts`. File is now 2,435 lines after this round. The architecture rubric §9 sets a pragmatic ~1,500-line threshold for TypeScript utility modules. The hydration adoption helpers have moved out, but the module still mixes retry scheduling, profiling, JSON peek helpers, and the main state machine.

**Current behavior:**
- Single module mixes hydration matching, retry scheduling, profiling, JSON peek helpers, and the main state machine.
- Per-cluster grep tax growing with each round.

**Proposal:**
- Defer to a dedicated pure-code-move commit per CLAUDE.md.
- Extract `hydration-retention.ts` (or `session-hydration.ts`) containing `hydrationRetainedMessagesMatch`, `SESSION_HYDRATION_RETRY_DELAYS_MS`, `SessionHydrationTarget`, `SessionHydrationRequestContext`, and the matching unit tests.

## `AgentSessionPanel.test.tsx` past 5,000-line review threshold

**Severity:** Low - `ui/src/panels/AgentSessionPanel.test.tsx`. File is now 5,659 lines (+511 this round), past the project's review threshold for test files. The added blocks cluster naturally by concern — composer memo coverage, scroll-following coverage, ResizeObserver fixtures — and would extract cleanly into siblings without behavioral change.

The adjacent `App.live-state.*.test.tsx` split (April 20) is the precedent for per-cluster `.test.tsx` files. Per `CLAUDE.md`, splits must be pure code moves and live in their own commit.

**Current behavior:**
- Single `AgentSessionPanel.test.tsx` mixes composer, scroll, resize, and lifecycle clusters.
- Per-cluster grep tax growing with each replay-cache-adjacent feature round.

**Proposal:**
- Pure code move: extract into `AgentSessionPanel.composer.test.tsx`, `AgentSessionPanel.scroll.test.tsx`, `AgentSessionPanel.resize.test.tsx` (matching the App.live-state cluster shape).
- Defer to a dedicated split commit; do not couple with feature changes.


## `src/tests/remote.rs` past the 5,000-line review threshold

**Severity:** Low - `src/tests/remote.rs` is now 9,202 lines after this round's +471-line addition, well past the project's review-threshold for test files. The new replay-cache work clusters cohesively between lines ~2,810 and ~4,040 (the `RemoteDeltaReplayCache` shape helper, the `local_replay_test_remote` / `seed_loaded_remote_proxy_session` / `assert_delta_publishes_once_then_replay_skips` / `assert_remote_delta_replay_cache_shape` / `test_remote_delta_replay_key` helpers, and the `remote_delta_replay_*` tests).

The growth is incremental across many rounds of replay-cache hardening, not a single landing — but extracting the cluster keeps the rest of the file's per-test density manageable. Per `CLAUDE.md`, splits must be pure code moves and live in their own commit.

**Current behavior:**
- Single `src/tests/remote.rs` mixes hydration tests, orchestrator-sync tests, replay-cache tests, and protocol-shape tests.
- Per-cluster grep is harder than necessary; future replay-cache work continues to grow the file.

**Proposal:**
- Extract the replay-cache cluster (lines ~2,810–4,040) into `src/tests/remote_delta_replay.rs` as a pure code move — including the helpers and all `remote_delta_replay_*` tests.
- Defer to a dedicated split commit; do not couple with feature changes.

## `SourcePanel.tsx` is growing along a separable axis

**Severity:** Low - `ui/src/panels/SourcePanel.tsx` grew from ~803 to 1119 lines in this round (+316). It is approaching but has not crossed the ~2,000-line scrutiny threshold. The new responsibility (rendered-Markdown commit pipeline orchestration: collect — resolve ranges — check overlap — reduce edits — re-emit with EOL style) is meaningfully separable from the existing source-buffer/save/rebase/compare orchestration. It has its own state (`hasRenderedMarkdownDraftActive`, `renderedMarkdownCommittersRef`), pure helpers already split into `markdown-commit-ranges`/`markdown-diff-segments`, and a clean parent-callback interface.

**Current behavior:**
- SourcePanel owns two distinct orchestration responsibilities in one component.

**Proposal:**
- No action this commit. Consider extracting a `useRenderedMarkdownDrafts(fileStateRef, editorValueRef, setEditorValueState, ...)` hook in a follow-up, owning `renderedMarkdownCommittersRef`, `hasRenderedMarkdownDraftActive`, `commitRenderedMarkdownDrafts`, `handleRenderedMarkdownSectionCommits`, and `handleRenderedMarkdownSectionDraftChange`.
- The hook would expose a small surface for SourcePanel to consume and keep the file under the scrutiny threshold.

## Metadata-first summaries make transcript search incomplete

**Severity:** Medium - search can silently miss transcript matches for sessions that have only metadata summaries loaded.

`/api/state` now returns session summaries with `messages: []` and
`messagesLoaded: false`. The session search index still walks
`session.messages` directly, so non-visible sessions can be treated as having
no searchable transcript even though the transcript simply has not been
hydrated in this browser view.

**Current behavior:**
- `ui/src/session-find.ts` builds transcript search items from
  `session.messages`.
- Metadata-first session summaries clear `messages` before reaching the
  frontend.
- Search has no "transcript not loaded" state and no on-demand hydration path
  before concluding that there are no message matches.

**Proposal:**
- Gate transcript search to hydrated sessions and surface incomplete results
  when a session summary is not loaded.
- Or hydrate/index target sessions on demand when search needs transcript
  content.
- Add coverage proving metadata-only summaries do not silently produce false
  "no transcript match" results.

## Metadata-first state summaries still broadcast full pending prompts

**Severity:** Low - transcript payloads were removed from global state, but queued prompt text can still ride along with every session summary.

Metadata-first state summaries clear `messages`, but the session summary still
includes full pending-prompt data. Queued prompts can contain user-authored
instructions or expanded prompt content, so this remains a smaller but real
data-minimization leak in `/api/state` and SSE `state` broadcasts.

**Current behavior:**
- `src/state_accessors.rs` builds transcript-free summaries but keeps the full
  `pending_prompts` projection.
- Every listening tab can receive pending prompt content for sessions it is not
  actively hydrating.

**Proposal:**
- Project pending prompts to a bounded metadata-only summary in `StateResponse`.
- Keep full queued-prompt content on targeted full-session responses where the
  active pane actually needs it.

## Hydration retry loop can spam persistent failures

**Severity:** Low - visible-session hydration retries clamp to the last retry delay and can continue indefinitely for persistent non-404 failures.

The new retry loop correctly recovers from stale hydration rejection and transient `fetchSession` failures, but it has no ceiling. A visible metadata-only session whose targeted hydration keeps failing will retry every 3 seconds and repeatedly call the normal request-error reporting path.

**Current behavior:**
- `ui/src/app-live-state.ts` schedules retry delays of 50 ms, 250 ms, 1000 ms, then 3000 ms, and clamps all later retries to 3000 ms.
- Non-404 `fetchSession` failures report the request error and schedule another retry.
- The transient non-404 failure branch is not covered by a regression test.

**Proposal:**
- Cap repeated user-facing error reporting or retry attempts for the same visible session while keeping event-driven or manual recovery possible.
- Add a test where the first `/api/sessions/{id}` request fails with a non-404 error, the retry succeeds, and the transcript appears without a tab switch or unrelated state event.

## Remote test module size slows review and triage

**Severity:** Note - `src/tests/remote.rs` is large enough that focused remote
review now has to scan many unrelated scenarios.

The file contains hydration, delta, orchestrator, proxy, and sync-gap coverage
in one module. New hydration/replay tests are coherent, but keeping every remote
scenario in the same file makes future review targeting and regression triage
harder, especially as the metadata-first remote work continues adding focused
cases.

**Current behavior:**
- Remote tests for several boundaries live in one oversized module.
- New review findings repeatedly point into the same large file, making
  ownership and intended fixture reuse harder to see.

**Proposal:**
- Split remote tests by boundary, for example `remote_hydration.rs`,
  `remote_deltas.rs`, and `remote_orchestrators.rs`.
- Move shared fake-server and remote-session helpers into a small support
  module used by those test files.

## Session store publication can race ahead of React session state

**Severity:** Medium - the new `session-store` publishes some session slices before the corresponding React `sessions` state commits, so the UI can mix newer store-backed session data with older prop-derived session state in one render.

The staged refactor publishes `session-store` updates directly from
`ui/src/app-live-state.ts` and `ui/src/app-session-actions.ts`, while other
parts of the active pane still derive session data from React state in
`ui/src/SessionPaneView.tsx`. That leaves two live sources of truth on slightly
different timelines: `AgentSessionPanel` / `PaneTabs` can read the new store
snapshot immediately, while sibling props such as `commandMessages`,
`diffMessages`, waiting-indicator state, and other session-derived metadata are
still coming from the previous React `sessions` commit.

**Current behavior:**
- `session-store` is synced directly from live-state/action paths before some
  `setSessions(...)` commits land.
- `AgentSessionPanel` and `PaneTabs` read session data from the store.
- `SessionPaneView` still derives other active-session slices from React state,
  so the same active pane can render mixed-version session data within one
  update.

**Proposal:**
- Keep store publication aligned with committed React state, or finish moving
  the remaining active-session derivations in `SessionPaneView` onto the same
  store boundary.
- Document which layer is authoritative during the transition so later changes
  do not deepen the split-brain state model.
- Add an integration test that forces a store-backed session update plus a
  lagging React-state-derived sibling prop and asserts the active pane never
  renders a torn combination.

## Deferred heavy-content activation is coupled into the message-card renderer

**Severity:** Low - `ui/src/message-cards.tsx` now owns deferred heavy-content
activation policy in addition to Markdown, code, Mermaid, KaTeX, diff, and
message-card composition concerns.

The new provider/hook is useful, but keeping the virtualization activation
contract embedded in the same large renderer increases coupling between scroll
policy and message rendering. Future performance fixes will have to reason
through a broad module instead of a small boundary with a clear contract.

**Current behavior:**
- Deferred activation context, heavy Markdown/code rendering, and message-card
  composition live in one large module.
- Virtualization policy reaches into message rendering through exported
  activation context.
- The ownership boundary is not documented near the exported provider.

**Proposal:**
- Extract the deferred activation provider/hook into a focused module with a
  short contract comment.
- Consider extracting the heavy Markdown/code rendering path separately so
  virtualization policy and content rendering can evolve independently.

## `CodexUpdated` delta carries a full subsystem snapshot despite the "delta" name

**Severity:** Medium - `src/wire.rs::DeltaEvent::CodexUpdated { revision, codex: CodexState }` publishes the entire `CodexState` on every rate-limit tick and every notice addition. The architectural contract the codebase otherwise respects is "state events for full snapshots, delta events for scoped changes". `CodexUpdated` is small today (rate_limits + notices capped at 5), but the naming invites future bulky additions to `CodexState` (login state, model-availability maps, per-provider metadata) to be broadcast in full on every tiny change.

**Current behavior:**
- The variant ships a full `CodexState` payload.
- Two publish sites in `src/session_sync.rs` send the complete snapshot even when only the rate limits changed.
- Wire name and shape set a precedent for "delta = tiny changes" that this variant violates.

**Proposal:**
- Split into narrower variants: `CodexRateLimitsUpdated { revision, rate_limits }` and `CodexNoticesUpdated { revision, notices }`. The two call sites in `session_sync.rs` already pick their publish trigger, so split dispatch is straightforward.
- Alternatively, add a source-level comment on the `CodexUpdated` variant stating that `codex` is intentionally the full subsystem snapshot and any future field addition to `CodexState` must reconsider whether a narrower event is needed.

## `DeferredHeavyContent` near-viewport activation now deferred by one paint

**Severity:** Low - `ui/src/message-cards.tsx:607-628` replaced `useLayoutEffect` with `useEffect` + a `requestAnimationFrame` before `setIsActivated(true)` for the near-viewport fast-activation branch. The previous sync layout-effect path activated heavy content that was already in-viewport before paint, avoiding a placeholder — content height jump. The new path defers activation by at least one paint, so on initial mount near the viewport the user may now see the placeholder for one frame before the heavy content replaces it. The deleted comment specifically warned about this risk for virtualized callers.

**Current behavior:**
- `useEffect` + `requestAnimationFrame` defers activation by ~1 paint even when the card is already near viewport on mount.
- The deferral was added as part of the `allowDeferredActivation` cooldown gate (to avoid layout thrash during active scrolls).
- Near-viewport mount activation now produces a one-frame placeholder flicker in place of the previous zero-frame activation.

**Proposal:**
- Use `useLayoutEffect` when `allowDeferredActivation === true` (or for the near-viewport branch generally). Keep the `requestAnimationFrame` in the IntersectionObserver entry path for rapid-entry de-dupe.
- Alternatively, add a targeted comment explaining the deliberate trade-off if the new behavior is intended.

## `"sessionId" in delta` poll-cancel branches are not extensible

**Severity:** Low - `ui/src/app-live-state.ts:1613, 1633` handle delta-event poll cancellations by structurally checking `"sessionId" in delta`. The two `revisionAction === "ignore"` / `"resync"` branches each hard-code the knowledge that only `SessionDeltaEvent` variants carry `sessionId`. Adding a third non-session delta type requires remembering to update both branches, and a new session-scoped delta that uses a different key (e.g. `sessionIds: string[]`) would silently miss both gates.

**Current behavior:**
- Two branches each run `"sessionId" in delta && typeof delta.sessionId === "string"`.
- The `SessionDeltaEvent` exclude type in `ui/src/live-updates.ts:76` exists but is not used here.

**Proposal:**
- Extract a `cancelPollsForDelta(delta: DeltaEvent)` helper that switches on `delta.type` (or uses the same `SessionDeltaEvent` narrowing). Call it from both branches.
- That also centralizes the "which deltas cancel which polls" contract in one place.

## `prevIsActive`-in-render replaced with post-commit effect delays the first-activation measurement pass

**Severity:** Low - `ui/src/panels/VirtualizedConversationMessageList.tsx:426-432` converted the `prevIsActive !== isActive` render-time derived-state update into a post-commit `useEffect`. Under the previous pattern, a session switching from `isActive: false — true` flipped `setIsMeasuringPostActivation(true)` during render, so the first frame rendered the measuring shell with the correct `preferImmediateHeavyRender` value. The new effect defers that flip to after commit — the first paint of the newly-active session briefly shows `isMeasuringPostActivation: false`, flipping to the measurement shell only on the next render.

Usually invisible (the effect runs the same tick). Under slow devices this may cause a one-frame flicker on session activation.

**Current behavior:**
- Post-commit effect fires after the first frame of the reactivated session.
- First paint uses `isMeasuringPostActivation: false` regardless of the actual transition.

**Proposal:**
- Restore the render-time pattern: `if (prevIsActive !== isActive) { setPrevIsActive(isActive); ... }` (the established React "derived state" form).
- Or upgrade the effect to `useLayoutEffect` so it runs before paint.
- The P2 task for `key={sessionId}` on the virtualizer supersedes this if that fix lands first.

## Focused live sessions monopolize the main thread during state adoption

**Severity:** Medium - a visible, focused TermAl tab with an active Codex session can spend multiple seconds of an 8 s sample on main-thread work even when no requests fail and no exceptions fire.

A live Chrome profile against the current dev tab showed no runtime exceptions, no failed network requests, and no framework error overlay, but the page still burned about `6.6 s` of `TaskDuration`, `0.97 s` of `ScriptDuration`, `372` style recalculations, and several long tasks above `2 s` while Codex was active. The hottest app frames were `handleStateEvent(...)` in `ui/src/app-live-state.ts`, `request(...)` / `looksLikeHtmlResponse(...)` in `ui/src/api.ts`, `reconcileSessions(...)` / `reconcileMessages(...)` in `ui/src/session-reconcile.ts`, `estimateConversationMessageHeight(...)` in `ui/src/panels/conversation-virtualization.ts`, and repeated `getBoundingClientRect()` reads in `ui/src/panels/VirtualizedConversationMessageList.tsx`. A second targeted typing profile pointed the same way: 16 simulated keystrokes averaged only about `1.0 ms` of synchronous input work and about `11 ms` to the next frame, while `handleStateEvent(...)` alone still consumed about `199 ms` of self time. Narrower composer-rerender and adoption-fan-out regressions have been fixed separately, but the remaining profile still points at broader whole-tab churn.

**Current behavior:**
- A visible, focused active session still produces repeated long main-thread tasks while Codex is working or waiting for output.
- Per-chunk session deltas now coalesce their full-session store publication and broad `sessions` render update to one animation frame, but full state snapshots and transcript measurement still need separate cuts.
- `codexUpdated` deltas and same-value backend connection-state updates are now coalesced or ignored, but snapshot adoption remains the dominant unresolved path.
- Slow `state` events now log per-phase timings in development, so the next profiling round should use the `[TermAl perf] slow state event ...` line to pick the next cut.
- Stale same-instance snapshots now avoid full JSON parse, so the remaining problematic lines should be adopted snapshots or server-restart/fallback snapshots.
- `handleStateEvent(...)` still drives broad adoption work through `adoptState(...)` / `adoptSessions(...)`, transcript reconciliation, and follow-on measurement/render work even after the narrower cleanup fan-out cut.
- `/api/state` resync currently reads full response bodies as text and runs `looksLikeHtmlResponse(...)` before JSON parsing, adding avoidable CPU on large successful snapshots.
- Transcript virtualization still spends measurable time on regex-heavy height estimation and synchronous layout reads, so live session churn compounds with scroll/measure work instead of staying isolated to the active status surface.

**Proposal:**
- Make the live state path more metadata-first so transcript arrays, workspace layout, and per-session maps are not reconciled or pruned when the incoming snapshot did not materially change those slices.
- Split the `/api/state` response handling into a cheap JSON-first path and keep HTML sniffing on a narrow error/prefix check instead of scanning whole successful payloads.
- Cache height-estimation inputs by message identity/revision and reduce repeated `getBoundingClientRect()` passes in the virtualized transcript.
- Re-profile the focused active-session path after each cut and keep this issue open until long-task bursts drop back below user-visible jank thresholds.

**Plan:**
- Start at the root of the profile: cut `handleStateEvent(...)` / `adoptState(...)` work first, because that is where both the passive and targeted rounds spend the most app CPU.
- Break the work into independently measurable slices: state adoption fan-out, `/api/state` parsing path, and transcript virtualization measurement/estimation.
- After each slice lands, rerun the live active-session profile and the focused typing round so reductions in `handleStateEvent(...)` self time, `TaskDuration`, and next-frame latency are verified instead of assumed.

## Composer drafts have three authoritative stores

**Severity:** Medium - committed composer drafts are tracked in React state (`draftsBySessionId`), a mutable ref (`draftsBySessionIdRef`), and the new `useSyncExternalStore`-backed `session-store`, with a post-commit effect mirroring state → ref and imperative paths writing the ref before React commits. Under concurrent draft updates the deferred effect can overwrite a newer ref value with a stale committed one, which then propagates to the composer snapshot via `syncComposerDraftForSession`.

`ui/src/session-store.ts` added a third source of truth for per-session drafts. Imperative handlers in `ui/src/app-session-actions.ts` (`handleDraftChange`, `sendPromptForSession`, queue-prompt flows) and `ui/src/app-workspace-actions.ts` write `draftsBySessionIdRef.current` synchronously before calling `setDraftsBySessionId`, so the store sync reads the fresh value. A separate effect in `ui/src/App.tsx` copies `draftsBySessionId` back into the ref after each commit. When two draft updates land in the same tick, the later-committed effect can briefly regress the ref to an older snapshot, and the store's composer-snapshot slice (`syncComposerDraftForSession`) can publish that stale draft to subscribers.

**Current behavior:**
- Three stores own the same data: React state, the ref, and the `session-store` slice.
- Imperative paths write ref → store before React commits; the effect writes state → ref after commit.
- Under concurrent updates the effect can stomp a newer imperative write with a stale React-committed value.

**Proposal:**
- Pick one owner for the ref: either drop the post-commit effect and rely entirely on imperative writes, or remove the imperative ref mutations and let the store read through a ref that mirrors state exactly once per commit.
- Document the invariant in the `session-store.ts` header so future changes do not reintroduce a third writer.
- Add a regression test that drives two overlapping `handleDraftChange` calls in the same tick and asserts the store snapshot matches the last-written value.

## Composer sizing double-resets on session switch

**Severity:** Low - `ui/src/panels/AgentSessionPanel.tsx:918-931` runs `resizeComposerInput(true)` synchronously inside a `useLayoutEffect` keyed on `[activeSessionId]`, and a following `useEffect` keyed on `[composerDraft]` schedules another resize via `requestAnimationFrame` on the same first render. The rAF resize is redundant because the synchronous one already measured the new metrics.

**Current behavior:**
- Layout effect resets cached sizing state and calls `resizeComposerInput(true)` synchronously.
- Draft effect schedules a second `requestAnimationFrame` resize on the same first render.
- First render of any newly-activated session does two resize passes instead of one.

**Proposal:**
- Track a "just-resized-synchronously" flag set in the layout effect and checked at the top of `scheduleComposerResize`, or gate the draft effect with a prev-draft ref so the "initial draft equals committed" case is a no-op.

## Duplicated `Session` projection types in `session-store.ts` and `session-slash-palette.ts`

**Severity:** Low - `ComposerSessionSnapshot` (`ui/src/session-store.ts:36-83`) and `SlashPaletteSession` (`ui/src/panels/session-slash-palette.ts:51-65`) each re-pick overlapping-but-non-identical field sets from `Session`. Three `Session`-like shapes now exist (`Session`, `ComposerSessionSnapshot`, `SlashPaletteSession`) with no compile-time check that additions to `Session` reach both projections — a new agent setting added to `Session` could silently default to `undefined` in consumers that read through either projection.

**Current behavior:**
- Both projection types declare field lists by hand.
- No `Pick<Session, ...>` derivation; nothing fails to compile when `Session` grows a new field.

**Proposal:**
- Derive both types via `Pick<Session, ...>`, or express `SlashPaletteSession` as `Omit<ComposerSessionSnapshot, ...>` where their field sets differ.
- Colocate the derivations in `session-store.ts` so the projection contract is visible in one place.

## `resolvedWaitingIndicatorPrompt` duplicates `findLastUserPrompt` derivation across `SessionBody` and `SessionPaneView`

**Severity:** Low - `ui/src/panels/AgentSessionPanel.tsx:399-404` computes `resolvedWaitingIndicatorPrompt` by calling `findLastUserPrompt(activeSession)` inside `SessionBody` whenever the live turn indicator is showing, overriding the `waitingIndicatorPrompt` prop that `ui/src/SessionPaneView.tsx:795-805` already computed via the same helper and `useMemo`. The override was added to pick up store-subscriber updates between parent renders (correct intent), but it leaves two parallel code paths that must be kept in sync.

Two smaller concerns ride along:
- The override's condition includes an `"approval"` status arm (`status === "active" || status === "approval"`) that is presently unreachable: `SessionPaneView` only sets `showWaitingIndicator=true` when `status === "active"` or (`!isSessionBusy && isSending`), and `isSessionBusy` is true for `"approval"`, so `showWaitingIndicator && status === "approval"` never holds. Harmless defensive check but misleading for readers inferring the truth table.
- The resolution is not wrapped in `useMemo`, so it re-runs on every `SessionBody` re-render — once per streaming chunk. `findLastUserPrompt` scans from the tail, so it usually stops early, but sessions dominated by trailing tool/assistant output could scan deep.

**Current behavior:**
- `SessionBody` (`AgentSessionPanel.tsx:399-404`) and `SessionPaneView` (`SessionPaneView.tsx:795-805`) both derive the waiting-indicator prompt by calling `findLastUserPrompt(activeSession)` on the same store record.
- The override runs on every `SessionBody` render, uncached.
- The `status === "approval"` arm of the override's condition is unreachable under current upstream gating.

**Proposal:**
- Collapse to one computation at the store-subscriber boundary. Either `SessionBody` becomes the sole resolver (drop the `useMemo` and prop passthrough in `SessionPaneView`), or add a one-line cross-reference comment on both sites so future readers know the two are paired.
- Narrow the override's condition to `status === "active"` to match the upstream truth table.
- Wrap the override in `useMemo(() => findLastUserPrompt(activeSession), [activeSession.messages])` to avoid re-scanning on every streaming chunk.

## Conversation cards overlap for one frame during scroll through long messages

**Severity:** Medium - `estimateConversationMessageHeight` in `ui/src/panels/conversation-virtualization.ts` produces an initial height for unmeasured cards using a per-line pixel heuristic with line-count caps (`Math.min(outputLineCount, 14)` for `command`, `Math.min(diffLineCount, 20)` for `diff`) and overall ceilings of 1400/1500/1600/1800/900 px. For heavy messages — review-tool output, build logs, large patches — the estimate is 20–40% under the rendered height, so `layout.tops[index]` for cards below an under-priced neighbour places them inside the neighbour's rendered area. The user sees the cards painted on top of each other for one frame, until the `ResizeObserver` measurement lands and `setLayoutVersion` rebuilds the layout.

An initial attempt to fix this by raising estimates to a single 40k px cap (and adding `visibility: hidden` per-card until measured) was reverted after it introduced two worse regressions: (1) per-card `visibility: hidden` combined with the wrapper's `is-measuring-post-activation` hide left the whole transcript empty for a frame whenever the virtualization window shifted before measurements landed; (2) raising the cap made the `getAdjustedVirtualizedScrollTopForHeightChange` shrink-adjustment huge (40k estimate − 8k actual = −32k scrollTop jump), so slow wheel-scrolling through heavy transcripts caused visible scroll jumps of tens of thousands of pixels. The revert restores the one-frame overlap as the known limitation.

**Current behavior:**
- Initial layout uses estimates that badly under-price long commands / diffs.
- First paint places subsequent cards overlapping the under-priced one for one frame.
- Next frame, `ResizeObserver` fires, `setLayoutVersion` rebuilds, positions correct.
- Visible to the user as a brief "jumble" during scroll.

**Proposal:**
- Proper fix likely needs off-screen pre-measurement (render the card in a hidden measure-only tree, read `getBoundingClientRect` height, then place in the layout) rather than a formula-based estimate. This is a bigger change than a single pure-function tweak.
- Alternative: batch-measurement pass when the virtualization window shifts — hide the wrapper briefly, mount the newly-entering cards, wait for all their measurements, then reveal.
- Not: raise the estimator cap. Large overshoots trade one visible artifact for a worse one.


## Hard kill (SIGKILL, power loss) can still lose the last un-drained persist write

**Severity:** Low - restarting the backend process while the browser tab is still open can make the most recent assistant message disappear from the UI, because the persist thread has a small window between "commit fires" and "row is durably in SQLite" during which an un-drained mutation is lost on kill.

Persistence is intentionally background and best-effort: every `commit_persisted_delta_locked` (and similar delta-producing commit helpers) signals `PersistRequest::Delta` to the persist thread and returns. The thread then locks `inner`, builds the delta, and writes. If the backend process is killed (SIGKILL, laptop sleep wedge, crash, manual restart of the dev process) between the signal fire and the SQLite commit, the mutation is lost. Old pre-delta-persistence behavior had the same window — the persist channel carried a full-state clone — so this is not a regression introduced by the delta refactor, but the symptom is visible now because the reconnect adoption path applies the persisted state with `allowRevisionDowngrade: true`: the browser's in-memory copy of the just-streamed last message is replaced by the freshly loaded (older) backend state, making the message disappear from the UI.

The message is not hidden; it is genuinely gone from SQLite. No amount of frontend re-rendering will bring it back.

**Current behavior:**
- Active-turn deltas (e.g., streaming assistant text, `MessageCreated` at the end of a turn) commit through `commit_persisted_delta_locked`, which only signals the persist thread.
- The persist thread acquires `inner` briefly, collects the delta, and writes to SQLite.
- Between "signal sent" and "row written" there is a small time window (usually sub-millisecond, but can stretch under contention) during which a hard kill of the backend loses the mutation.
- On backend restart + SSE reconnect, the browser's `allowRevisionDowngrade: true` adoption path applies the persisted state. The persisted state is missing the un-drained mutation, so the in-memory latest message is overwritten and disappears.

**Proposal:**
- The user-initiated restart path (Ctrl+C / SIGTERM) is now covered by the graceful-shutdown drain — see the preamble.
- For the residual hard-kill case (SIGKILL, power loss): consider opt-in synchronous persistence for the last message of a turn — the turn-completion commit (`finish_turn_ok_if_runtime_matches`'s `commit_locked`) could flush synchronously before returning, trading a few ms of latency on turn completion for zero-loss durability of the final message.
- Or accept and document this as a known Phase-1 limitation in `docs/architecture.md` (background-persist durability contract: at most one un-drained mutation may be lost on hard kill).

## SSE state broadcaster can reorder state events against deltas

**Severity:** Medium - under a burst of mutations, a delta event can arrive at the client before the state event for the same revision, triggering avoidable `/api/state` resync fetches.

Before the broadcaster thread, `commit_locked` published state synchronously (`state_events.send(payload)` under the state mutex), so state N always hit the SSE stream before any follow-up delta N+1. Now `publish_snapshot` enqueues the owned `StateResponse` to an mpsc channel and returns; the broadcaster thread drains and serializes on its own schedule. `publish_delta` remains synchronous. A caller that does `commit_locked(...)?` + `publish_delta(...)` can therefore race: the delta hits `delta_events` before the broadcaster drains state N. The frontend's `decideDeltaRevisionAction` requires `delta.revision === current + 1`; if state N hasn't advanced `latestStateRevisionRef` yet, the delta is treated as a gap and the client fires a full `fetchState`.

**Current behavior:**
- `publish_snapshot` is async (channel + broadcaster thread).
- `publish_delta` is sync.
- Client can observe delta N+1 before state N.
- Extra `/api/state` resync fetches fire under sustained mutation bursts.
- Correctness preserved (resync fixes the view), but behavior is chatty and pushes load onto `/api/state` — which is exactly the path we just made cheaper.

**Proposal:**
- Route deltas through the same broadcaster thread so state and delta events for the same revision stream in order. Coalescing is fine because deltas are idempotent after a state snapshot.
- Or: have `publish_snapshot` synchronously send a revision-only "marker" into `state_events` immediately and let the broadcaster thread serialize and send the full payload; the client's `latestStateRevisionRef` advances on the marker.
- Or: document the tradeoff and rely on the existing `/api/state` resync fallback; track the extra traffic.

## SSE state broadcaster queue can grow before coalescing

**Severity:** Low - bursty commits can enqueue multiple full `StateResponse` snapshots before the broadcaster gets a chance to drop superseded ones.

The broadcaster thread coalesces snapshots only after receiving from its unbounded `mpsc::channel`. During a burst of commits, the sender side can enqueue several large snapshots first, so the "newest only" behavior does not actually bound queued memory or provide backpressure.

**Current behavior:**
- `publish_snapshot` sends owned `StateResponse` values to an unbounded channel.
- The broadcaster drains and coalesces only after snapshots have already queued.
- Full-state snapshots can accumulate during bursts even though older snapshots will be superseded.

- Replace the unbounded queue with a single-slot latest mailbox or bounded channel.
- Drop or overwrite superseded snapshots before they can accumulate in memory.
- Add a burst test that publishes multiple large snapshots while the broadcaster is delayed and asserts only the latest snapshot is retained.

## Implementation Tasks

- [ ] P2: Add reconnect-specific gapped session-delta recovery coverage:
  arm reconnect fallback polling, reopen SSE, dispatch an advancing stamped `textDelta`/`textReplace` across a revision gap, and assert live text renders before snapshot repair while recovery remains pending until authoritative repair succeeds.
- [ ] P2: Add equal-revision gap repair snapshot adoption coverage:
  skip a non-session revision, optimistically apply a later session delta, then return `/api/state` at the same revision and assert the skipped global state is adopted instead of rejected as stale.
- [ ] P2: Add production SQLite persistence coverage:
  make the SQLite runtime persistence path available under `cargo test`, then cover temp-database full snapshot save/load, delta upsert, metadata-only update, hidden/deleted row removal, and startup load.
- [ ] P2: Add Windows state-path redirection coverage:
  cover SQLite main-file symlinks, sidecar symlinks, and `.termal` directory junction/symlink cases behind Windows-gated tests.
- [ ] P2: Add post-shutdown persistence ordering coverage:
  race a late background commit against `shutdown_persist_blocking()` and prove the final persisted state reflects the latest `StateInner`, not an older worker-drained delta.
- [ ] P2: Add concurrent shutdown idempotency race coverage:
  call `shutdown_persist_blocking()` concurrently from two `AppState` clones and assert `persist_worker_alive` cannot flip false until the join owner has returned.
- [ ] P2: Add graceful-shutdown open-SSE coverage:
  cover both shutdown-before-connect and shutdown-after-initial-state through `/api/events`, and assert the stream exits within a timeout so the persist drain is reached.
- [ ] P2: Add shutdown persist failure retry coverage:
  force the final shutdown persist attempt to fail once and then succeed, and assert the worker does not exit before the successful write.
- [ ] P2: Add non-send action restart live-stream delta-on-recreated-stream coverage:
  the round-13 fix proves `forceSseReconnect()` is called on cross-instance `adoptActionState` recovery, but does not dispatch live deltas through the recreated EventSource. Submit an approval/input-style action after backend restart, then dispatch assistant deltas on the new `EventSourceMock` and assert they render in the active transcript bubble.
- [ ] P2: Add live text-repair hydration rendering regression:
  drive the live-state hook or app through text-repair hydration after an unrelated newer live revision and assert the active transcript renders the repaired assistant text without scroll, focus, or another prompt.
- [ ] P2: Add AgentSessionPanel deferred-tail component regressions:
  cover switching from a non-empty deferred transcript to an empty current session, and same-id updated assistant text through the rendered component path (`useDeferredValue`, pending-prompt filtering, and the virtualized list), not only the exported helper.
- [ ] P2: Add lagged-marker EventSource reconnect-boundary regression:
  dispatch `lagged`, trigger EventSource error/reconnect, then send a lower/same-instance state on the new stream and assert the old marker cannot force-adopt it.
- [ ] P2: Add remote hydration dedupe production-path coverage:
  drive bursty same-session remote deltas through the production hydration path, assert only one remote session fetch is issued, and assert the in-flight guard is cleared after successful hydration.
- [ ] P2: Add failed manual retry reconnect-rearm regression:
  cover manual retry hitting a transient failure, then the next scheduled attempt adopting a newer same-instance snapshot while polling still continues until SSE confirms.
- [ ] P2: Add timer-driven reconnect same-instance-progress live-proof regression:
  trigger the non-manual reconnect fallback path, adopt a same-instance `/api/state` snapshot with forward progress while SSE remains unopened/unconfirmed, advance timers, and assert fallback polling continues until a data-bearing live event confirms recovery.
- [ ] P2 watchdog wake-gap stop-after-progress regression:
  trigger watchdog wake-gap recovery, adopt same-instance `/api/state` progress, and assert no additional reconnect polling occurs before a later live event.
- [ ] P2: Cover the index clamp-on-shrink branch in `MarkdownDiffView` and `RenderedDiffView`:
  re-render the parent with a smaller `regions`/`segments` array while `currentChangeIndex`/`currentRegionIndex` points past the new end and assert the counter snaps to "Change/Region 1 of N" while prev/next still wrap correctly. Today the existing prev/next tests only exercise wrap-around at full length; the `current >= changeCount/regionCount` clamp branch in the `useEffect` is unexercised.
- [ ] P2: Add rendered diff render-budget coverage:
  create many Mermaid/math rendered regions and assert the preview applies the same document-level caps as a single `MarkdownContent` document.
- [ ] P2: Add Mermaid dynamic import fallback coverage:
  force `import("mermaid")` to reject with a dynamic module fetch error and assert the bundled fallback script path renders successfully.
- [ ] P2: Add single-target rendered diff navigation coverage:
  assert prev/next scrolls the only Markdown diff change and the only rendered diff region even though the selected index does not change.
- [ ] P2: Route the new lagged-recovery reconnect test through the textDelta fast-path it documents:
  the new `App.live-state.reconnect.test.tsx` test exercises the revision-gap branch (the `messageCreated` delta omits `sessionMutationStamp` so it falls into the resync fallback). Add `sessionMutationStamp` so the delta routes through the matched-stamp fast-path that the surrounding `handleDeltaEvent` comment is most concerned about, OR rename the test to clarify it covers the revision-gap branch specifically and add a sibling test for the textDelta fast-path.
- [ ] P2: Split the bad-live-event + workspaceFilesChanged test into isolated arrange-act-assert phases:
  `ui/src/backend-connection.test.tsx:1225-1261` co-fires the stale `delta` and the `workspaceFilesChanged` event in one `act()`. The assertion `countStateFetches() === hydratedStateFetchCount` is satisfied if either side skips confirmation, so the test cannot pinpoint which side regressed. Dispatch `workspaceFilesChanged` alone first and assert no fetch fired; then add the stale delta separately and re-assert.
- [ ] P2: Add frontend stop/failure delta-before-snapshot terminal-message coverage:
  dispatch cancellation/update deltas before the same-revision snapshot and assert appended stop/failure terminal messages remain rendered without relying on a later unrelated refresh.
- [ ] P2: Add delegation EventSource recovery timing coverage:
  drive delegation repair through `EventSource.onerror`/`onopen` and assert reconnect recovery remains armed until live SSE data resumes after snapshot repair.
- [ ] P2: Add delegation persist-delta transition coverage:
  assert running, failed, and canceled delegation transitions mark delegation state as mutated and persist through the delta path, not only create/completion updates.
- [ ] P2: Add homogeneous conversation-overview segment cap coverage:
  build a long same-kind message run with `maxItemsPerSegment` set and assert the segment policy is either capped or explicitly documented as a mixed-run-only cap.
- [ ] P2: Scope marker slot lookup to the panel root in `ui/src/panels/AgentSessionPanel.tsx:921-931`:
  `findMountedConversationMessageSlot` does a global `document.querySelectorAll("[data-session-search-item-key]")` and returns the first match by `messageId`. When the same session is rendered in two workspace panes, this can scroll the wrong pane's slot when the in-page `messageSlotNodesRef` cache misses (e.g. after a remount). Scope to the panel root via the `scrollContainerRef` parent or include a pane id in the selector.
- [ ] P2: Reset `messageSlotNodesRef` on session change in `ui/src/panels/AgentSessionPanel.tsx:666-692`:
  ref keys by `messageId` only and is never cleared. If `SessionConversationPage` is reused for a different session (page is keyed by session id higher up, but if memoization changes), stale message-id keys could persist across sessions. Add `useEffect(() => messageSlotNodesRef.current.clear(), [session.id])` to harden.
- [ ] P2: Add remote marker update/delete proxy coverage:
  cover the remote-backed PATCH and DELETE marker proxy branches, including request method/path/body, nullable PATCH serialization, delete response id checks, and localized response application.
- [ ] P2: Drive marker id-mismatch coverage through the actual remote PATCH proxy:
  have the production update path or PATCH route call a fake remote that returns a different marker id, then assert the proxy rejects it and does not upsert locally.
- [ ] P2: Extend marker mutation-stamp regression coverage to update/delete failures:
  add rejected update and delete cases, such as missing marker or bad anchor, and assert both `inner.last_mutation_stamp` and the session mutation stamp remain unchanged.
- [ ] P2: Add PATCH-specific marker request rejection tests:
  cover malformed PATCH JSON and an update payload with an unknown marker kind, pinning the `update_session_marker` rejection path and strict update DTO deserialization.
- [ ] P2: Make marker hover toolbar reachable on touch-only devices (`ui/src/styles.css:3920-3935`):
  toolbar is `pointer-events: none` until hover/focus-within. Touch-only devices without a hover state cannot reveal the "Add checkpoint marker" button. Toolbar can be tab-reached after the user lands on something focusable inside the message; flag if Phase 1 wants touch parity. Add a long-press or always-visible mode for touch.
- [ ] P2: Switch delegation persistence from rewrite-all to row-level upsert + tombstones in `src/persist.rs:1104-1123`:
  the new `delegations` SQLite table uses `DELETE FROM delegations` then re-insert every row on any single-row change. Session table uses targeted upserts/deletes, but delegations rewrite the whole table per delta tick. With long delegation history this regresses to the same pattern the SQLite split set out to avoid. Add per-row insert/update + tombstone tracking matching the session pattern, OR document why wholesale rewrite is acceptable for delegation cardinality.
- [ ] P2: Add stored-version guard to SQLite schema migration in `src/persist.rs:638-665`:
  `SQLITE_SCHEMA_VERSION` was bumped to `"2"` but `ensure_sqlite_state_schema` ignores the existing value and unconditionally writes `"2"`. A downgrade to a v1 binary would silently lose dedicated `delegations` rows after the v2 binary cleared the embedded JSON copy from `app_state`. Add a stored-version check (read the `meta` row first, refuse downgrade or run a one-time legacy-to-table migration), and document the no-downgrade contract in `docs/features/sqlite-session-storage.md`.
- [ ] P2: Add `shouldApplyMarkerMutationResponse` call sites for update + delete in `ui/src/app-session-actions.ts:653-684`:
  the helper is helpfully extracted but only consumed by `handleCreateConversationMarker`. There is no `handleUpdateConversationMarker`/`handleDeleteConversationMarker` in this file (only `api.ts` exposes those). When marker update/delete UI is wired in, the gating helper exists but the new entry points must remember to use it. Add a TODO comment near the helper, or land the update/delete handlers immediately so the gate is applied uniformly.
- [ ] P2: Add a pure-delegation-update regression test in `src/tests/delegations.rs`:
  `delegation_mutation_stamp` is now a second top-level mutation watermark next to `last_mutation_stamp`. Both new round-32 tests stage a delegation create which incidentally bumps a session via the parent card. Add a targeted regression for "pure delegation update produces a delta": `state.collect_persist_delta(0)` after modifying *only* delegation fields (no session push) should emit `changed_delegations` with the right ids.
- [ ] P2: Document the empty-delegations upgrade edge case in `src/persisted_state.rs:122`:
  `has_persisted_delegations = !self.delegations.is_empty()` means a load with zero delegations does not seed `delegation_mutation_stamp`, so the legacy embedded delegations payload never gets cleared from the metadata JSON if all delegations were removed before the v2 upgrade. Practically harmless (an empty `delegations: []` is small) but the comment "rewrites the metadata row without any legacy embedded delegation payload" is conditional on `delegations` being non-empty at upgrade time. Add a Note in the comment, or unconditionally seed the watermark.
- [ ] P2: Restore `active_turn_*` fields on commit failure in `src/session_lifecycle.rs:398, src/turn_lifecycle.rs:442`:
  `finish_active_turn_file_change_tracking(record)` was relocated inside the closure, ahead of `commit_locked`. On commit failure (the explicit Err arm at `session_lifecycle.rs:438`), the in-memory record now leaves the active-turn tracking already finished/cleared while no rollback restores `active_turn_start_message_count` or `active_turn_file_changes`. Previously the call ran after a successful commit. Not catastrophic (these fields aren't persisted, and the next stop attempt re-triggers), but a small fidelity regression in the commit-failure rollback path. Roll back on commit failure or document the intentional drop with a brief comment.
