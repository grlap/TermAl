# Bugs & Known Issues

This file tracks only reproduced, current issues and open review follow-up
tasks. Resolved work, fixed-history notes, speculative refactors, cleanup notes,
and external limitations do not belong here. Review follow-up task items live in
the Implementation Tasks section.

## Active Repo Bugs

## Command-file regular-file gate is check-then-open

**Severity:** Note - `src/api_files.rs:418, 562, 597`. Command discovery and resolver metadata now reject stable symlinks and non-regular files before opening, but the check is still separate from the subsequent file open. A command file swapped between the check and open can still be followed/read.

**Current behavior:**
- Stable symlinks and non-files under `.claude/commands/` are skipped.
- There is still a small TOCTOU window between file-type validation and opening.

**Proposal:**
- Bind validation to the opened handle where platform support allows it, e.g. no-follow open plus handle metadata checks.
- Or compare pre/post file metadata and treat mismatch as unavailable.

## `dispatch_delegation_wait_resumes` errors are stderr-only without audit ledger

**Severity:** Low - `src/delegations.rs:1131-1154`. Dispatch errors are written to stderr only. A wait that was consumed but failed to dispatch leaves no structured trace in state, deltas, or a retained wait record.

Operators and the UI cannot tell that fan-in resume should have happened but did not.

**Current behavior:**
- Dispatch errors write to stderr only.
- The wait has already been removed.
- No audit ledger entry is created.

**Proposal:**
- Emit a structured warning event or retain dispatch error metadata.
- Or document the best-effort policy and recovery expectations.

## First-settled active-baseline same-message growth lacks a safe turn boundary

**Severity:** Medium - `src/telegram.rs:2583-2637`. When a Telegram prompt is armed behind an active/approval-paused turn, the relay baselines the current assistant message while `baseline_while_active=true`. If the tracked message id has already grown by the first settled poll, the relay cannot distinguish "old turn finished after the last active poll" from "the Telegram reply was appended to the same message id."

Forwarding the grown same message immediately can leak the pre-existing active turn into Telegram and consume the arm. Baseline-only behavior avoids that leak but can miss producers that append the actual Telegram reply to the same assistant message before the first settled poll.

**Current behavior:**
- First settled poll records the grown same-message length as the baseline and waits for later growth or a later message.
- Later same-message growth is forwarded because `resend_if_grown` remains armed.
- Same-message reply text already present on the first settled poll is not forwarded.

**Proposal:**
- Add a stronger turn-boundary signal from the session/agent layer, then forward only text known to belong to the Telegram-originated prompt.
- Or document that same-message append before the first settled poll is unsupported for queued Telegram prompts.

## `forward_new_assistant_message_outcome` is now ~400 lines with interleaved early-returns

**Severity:** Note - `src/telegram.rs:2512-2912`. The forwarding path now mixes active-baseline transitions, footer retry, chunk retry/skip state, and visible-content suppression. Future contributors will struggle to trace which baseline shape is preserved across the merge.

**Current behavior:**
- Single function ~400 lines.
- Multiple interleaved early-return branches.

**Proposal:**
- Extract the active-baseline transition into a helper `transition_active_baseline_to_settled` that returns either the new cursor + position or an `OutcomeShortCircuit`.

## Marker dialog-semantics test bundles 6+ behaviors in one `it()`

**Severity:** Note - `ui/src/panels/AgentSessionPanel.test.tsx:1024-1121`. The new "uses dialog semantics and local keyboard behavior" test combines six behaviors (dialog role, input focus/select, codepoint truncation, whitespace-disabled submit, button-keydown short-circuit, resize-doesn't-close-during-edit, trim, cancel restores focus, escape restores focus). One test asserting six behaviors fails opaquely.

**Current behavior:**
- 6+ assertions in one `it()`.
- Failure messages cluster at one line.

**Proposal:**
- Consider splitting once the test grows further.
- Current 6-in-1 is acceptable but the pattern should not expand.

## `from_ui_file` returns `Option<Self>` for three distinct disabled-relay reasons

**Severity:** Note - `src/telegram.rs:181-213`. The function returns `Option<Self>` for THREE distinct disabled-relay reasons (disabled flag, missing/empty token, missing/empty default project). The caller cannot tell why the relay isn't started. A typed reason would help diagnostics and let the UI surface a more accurate "Stopped" reason.

The new "Stopped" UI label is broad. If the user thinks they enabled the relay but configured an invalid project, they get the same "Stopped" copy as if they merely toggled the relay off.

**Current behavior:**
- Three disabled paths collapse to `None`.
- Caller cannot distinguish.

**Proposal:**
- Return a `Result<Self, RelayDisabledReason>` and route the reason through to status / preferences UI.

## `prune_telegram_config_for_deleted_project` reconcile path is `#[cfg(not(test))]`

**Severity:** Low - `src/telegram_settings.rs:243-264`. Round 74 wired the relay reconcile into `prune_telegram_config_for_deleted_project` (closes the round-73 deleted-project entry) but the reconcile path is `#[cfg(not(test))]`. The persistence side is tested, the reconcile side is not.

**Current behavior:**
- Reconcile call is `#[cfg(not(test))]`.
- Production restart path is structurally untested.

**Proposal:**
- Add a non-`cfg`-gated abstraction so a Rust test can verify the reconcile is invoked after a successful prune.

## Supervised in-process Telegram relay status is untestable in production due to `#[cfg(test)]` fallback

**Severity:** Medium - `src/telegram.rs:220-331`. `telegram_relay_status_snapshot()` has a production implementation backed by the live relay runtime and a test fallback that always returns `running: false` / `lifecycle: Manual`. The wire-shape tests can assert `InProcess` serialization statically, but no integration test exercises the live status endpoint while the in-process relay is running.

**Current behavior:**
- Relay status snapshot has `#[cfg(not(test))]`/`#[cfg(test)]` parallel implementations.
- Tests always see `running: false` / `lifecycle: Manual`.
- Production behavior is structurally untested.

**Proposal:**
- Add a non-`cfg(test)` "test mode" environment variable that lets a Rust integration test boot the runtime in a no-op mode and assert `running` flips.
- Or refactor the runtime so the status accessors take a `&Self` parameter that tests can inject.

## `TelegramRelayRuntime` is a file-level global rather than `AppState`-owned state

**Severity:** Note - `src/telegram.rs:220-331`. `TelegramRelayRuntime` and `TELEGRAM_RELAY_RUNTIME` are file-level globals (`LazyLock<Mutex<...>>`). `AppState` has no visibility into the relay's running state, so any future health-monitor, restart-on-error, or readiness-signaling logic ends up reading globals instead of methods on `AppState`.

**Current behavior:**
- Runtime state lives in module-level statics.
- Test injection is harder; production-vs-test parity is structural.

**Proposal:**
- Move the runtime into `AppState` and own its lifecycle on the state object.

## `reconcile_telegram_relay_from_saved_settings` is synchronous on main task at startup

**Severity:** Note - `src/main.rs:115-116`. The reconcile runs synchronously on the main task, blocking after "listening: http://" is printed but before the server starts accepting requests. With corrupt-file backup paths the reconcile could spend time on filesystem operations before the server is fully responsive.

**Current behavior:**
- Synchronous reconcile after server bind, before request handling.

**Proposal:**
- Spawn the reconcile as a `tokio::spawn` so the server responds immediately.

## Telegram relay stop/restart does not wait for old thread quiescence

**Severity:** Medium - `src/main.rs:145`, `src/telegram.rs:248-315` signal the Telegram relay to stop but do not join the old relay thread or otherwise wait until it has stopped using its captured config.

After shutdown, disable, or config retargeting, a relay that already passed its shutdown check can briefly continue polling or handling Telegram updates with the old bot/project configuration. During process shutdown this can also exit before update cursors or state-file work has quiesced.

**Current behavior:**
- Stop/restart flips a shutdown flag for the old relay.
- The old detached thread is not joined.
- Replacement or shutdown can proceed before the old relay is fully idle.

**Proposal:**
- Retain a relay `JoinHandle` and join with a bounded timeout during restart and graceful shutdown.
- Or gate update/action side effects on a runtime generation check immediately before each side effect.

## Telegram relay status can report running before initialization succeeds

**Severity:** Low - `src/telegram.rs:257-324`. `start_telegram_relay_runtime()` sets `runtime.running = true` before the spawned worker has completed Telegram bot initialization, then `telegram_relay_status_snapshot()` reports `running: runtime.running && !runtime.spawning` after the spawn call clears `spawning`.

That means `/api/telegram/status` can briefly report `running: true` while `run_telegram_bot_with_config()` is still blocked in startup work such as `getMe`, or is about to fail and clear the state.

**Current behavior:**
- Runtime state flips to `running = true` before the worker enters and completes bot initialization.
- `spawning` is cleared immediately after the OS thread is spawned, not after the relay is ready to poll.
- Status can present the relay as running before readiness is proven.

**Proposal:**
- Track a distinct `starting`/`ready` state in `TelegramRelayRuntime`.
- Or have the worker signal readiness only after initialization succeeds, then expose `running: true`.

## Telegram bot token is persisted as plaintext in `telegram-bot.json`

**Severity:** Medium - `TelegramUiConfig.bot_token` is serialized directly into `~/.termal/telegram-bot.json`.

Responses mask the token, but the full credential remains on disk and in temp/corrupt-backup write paths. Unix hardening sets `0600`; Windows is a P0 platform and currently has only a no-op permission hardening path. Backups, sync tools, or another local process can read the token from the settings file.

**Current behavior:**
- Saving Telegram settings writes the full bot token to `telegram-bot.json`.
- API responses return only a masked token.
- Windows file hardening does not apply an ACL or secret-store protection.

**Proposal:**
- Move the token to an OS secret store, or keep token configuration env-only until protected storage exists.
- If file persistence stays, add explicit Windows ACL handling and document backup/sync exposure.

## `wire_session_from_record` and `wire_session_summary_from_record` parallel paths still risk drift

**Severity:** Note - `src/state_accessors.rs:285-318`. Round 72 added comments to both helpers reminding callers to keep them in sync, but the structural risk remains: any new field added to wire `Session` must be remembered in the explicit struct literal at `wire_session_summary_from_record`. The first proposal (refactor to a single field list) was not adopted; the second (debug-assert summary equals full for shared fields) was also not adopted.

Comments are documentation-only mitigation — they don't fail when the contract drifts.

**Current behavior:**
- Round 72 added sync-reminder comments at both call sites.
- Summary form still lists fields explicitly; full form uses clone-and-modify.
- New `record.foo` fields can silently miss the summary path.

**Proposal:**
- Add a debug-assert that the summary form's output equals the full form's output for shared fields.
- Or refactor `wire_session_summary_from_record` to call `wire_session_from_record` and then strip messages/messages_loaded.
- Or introduce a separate `SessionSummary` wire struct that omits `messages`/`messages_loaded` (eliminates the duplicate field list naturally).

## Test bypasses internal mutation invariants for `wire_sessions_expose_remote_owner_metadata`

**Severity:** Note - `src/state_accessors.rs:200-242`. The new test reaches into `state.inner.lock()` and directly mutates `inner.sessions[index].remote_id`. The test bypasses any normal mutation path (`session_mut_*`), so it doesn't exercise the mutation-stamp bookkeeping that real remote-proxy ingestion goes through.

**Current behavior:**
- Test directly mutates record fields.
- No public ingestion path exercised.

**Proposal:**
- Drive the same scenario through a public ingestion path (e.g., feed a remote state snapshot via `apply_remote_state_snapshot`).

## Cross-remote `remote_id` information leak in wire responses to remotes

**Severity:** Low - `src/wire.rs:490-491`, `src/state_accessors.rs:267`. Adding `remote_id: Option<String>` to wire Session means the field is now in every API response that returns a Session, including `/api/state`, session responses, SSE `SessionCreated` payloads, and responses we serve to remotes. If we proxy a session for remote A and remote B asks us for that proxy session, the wire would emit `remote_id: "remote-a-id"`, leaking our naming for A to B.

The `remote_id` is a local config alias (e.g., "ssh-lab"), not a credential, but it's now visible across remotes. Phase 1 trust model may waive this.

**Current behavior:**
- `remote_id` exposed in broad wire Session responses.
- `localize_remote_session` clears the field on inbound (correct).
- Outbound responses to remotes still include OUR alias for OTHER remotes.

**Proposal:**
- When serving wire Sessions to remotes (vs. to local UI), strip the `remote_id` field.
- Or explicitly document that local remote aliases/session-to-remote ownership are non-sensitive shared metadata under the Phase 1 trust model.

## No test for inbound attacker-chosen `remote_id` in `localize_remote_session`

**Severity:** Note - `src/remote_sync.rs:534`. The defensive clear of inbound wire `remote_id` is correct, but no test covers the case where a remote snapshot sends `remote_id` set to an attacker-chosen value. The `apply_remote_session_to_record` path should overwrite with the trusted `remote_id` from the connection, so this is safe only if that production ingestion path is exercised.

**Current behavior:**
- Defensive clear is correct.
- Existing coverage can set record metadata directly instead of feeding an inbound remote snapshot.
- No test exercises the attacker-claim case through the production localization/ingestion path.

**Proposal:**
- Add a Rust test that simulates a remote snapshot sending `Session` with `remote_id: Some("OTHER-REMOTE")` and asserts the resulting `record.remote_id` is the trusted connection id while embedded wire metadata is cleared.

## Unmount race test relies on console error suppression for `act` warnings

**Severity:** Low - `ui/src/panels/AgentSessionPanel.test.tsx:7102-7129`. "Ignores delegation completion after the footer unmounts" exercises only the unmount-during-await path. It does NOT verify (a) `setIsDelegationSpawning(false)` is gated by `isMountedRef.current` so the React `act` warning never appears (covered indirectly by lack of console error), or (b) `focusComposerInput()` is not called after unmount (a pending rAF could try to focus a detached node).

A regression that drops the `isMountedRef.current` check inside `finally` would not be caught directly — `act` warnings only surface in CI and may be flaky.

**Current behavior:**
- Unmount-during-await path covered.
- `isMountedRef.current` guard not directly asserted.
- `focusComposerInput()` post-unmount not verified.

**Proposal:**
- Assert `console.error` was not called with "act"/"unmounted" warnings during the test.
- Or stub `setIsDelegationSpawning` via spy and verify it isn't invoked post-unmount.

## `enableLocalDelegationActions` flag flips invalidate `MessageCard` memo

**Severity:** Low - three callbacks at `ui/src/SessionPaneView.render-callbacks.tsx:355-371` are passed as `enableLocalDelegationActions ? handler : undefined`. When the flag flips between renders, three new `undefined` slots vs. three stable function refs change the `MessageCard` props and re-render the entire parallel-agents card. `MessageCard` is `memo`-wrapped — passing `undefined` toggles invalidate the memo check on every flag flip.

**Current behavior:**
- Flag flip → three undefined props → memo invalidation → full card re-render.
- Acceptable today (project remoteId rarely flips).

**Proposal:**
- Memoize the three "disabled" undefined values as a single object.
- Or pass the flag itself through and let the consumer decide.

## `delegation_parent_card_update_ignores_tool_source_id_collision` only manually constructs the collision

**Severity:** Low - the new test at `src/tests/delegations.rs:655-746` constructs the collision by manually inserting a tool-source row with the same id as the delegation. The production paths that could create such a collision (Claude task path emitting a tool-source row with a delegation-id-overlapping uuid) are not exercised.

A regression in delegation-id generation (e.g., switches from uuid to deterministic source) could create real collisions and this test wouldn't catch it.

**Current behavior:**
- Test manually inserts a same-id collision.
- Production cross-path collision not exercised.

**Proposal:**
- Add a sibling test that drives both the Claude task path and the delegation creation path with overlapping ids.
- Or document the assumption that uuid id spaces don't collide deterministically.

## Conversation overview viewport translation can reuse a stale same-size tail window or cross-session translation

**Severity:** Medium - tail-window viewport translation validates compatibility only by `messageCount` and lacks any session identity guard.

`ui/src/panels/conversation-overview-map.ts:299-333`. During streaming, the visible tail can shift from one 20-message window to another while the viewport snapshot still reports the same count, allowing an old translation offset to project the rail viewport marker onto the wrong transcript region. Additionally, a snapshot from a different session that happens to share the same `messageCount` could trigger the translation branch, projecting one session's viewport against another session's overview.

**Current behavior:**
- `viewportSnapshotTranslation` carries only `snapshotMessageCount`.
- `projectConversationOverviewViewport` accepts any later viewport snapshot with the same count.
- Same-size shifted tail windows can reuse a stale `sourceTopOffsetPx`.
- No `sessionId` equality guard; cross-session reuse is structurally possible.

**Proposal:**
- Include a cheap window identity, such as first/last message id or a layout window version, in both the translation and viewport snapshot.
- Add `sessionId` equality as part of the translation gate (and persist `sessionId` on the translation).
- Add regressions for a same-size shifted tail window and for cross-session viewport projection.

## Returning to bottom leaves stale virtualized scroll-kind classification

**Severity:** Medium - bottom re-entry clears the idle-compaction timer but leaves `lastUserScrollKindRef.current` set to `"incremental"`.

`ui/src/panels/VirtualizedConversationMessageList.tsx:2726`. The cleared idle timer is normally what expires scroll-kind state, so later native scrollbar movement without wheel/key/touch input can inherit the stale classification.

**Current behavior:**
- Native downward scroll near bottom sets `lastUserScrollKindRef.current = "incremental"`.
- The idle-compaction timer is cleared at the same boundary.
- Later native scrolls can reuse the cached scroll kind indefinitely.

**Proposal:**
- Clear `lastUserScrollKindRef` on bottom re-entry, or expire the one-tick override with a short timestamp/timer.
- Add a regression that returns to bottom, then performs a native scroll with no preceding wheel/key/touch input.

## `resolveViewportSnapshotTranslation` only happy-path tested

**Severity:** Medium - the new translation helper has six negative branches (`layoutSnapshot === null`, `layoutSnapshot.messageCount >= estimatedRows.length` (full-transcript no-op), `layoutSnapshot.messages.length === 0`, `firstRowIndex < 0` (orphan tail message id absent from full transcript), `!hasContiguousWindow`, drift case where `viewportSnapshot.messageCount !== snapshotMessageCount`). The new test at `conversation-overview-map.test.ts:452-506` covers only the happy path and reuses the layout snapshot as the viewport snapshot.

`ui/src/panels/conversation-overview-map.test.ts:452-506`. Each negative branch silently returns null and falls through to the legacy projection. The current happy-path test also does not prove that `projectConversationOverviewViewport` handles a newer live viewport snapshot independently from the layout snapshot. A regression that flipped any of these guards or stale-live-viewport handling would only surface as misaligned viewport markers in production.

**Current behavior:**
- One happy-path test exercises contiguous tail window with `messageCount < estimatedRows.length`.
- That test passes the same snapshot as both layout and viewport input.
- Negative branches silently fall through.
- Drift case (where viewport snapshot count differs from translation snapshot count) is uncovered.

**Proposal:**
- Add focused tests for each negative branch via `buildConversationOverviewProjection` then probing `projection.viewportSnapshotTranslation` for null in each case.
- Add a separate live viewport snapshot case with a different `viewportTopPx`.
- Add a drift-case test where viewport snapshot count differs from translation snapshot count and the legacy projection path is exercised.

## `resolvePrependedMessageCount` only happy path tested

**Severity:** Medium - the new pure helper has six branches (cross-session, empty previous window, no growth, partial overlap, no first-message match, contiguous match at index 0). Only the happy path (genuine prepend at startIndex>0) is exercised end-to-end via the prepend integration test.

`ui/src/panels/VirtualizedConversationMessageList.tsx:413-441`. The integration test combines layout, scroll, and DOM behavior, so a regression in the matcher (off-by-one in `maxStartIndex`, accepting a non-contiguous overlap) might be masked by the looser scroll-position assertion.

**Current behavior:**
- Helper is not exported; no direct unit test.
- Edge cases (empty before/after, single message, all messages new vs. all stale, partial overlap, session change) unverified.

**Proposal:**
- Export `resolvePrependedMessageCount` (or move it to a sibling module) and add unit tests for each branch with `MessageWindowSnapshot` fixtures.

## `mountedRangeWillChange` early-return not pinned by test

**Severity:** Medium - the new `mountedRangeWillChange || !preservedAnchorSlot` early-return at `VirtualizedConversationMessageList.tsx:1561-1567` is the load-bearing fix for the "single-frame visual jump" issue (which was removed from bugs.md by this round). The integration test was simultaneously rewritten to drop `await waitFor(...)` in favor of a synchronous assertion. There is no test that pins the new condition — i.e., that when a prepend forces a range change AND the anchor is mounted, no stale-rect scroll write is emitted before the followup effect re-anchors.

`ui/src/panels/VirtualizedConversationMessageList.test.tsx:484-492`. The test asserts the post-flush position, not the absence of the intermediate stale write. A regression that flipped back to the old `!preservedAnchorSlot` gate would still pass the new test (the followup effect catches up either way).

**Current behavior:**
- New test asserts post-flush scroll position synchronously.
- No assertion verifies the absence of an intermediate stale-rect scroll write.
- A regression to the prior gate would not be caught.

**Proposal:**
- Assert `harness.scrollWrites` between the prepend and the followup effect — specifically, that no scroll write lands at the stale `targetScrollTop` value computed from pre-mutation rects.
- Or use `hydrationScrollWrites.every(...)` to assert all writes track the final scroll position.

## rAF-coalesced `messageCount` refresh now lags `layoutSnapshot.messageCount` behind `messageCount` by one frame

**Severity:** Medium - the round-62 fix routes the `messageCount`-driven `refreshLayoutSnapshot` through the same rAF-coalesced scheduler as the steady-state effect. Coalescing is correct, but downstream consumers reading `layoutSnapshot.messageCount` synchronously inside the same React render now see a stale snapshot until the rAF flushes.

`ui/src/panels/conversation-overview-controller.ts:241-274`. The new "coalesces ready layout refreshes" test pins this behavior — `expect(layout-message-count).toHaveTextContent("90")` immediately after rerender to 120/140, then 140 only after `flushNextFrame`. The active-streaming case (per-chunk delta increments) is the most sensitive — every assistant chunk leaves snapshot consumers one rAF behind.

**Current behavior:**
- All `refreshLayoutSnapshot` calls go through rAF scheduling.
- Synchronous consumers see stale snapshots within the same React frame.
- The new test codifies the lag.

**Proposal:**
- Confirm whether downstream consumers (rail tail items, viewport projection) rely on synchronous freshness.
- If yes: add a fast-track path — when `messageCount - layoutSnapshot.messageCount > N`, refresh synchronously to avoid >1 rAF lag.
- Otherwise document the lag explicitly so future consumers know the snapshot lags by up to one rAF behind `messageCount`.

## `markUserScroll` anchor speculation captures approximate touch offsets

**Severity:** Medium - speculative offset adjustment `viewportOffsetPx - inputScrollDeltaY` applied unconditionally on every input event. For touch events, `touchDeltaY` is the FINGER delta (not the scroll delta). When user touches a non-scrollable region, swipes within an iframe, or hits a scroll boundary, the anchor's `viewportOffsetPx` ends up off by the would-be delta.

`ui/src/panels/VirtualizedConversationMessageList.tsx:2767-2778`. The downstream prepended-restore effect uses this anchor as a scroll target.

**Current behavior:**
- Speculative offset applied to anchor on every input event with non-null delta.
- Touch deltas approximate scroll deltas.
- At scroll boundaries the speculation is wrong.

**Proposal:**
- Defer the speculative offset until the native scroll handler observes an actual `scrollTop` change.
- OR drop the speculation and re-capture the anchor inside the prepended-restore effect.

## `isPurePrepend` strict gate drops bottom-gap preservation when concurrent append happens

**Severity:** Medium - in streaming sessions hitting hydration, the user-near-bottom-escape-upward scenario is exactly when a new assistant chunk lands alongside the prepend — making `isPurePrepend` false. The bottom-gap signal is silently consumed.

`ui/src/panels/VirtualizedConversationMessageList.tsx:1473-1479`. With any trailing growth, the bottom-gap path is bypassed and `pendingBottomGapAfterPrepend` is cleared without being applied.

**Current behavior:**
- Strict `isPurePrepend` gate.
- Concurrent append makes the gate false.
- Bottom-gap preservation silently consumed.

**Proposal:**
- Relax to `pureOrAppendingPrepend` allowing N appended messages alongside the prepend.
- OR re-store the bottom gap if the gate fails so the next layout effect can still consume it.

## `skipNextMountedPrependRestoreRef` cleared by new prepend effect — silently overrides user-scroll intent

**Severity:** Medium - the new prepend-anchor `useLayoutEffect` unconditionally writes `skipNextMountedPrependRestoreRef.current = false` whenever a prepend is detected. If user wheels (sets it true), then a transcript prepend fires before the prior effect drains, the skip flag is silently cleared.

`ui/src/panels/VirtualizedConversationMessageList.tsx:1520-1521`.

**Current behavior:**
- `markUserScroll` sets `skipNextMountedPrependRestoreRef = true`.
- New prepend effect unconditionally clears it.
- User-scroll intent lost on prepend.

**Proposal:**
- Respect the skip flag if set; only clear when no prior intent exists.

## `pendingPrependedMessageAnchorRef.remainingAttempts = 3` magic number with no telemetry on exhaustion

**Severity:** Medium - if the anchor never re-mounts (e.g., user scrolls away during chained re-renders), `remainingAttempts` decrements to 0 and gives up — leaving `latestVisibleMessageAnchorRef` stale. No log when this exhausts.

`ui/src/panels/VirtualizedConversationMessageList.tsx:1523-1529`. 3 is arbitrary with no test pinning the boundary.

**Current behavior:**
- Three retry attempts.
- Silent exhaustion if all fail.
- No telemetry signal.

**Proposal:**
- Log when exhaustion occurs, OR
- Make the anchor invalidate on user-scroll inside the followup effect.

## `latestVisibleMessageAnchorRef` capture re-runs on every native scroll tick

**Severity:** Medium - useLayoutEffect deps include `viewportScrollTop` (state). On every native scroll tick the viewport state updates → effect re-runs → `getBoundingClientRect()` over all mounted slots. For a 600+ message tail with mounted range covering 50+ slots, this is per-scroll-tick rect reads.

`ui/src/panels/VirtualizedConversationMessageList.tsx:1645-1651`.

**Current behavior:**
- Anchor capture re-runs on every viewport scroll-state update.
- Each run does N `getBoundingClientRect()` reads.

**Proposal:**
- Throttle via rAF.
- OR only capture when prepend is imminent.

## `status-fetch-failed` priority drops mixed-instance signal silently

**Severity:** Medium - round 60 documents the priority rule, but `applyCurrentInstanceStatusBatchResponses` filters out responses whose `serverInstanceId` differs from the previous baseline. So if a status batch sees one fetch fail AND collected responses come from a NEW instance, the mixed-instance information is silently dropped: no `recoveryGroups` for the new-instance responses.

`ui/src/delegation-commands.ts:594-617` + `docs/features/agent-delegation-sessions.md:245-260`.

**Current behavior:**
- `status-fetch-failed` masks concurrent server restart.
- Wrappers receive `status-fetch-failed` packet without instance-change diagnostic.
- Doc claims status-fetch priority but doesn't note the partial-information loss.

**Proposal:**
- Document the partial-information loss in `agent-delegation-sessions.md` so wrappers know `status-fetch-failed` may hide a concurrent server restart.

## Delegation result formatting remains coupled to command transport

**Severity:** Low - the hook at `ui/src/SessionPaneView.render-callbacks.tsx:13-20` imports `delegation-commands` and `delegation-result-prompt` directly, and the pure formatter at `ui/src/delegation-result-prompt.ts:11` imports `DelegationResultPacket` from `delegation-commands`.

The formatter now uses the stricter packet shape, which fixed the prior type-drift issue, but the dependency still points from pure prompt formatting into command transport. A future refactor to swap delegation transports requires re-wiring both the hook and the formatter.

**Current behavior:**
- Hook directly imports network-API module.
- Formatter imports a transport-owned packet type.
- Tests must mock the imports at the module level.
- Future transport swap requires hook rewrite.

**Proposal:**
- Pass `delegationActions: { open, insert, cancel }` as hook props (defaulting to the production wrappers in `SessionPaneView.tsx`).
- Move `DelegationResultPacket` to a neutral shared module such as `types.ts` or `delegation-result-types.ts`.
- Or expose a `DelegationActionContext` provider so consumers can override.

## `useInitialActiveTranscriptMessages` mutates `hydrationRef` during render

**Severity:** Medium - render-time side effect on a ref. Works but fragile under concurrent mode (`useTransition`/`Suspense`) — a render that's discarded would still leave the ref in its mutated state, prematurely flipping `hydrated = true` on a discarded render path.

`ui/src/panels/AgentSessionPanel.tsx:228-236`. Lines 221-226 reset on session change; lines 234-236 set `hydrated = true` in early-eligibility branch.

**Current behavior:**
- Two ref mutations during render body.
- React 18 concurrent rendering or Suspense can discard renders.
- Discarded render's ref mutations persist.

**Proposal:**
- Hoist the session-id-change reset into a `useEffect` (with the trade-off of one stale render after the change).
- Or document the render-mutation as deliberate and known-fragile under Suspense.

## "in-flight Telegram test unmounts" test asserts only `consoleError`, doesn't actually pin the unmount guard

**Severity:** Medium - React 18+ removed the "Can't perform a state update on an unmounted component" warning entirely, so removing the `isMountedRef` checks would not cause the test to fail.

`ui/src/preferences-panels.telegram.test.tsx:162-189`. The test reads as effective coverage for the `isMountedRef` guard but actually catches no regression because no warning fires under React 18. A regression making `setError` always swallow would also pass.

**Current behavior:**
- Test asserts `expect(consoleError).not.toHaveBeenCalled()`.
- Under React 18, no warning fires regardless of the guard.
- Removing the guard would not cause the test to fail.

**Proposal:**
- Spy on the test promise's then-handler (or wrap `setError`/`setIsTesting` via mock) to assert they aren't invoked post-unmount.
- OR remount and verify state is freshly initialised.
- Add a positive control: same flow stays mounted, error DOES surface.

## Two cancellation patterns coexist in `TelegramPreferencesPanel`: `cancelled` flag for initial-fetch, `isMountedRef` for handlers

**Severity:** Low - same component has two patterns for the same concern ("drop late updates after unmount"). Future maintainers may copy the wrong one.

`ui/src/preferences-panels.tsx:1229-1263`. The fetch-status `useEffect` uses its own `cancelled` closure flag while the three async handlers use `isMountedRef`.

**Current behavior:**
- Initial-fetch effect uses `cancelled` flag.
- `handleSave`/`handleTestConnection`/`handleRemoveBotToken` use `isMountedRef`.
- Two patterns side-by-side.

**Proposal:**
- Consolidate on one pattern. `isMountedRef` reads cleaner for fire-and-forget click handlers; `cancelled` flags read cleaner for effect-scoped fetches; both are fine, but pick one per file.

## `prepare_assistant_forwarding_for_telegram_prompt` race window between cursor capture and POST send

**Severity:** Medium - the new prepare/apply split correctly avoids mutate-before-success, but widens the cursor-capture-to-apply window across a network round-trip. If the agent emits new assistant text between T0 (capture) and T1 (POST returns), the T0 baseline marks the freshly-emitted message as already-forwarded.

`src/telegram.rs:890-894`. The pre-round-55 `arm_assistant_forwarding_for_telegram_prompt` had the same fundamental race but a much narrower window (no network call between cursor read and state write).

**Current behavior:**
- T0: `prepare_*` reads cursor.
- T1: `send_session_message` POST returns.
- T2: `apply_*` commits T0 cursor to state.
- An assistant message emitted between T0 and T1 is silently marked as "already forwarded".

**Proposal:**
- Re-fetch the cursor right before applying, not at the prepare step.
- Or capture `latest` AFTER the POST returns (since the goal is "baseline as of after this prompt is sent").

## `src/telegram.rs` past 1500-line architecture rubric threshold

**Severity:** Medium - file now exceeds 1766 lines after round 56. CLAUDE.md asks for smaller modules.

`src/telegram.rs`. Round 56 added `backup_corrupt_telegram_bot_file`, `telegram_command_mentions_other_bot`, and digest-failure branches on top of the round-55 baseline. Mixes: HTTP client, TermAl client, wire types, command parser, digest renderer, assistant-forwarding cursor logic, corrupt-file backup helper, and the relay loop. `telegram_settings.rs` already extracted the UI surface; the next natural cut is `telegram_relay.rs` + `telegram_clients.rs` + `telegram_wire.rs`.

**Current behavior:**
- One file owns seven concerns now.
- Continued growth pattern across recent rounds.

**Proposal:**
- Split into 2-3 modules mirroring the api.rs/wire.rs split shape.
- Defer to a dedicated pure-code-move commit per CLAUDE.md.

## `ui/src/preferences-panels.tsx` past 2000-line natural split point

**Severity:** Medium - `TelegramPreferencesPanel` now has three async handlers (handleSave, handleTestConnection, handleRemoveBotToken) each with `if (!isMountedRef.current)` guards repeated 3×. Pattern duplication is wide enough that a `useUnmountSafeAsync` helper would cut ~60 lines.

`ui/src/preferences-panels.tsx:1226-1430`. CLAUDE.md asks for smaller modules; the panel duplicates 30-40 line handler bodies with the same guard pattern.

**Current behavior:**
- Three async handlers each with three `if (!isMountedRef.current)` checkpoints (start of catch, end of try, finally).
- Pattern duplication; future maintainer copies the shape into a fourth handler.

**Proposal:**
- Extract `useUnmountSafeAsync` hook returning a `runSafe(asyncFn, { onSuccess, onError, onFinally })` wrapper, OR
- Split `TelegramPreferencesPanel` into form-state component + inner mount-safe handler module.

## `validate_and_normalize_telegram_config` mixes pure validation, mutation, and mutex acquisition

**Severity:** Medium - holds the state mutex while iterating + mutating caller-owned config. Same anti-pattern that the new prepare/apply assistant-forwarding split fixed elsewhere this round.

`src/telegram_settings.rs:148-227`. The rename from `validate_telegram_config` documents the dual responsibility but the `&mut TelegramUiConfig` signature still buries it.

**Current behavior:**
- One function holds state mutex, iterates `inner.projects`/`inner.sessions`, mutates caller-owned config.
- State held across multiple checks, allocations, and conditional mutations.

**Proposal:**
- Split into `pure_validate_telegram_config(...) -> Result<TelegramConfigNormalization, ApiError>` + `apply_telegram_config_normalization(...)` outside the lock.

## Wire projection layer owns `messages_loaded` SEMANTIC field for partial case

**Severity:** Medium - `wire_session_tail_from_record` decides `messages_loaded` based on whether the slice covers the whole transcript AND the source is loaded. This is wire-semantics decision (UI uses `messagesLoaded: false` to mean "still adopt me, but don't trust messages.length === messageCount") that lives in the projection helper.

`src/state_accessors.rs:210-219`. The wire layer's job is "single source of truth for the JSON shape" — but `messages_loaded` here is becoming a SEMANTIC field, not a shape field. `get_session_tail` is the only caller, but the next time someone needs partial transcripts (e.g., a "show me messages around message-X" range fetch), they'll either reuse this helper with a new caller (coupling unrelated wire-projections) or duplicate the logic.

**Current behavior:**
- `wire_session_tail_from_record` encodes "tail only counts as fully loaded if it covers the whole transcript AND the source is loaded".
- This is semantic flag manipulation, not pure shape projection.
- Future range-fetch callers must reuse or duplicate.

**Proposal:**
- Either move `messages_loaded` decision into the route handler (keeping the projection pure-shape), OR formalize a `partial_transcript_loaded` distinction in the wire shape itself (`transcriptLoaded: "full" | "partial-tail" | "summary"`) and have the frontend act on the typed value rather than inferring from `messagesLoaded === false && messages.length > 0`.

## `SessionHydrationRequestContext` is a four-flag bag with non-obvious mutual exclusions

**Severity:** Medium - two booleans (`allowDivergentTextRepairAfterNewerRevision`, `allowPartialTranscript`) plus three metadata fields. The flags have non-obvious interactions encoded in call-site logic, not the type.

`ui/src/session-hydration-adoption.ts:16-23`. `allowDivergentTextRepairAfterNewerRevision === true` means the request is for a divergence repair, which `shouldStartTailFirstHydration` deliberately excludes from tail-first. That exclusion lives at `app-live-state.ts:771-773`, not in the type. A reader of `SessionHydrationRequestContext` sees two unrelated flags and has to chase to the call sites to learn they're never simultaneously true.

**Current behavior:**
- Four-flag context bag.
- Mutual exclusions encoded as call-site early-returns.
- Type system doesn't enforce the contract.

**Proposal:**
- Convert to a discriminated union — `type SessionHydrationRequestContext = ({ kind: "fullSession" } | { kind: "partialTail" } | { kind: "textRepair" }) & SharedMetadata`. Classifier dispatches on `kind`; call sites can never set inconsistent flags.

## `hydratedSessionIdsRef.current.add(sessionId)` invariant has three call sites

**Severity:** Medium - after this change, `add` is called at three places (tail "adopted", early-return after partial-then-already-hydrated, full "adopted"). The invariant — "add when fully hydrated and we won't run another hydration" — is encoded by repetition.

`ui/src/app-live-state.ts:1308, 1335, 1359-1361`. Worse, the `partial` outcome at line 1310 deliberately does NOT add to the set, because the session is not fully hydrated yet. A future reader scanning for "where do we mark hydrated" sees three places and must read each branch to understand the implicit "and partial is not hydrated" rule. If a fourth state is added (e.g., "tail returned the whole transcript because backend has fewer messages than the limit AND messages_loaded was true"), the question "do we add to hydratedSessionIdsRef here?" has no automatic answer.

**Current behavior:**
- Three call sites for the "fully hydrated" mark.
- One outcome (partial) deliberately omits the mark.
- The invariant is encoded by repetition.

**Proposal:**
- Either (a) extract a small `markFullyHydrated(sessionId)` helper that wraps the add + clearHydrationRetry pair (already paired at all three sites), OR (b) compute "is this session fully hydrated" from session state at use sites and stop tracking it in a separate ref.

## Tail-then-full sequence doubles HTTP request volume for sessions ≥101 messages

**Severity:** Low - the frontend always pairs `fetchSessionTail(SESSION_TAIL_WINDOW_MESSAGE_COUNT)` with `fetchSession(...)` for sessions where `messageCount >= 101`. Phase 1 local-only is fast. Future remote-host or flaky-network scenarios pay this tax.

`ui/src/app-live-state.ts:1278-1390`. Over SSH this matters more than over HTTP loopback. Combined with the High-severity "remote-proxy hydration skipped" entry, the worst case is: tail-first (returns empty for unhydrated remote proxy) + full-fetch (triggers remote hydration, returns full transcript) — two round-trips for what could have been one.

**Current behavior:**
- Two HTTP calls per visible-session hydration for sessions ≥101 messages.
- Phase 1 local-only is fast.
- Combined with remote-proxy issue, worst case is 2× wasted traffic.

**Proposal:**
- Once remote routing is sorted, consider returning the full transcript in the same response for sessions under a "small-enough" threshold.
- Or have the client skip the tail-first request when the remote round-trip cost would dominate.

## Telegram settings updates live outside the app state/revision model

**Severity:** Medium - Telegram settings are user-visible configuration, but saves bypass `StateInner`, `commit_locked()`, snapshots, revisions, and SSE.

`src/telegram_settings.rs:30` updates `~/.termal/telegram-bot.json` directly through the Telegram settings endpoint. That means one browser tab can save config while other tabs keep stale settings until they manually refetch, and future relay lifecycle work will need to reconcile app state with a separate settings file.

**Current behavior:**
- Telegram settings updates do not bump the app revision.
- `/api/state` and SSE do not carry the changed config.
- Other open clients cannot observe settings changes through the normal state model.

**Proposal:**
- Store Telegram UI config in durable app state and mutate it through `commit_locked()`.
- If `telegram-bot.json` remains necessary for adapter interop, mirror committed state to that file behind a documented boundary.

## Telegram settings and relay state can overwrite each other in `telegram-bot.json`

**Severity:** Medium - the UI settings endpoint and Telegram relay both read-modify-write the same JSON file, and the settings mutex only protects one process.

`src/telegram_settings.rs:20` defines a process-local mutex, while `src/telegram.rs` can still run in the standalone `cargo run -- telegram` process and write the same file. Concurrent `/api/telegram/config` saves and relay cursor persistence can lose either UI-owned token/config fields or runtime-owned `chatId` / `nextUpdateId` fields. Atomic file replacement prevents partial files, but it does not serialize read-modify-write cycles across processes.

**Current behavior:**
- Settings saves and relay state persistence share one file.
- Writes are read-modify-write operations without cross-process serialization.
- The process-local mutex does not coordinate server and standalone relay modes.
- Last writer wins if the two processes read old state and then save different halves.

**Proposal:**
- Split UI config and runtime cursor/chat state into separate files, or guard all writers with an OS-level file lock.
- Add cross-process interleaving coverage proving config and runtime state both survive competing writes.

## Telegram settings UI belongs behind a focused module boundary

**Severity:** Low - the Telegram panel adds a large independent API workflow to the already broad preferences panel module.

`ui/src/preferences-panels.tsx:1214` adds several hundred lines of Telegram settings state, effects, API calls, payload shaping, and rendering to a file that already owns multiple preferences panels.

**Current behavior:**
- Telegram settings lifecycle/config UI lives inside `preferences-panels.tsx`.
- Fetch/save/test behavior and render structure are coupled to the broad preferences module.

**Proposal:**
- Extract Telegram settings UI and its fetch/save/test hook into a dedicated preferences or telegram-settings module.

## `useInitialActiveTranscriptMessages` mutates a ref during render

**Severity:** Medium - the new long-session tail-window hook writes `hydrationRef.current.sessionId` and `hydrationRef.current.hydrated = true` during render, breaking React 18 Strict Mode / concurrent rendering invariants.

`ui/src/panels/AgentSessionPanel.tsx:236-285`. The hook is part of the long-session tail-window path that activates only on transcripts above ~512 messages. Concurrent renders can flip `hydrated: true` before the actual commit, causing the windowing optimization to be skipped on first paint of large sessions. Worse, the second render re-keys the ref, potentially losing the "I started hydrating" intent. Most hooks in `panels/` use `useState` for derived-from-prop state with explicit reset effects.

**Current behavior:**
- `if (hydrationRef.current.sessionId !== sessionId) { hydrationRef.current = { hydrated: false, sessionId }; }` mutates during render (line 242-247).
- `if (!isTailEligible && messages.length > INITIAL_ACTIVE_TRANSCRIPT_TAIL_MIN_MESSAGES) { hydrationRef.current.hydrated = true; }` mutates during render (line 255-257).
- Strict Mode double-invoke fires the mutation twice without committing.

**Proposal:**
- Convert to `useState` with `useEffect` reset.
- Or use the React-docs "derived state" pattern: `const [prevSessionId, setPrev] = useState(sessionId); if (prevSessionId !== sessionId) { setPrev(sessionId); setHydrated(false); }`.
- Add Strict Mode coverage proving the windowing path still activates after a double-render.

## Active-transcript tail-window hook overlaps with `VirtualizedConversationMessageList`'s bottom-mount path

**Severity:** Medium - two layers (panel + virtualizer) gate "skip work for the tail" with different thresholds and different effects on dependent UI.

`ui/src/panels/AgentSessionPanel.tsx:175-286 useInitialActiveTranscriptMessages` windows messages to the last 96 before passing them to `ConversationMessageList` → `VirtualizedConversationMessageList`. The virtualizer's `preferInitialEstimatedBottomViewport` (round 53 addition) mounts the bottom range without rendering all messages above. The hook drops messages from React's perspective entirely (so `messageCount` becomes 0 → overview rail hides via `messageCount: isInitialTranscriptWindowActive ? 0 : visibleMessages.length` at line 804), while the virtualizer would just not mount unused slabs. A future reader changing the threshold has two places to keep in sync.

**Current behavior:**
- Hook drops messages above a 512-message session threshold, returning a 96-message tail.
- Virtualizer mounts only the bottom-of-viewport range via `preferInitialEstimatedBottomViewport`.
- Overview-rail gating uses `messageCount: 0` when the hook is windowing, hiding the rail.

**Proposal:**
- Move all "long session initial mount" logic into the virtualizer alone, then drop the hook.
- Or document the layer split with a header comment naming which problem each layer owns and why two layers exist.

## Telegram settings HTTP API split across three routes diverges from `/api/settings` convention

**Severity:** Medium - every other settings surface uses `POST /api/settings` returning `StateResponse` with SSE broadcast; Telegram uses `GET /api/telegram/status` + `POST /api/telegram/config` + `POST /api/telegram/test` returning `TelegramStatusResponse` with no broadcast.

`src/main.rs:233-235`. The `/test` route reasonably stays separate (genuinely a side-effecting outbound call). But splitting the GET/POST status+config into its own route is a divergence from the established pattern. The split also means none of the rest of the codebase's settings infrastructure (revision bumping, SSE broadcast, partial-payload merging via `UpdateAppSettingsRequest`) applies. A future caller scripting via the API has two patterns to learn.

**Current behavior:**
- Existing settings flow through `POST /api/settings` returning `StateResponse` (broadcast via SSE).
- Telegram settings use three new routes returning custom `TelegramStatusResponse` (not broadcast).
- The divergence is unexplained in code or docs.

**Proposal:**
- Fold the Telegram config bag into `UpdateAppSettingsRequest` with a `telegram: Option<UpdateTelegramConfigRequest>` field, returning `StateResponse` like every other setting.
- Or document explicitly in `docs/features/` why Telegram is intentionally separated (e.g., "secret tokens kept out of the broadcast snapshot").

## `validate_telegram_config` does TOCTOU between in-memory validation and on-disk persistence

**Severity:** Low - the validation reads `inner.projects` and `inner.sessions` while holding the state mutex, releases the lock, then `persist_telegram_bot_file(&file)?` writes. Between release and write, another thread could delete the validated project, leaving a persisted config that references a now-missing project.

`src/telegram_settings.rs:138-208`. The lock is correctly NOT held across I/O — that's the right call — but the TOCTOU window means the next status fetch will silently strip the dropped project ID via `sanitize_telegram_config_for_current_state`, which can be surprising to the user who just clicked Save. The read-time sanitize covers the symptom but not the underlying inconsistency.

**Current behavior:**
- Validation acquires the mutex briefly, then drops it.
- Persistence runs without holding the mutex.
- A concurrent project deletion between validation and persistence persists a stale reference.

**Proposal:**
- Add a header comment explaining the TOCTOU model and the sanitize-on-read recovery path.
- Or run `sanitize_telegram_config_for_current_state` after `validate_telegram_config` so the persisted file matches what the next read would return.

## `TelegramPreferencesPanel` does not memoize handlers, diverging from sibling preference panels

**Severity:** Low - `projectOptions` and `sessionOptions` are memoed, but `updateDraft`, `toggleProject`, `handleSave`, `handleTestConnection`, and the inline `onChange` lambdas at lines 1797, 1822, 1834 are recreated on every render. The two `ThemedCombobox` controls receive new function identity on every keystroke. Pattern divergence with `RemotePreferencesPanel` and other sibling panels in the same file.

`ui/src/preferences-panels.tsx:1214-1971`. A future reader copy-pasting from one panel to another now has two patterns to choose from.

**Current behavior:**
- Handlers are recreated on every render.
- Sibling preference panels in the same file memoize handlers.
- ThemedCombobox children receive new identity on every keystroke.

**Proposal:**
- Stabilize handlers via `useCallback`.
- Or document explicitly that the panel intentionally avoids memoization. Either is fine; consistency is the architectural goal.

## `src/telegram_settings.rs` module header doesn't enumerate critical invariants

**Severity:** Low - the header explains the file format transition but does not document the two-writer race, validation TOCTOU, divergent lock-error handling, or sanitize-on-read recovery model.

`src/telegram_settings.rs:1-9`. The header describes "the relay loop still reads the legacy flat runtime fields … the file format below keeps those fields flat and adds a `config` object", but does not document: (a) the two-writer race with the standalone CLI relay, (b) the validation TOCTOU window, (c) why the lock-error handling diverges from project convention, or (d) the sanitize-on-read recovery model. This is the entry point for the next reader who needs to extend the module (e.g., the Phase 1 in-process relay lifecycle).

**Current behavior:**
- Header enumerates the file-format transition but no invariants.
- Future readers risk regressing the implicit contracts.

**Proposal:**
- Extend the header to enumerate (a) what owns what in the file, (b) coordination assumptions between writers, (c) lock-failure / IO-failure recovery model.

## `persist_telegram_bot_state` reads-then-writes the file unconditionally on every state change

**Severity:** Low - the relay polls every `TELEGRAM_DEFAULT_POLL_TIMEOUT_SECS` (5s default) and writes whenever `dirty`. The new logic adds a `fs::read` + `serde_json::from_slice` round-trip on every persist, doubling syscalls.

`src/telegram.rs:190-205`. Modest cost on its own. More concerning: if the file is concurrently being rewritten by the HTTP route, `fs::read` could observe a partial write (since `fs::write` truncates and rewrites without atomicity), and the relay would silently `unwrap_or_default()` — meaning a corrupt-read is treated as "first ever persist" and the next write erases the `config` portion. Pairs with the existing "Telegram settings and relay state can overwrite each other" entry.

**Current behavior:**
- Each persist does `fs::read` + parse + merge + `fs::write`.
- A partial-read mid-concurrent-write silently degrades to defaults.
- The next write erases legitimate config.

**Proposal:**
- Combine with the atomic-write fix on the existing two-writer-race entry.
- Distinguish "file does not exist" (legitimate first-run) from "file exists but unparseable mid-write" (warn + retry).

## `SessionPaneView` `paneScrollPositions` in deps adds no reactivity

**Severity:** Low - the dependency on the dictionary identity is stable across renders for the same `pane.id`; mutations inside the dictionary do not trigger the effect. False reactivity impression for future readers.

`ui/src/SessionPaneView.tsx:1869-1900`. Either drop the dep with an `eslint-disable` comment explaining why, or capture the dependency narrowly (e.g., `paneScrollPositions[scrollStateKey]?.shouldStick`).

**Current behavior:**
- `paneScrollPositions` dict identity is stable across renders.
- Mutations inside the dict don't trigger the effect.
- The dep gives a false impression of reactivity.

**Proposal:**
- Drop the dep with an `eslint-disable` comment, or narrow to the specific value being read.

## `ConversationOverviewRail` per-segment fresh handlers and aria-label per render

**Severity:** Low - up to 160 segment buttons each get fresh `onClick`/`onKeyDown` arrow functions per render, plus a fresh `aria-label` string from `overviewSegmentLabel(segment, projection.items)` (an O(n) lookup against `projection.items.length`).

`ui/src/panels/ConversationOverviewRail.tsx:267-289`. Acceptable today, but as transcripts grow this is the next hot spot if rail rebuilds churn.

**Current behavior:**
- Each render creates 160 arrow functions and 160 aria-label strings.
- aria-label computation is O(n) against `projection.items`.

**Proposal:**
- Memoize per-segment handlers via a single delegated handler that reads the segment index from `data-conversation-overview-index`.
- Cache aria-labels alongside the segments.

## `ThemedCombobox` `useEffect` deps include `activeIndex`, tearing down listeners per keystroke

**Severity:** Low - the outside-pointer/keyboard handler effect re-attaches the global `pointerdown`/`keydown` listeners every time `activeIndex` changes (every ArrowUp/ArrowDown).

`ui/src/preferences-panels.tsx:1782-1859`. Functionally correct, but wasteful. If the same keystroke that triggered the change also fires a synthetic `keydown`, ordering between "old listener cleanup" and "new listener registration" is invisible to React.

**Current behavior:**
- Effect deps `[activeIndex, isOpen, onChange, options]` rebuild listeners per keystroke.
- Each open menu sees attach/detach churn.

**Proposal:**
- Move `activeIndex` into a ref synchronized with the state update; drop it from deps.
- Or split the effect into "attach listeners once when open" + "read activeIndex from a ref".

## `AgentSessionPanel.tsx` exceeds 2000-line architecture rubric threshold

**Severity:** Note - `ui/src/panels/AgentSessionPanel.tsx:1475` and `ui/src/panels/AgentSessionPanel.test.tsx`. The panel remains over the documented TSX file-size budget, and the composer resize/transition behavior is now a local state machine inside `SessionComposer`.

This review adds and exercises multiple rAF/transition refs plus cancellation/restore ordering in the same component. The behavior is UI-local, but the ordering contract is subtle enough that future changes are hard to reason about inside the broader panel file.

**Current behavior:**
- `AgentSessionPanel.tsx` is 2605 lines.
- `AgentSessionPanel.test.tsx` is 8677 lines.
- Composer auto-resize and transition restoration share state across several refs and rAF callbacks.

**Proposal:**
- When touching this area again, extract textarea sizing/transition behavior into a focused hook such as `useComposerAutoResize`.
- Keep targeted tests for resize scheduling, transition restoration, and session-switch cleanup with that hook.

## `scheduleConversationOverviewRailBuild` module-level FIFO queue is shared across all controller instances

**Severity:** Note - a slow rail build in pane A delays pane B by one frame; cleanup story across module reloads / HMR is subtle.

`ui/src/panels/conversation-overview-controller.ts:33-87`. The module-level `pendingConversationOverviewRailBuildTasks: ConversationOverviewRailBuildTask[]`, `conversationOverviewRailBuildFrameId: number | null`, and `nextConversationOverviewRailBuildTaskId: number = 1` are shared across all controllers/sessions/panes. Acceptable per the rAF cadence (60Hz = 16ms/frame). The cleanup logic (cancel-on-empty-queue, splice-by-task-id) is correct but the global-state coupling means HMR / module reloads have subtle behavior.

**Current behavior:**
- All rail builds across all panes serialized through one global FIFO queue.
- One slow task delays all subsequent panes by one frame.
- Module-level globals make cleanup-across-HMR subtle.

**Proposal:**
- Defer (no concrete bug today). Consider in a future round whether per-pane queues would simplify reasoning, especially as multi-pane scenarios become more common.

## CSS context-menu pattern duplicated between pane-tab and conversation-marker variants

**Severity:** Low - two near-third "context menu" features now share ~80% of the same CSS shell; the third copy will be the trigger for extraction but it should be promoted to a `.context-menu` family before then.

`ui/src/styles.css:3981-4023` (new `.conversation-marker-context-menu*`) and `:2506-2546` (existing `.pane-tab-context-menu*`). Same `position: fixed`, z-index ordering, `color-mix(in srgb, var(--surface-white) ...)` background pattern, `box-shadow: 0 20px 40px color-mix(in srgb, var(--ink) 14%, transparent)`, hover/focus blue mix, `*-item-danger` red. Differences are only `min-width`, `border-radius` (custom 1rem vs `var(--control-radius)`), padding values, and `border: 1px solid var(--line)` vs unbordered. The pattern is reusable as a `.context-menu` / `.context-menu-item` / `.context-menu-item-danger` family.

**Current behavior:**
- Two near-duplicate context-menu CSS blocks.
- Small variations are unique-to-call-site.
- Future third instance would copy a third near-duplicate.

**Proposal:**
- Promote the shared shell + item rules into a base `.context-menu` set.
- Let `.pane-tab-context-menu` and `.conversation-marker-context-menu` carry only their unique tweaks (`min-width`, `border-radius`, `border`, separator).
- Defer if the variations are deliberately divergent — but mark this as a known cluster so the third instance triggers extraction.

## `SessionPaneView.tsx` near-bottom early-out is captured at `isSending` flip, not reactive

**Severity:** Low - the catchup branch never schedules for the started-near-bottom-then-scrolled-away case, contrary to what the comment promises.

`ui/src/SessionPaneView.tsx:2024`. The effect has dependency array `[isSending, pane.viewMode, scrollStateKey]`, and `isMessageStackNearBottom()` reads from `messageStackRef.current` (not reactive). The effect's near-bottom decision is therefore evaluated only at the moment `isSending` flips true. If the user starts the send near bottom but scrolls away while the request is in flight, the catchup branch (`scheduleSettledScrollToBottom` via `followLatestMessageForPromptSend`) never schedules.

**Current behavior:**
- Near-bottom snapshot captured only at `isSending` true→false transition.
- User scrolling away during the in-flight request bypasses catchup.
- Comment overstates the guarantee ("schedule the settled-poll catchup here to bring the user's prompt into view once it lands").

**Proposal:**
- Add a reactive signal (e.g., a derived `nearBottomAtSendStart` captured into the effect's deps via a ref-based subscription).
- Or update the comment to match the actual behavior ("when the user is near bottom AT THE TIME isSending toggled, defer entirely to the post-message-land effect").

## Near-bottom prompt-send early return lacks direct scroll coverage

**Severity:** Medium - the prompt-send stutter fix is not directly pinned by a near-bottom pending-POST test.

`ui/src/SessionPaneView.tsx:2024` returns early when the message stack is already near bottom so the old-bottom smooth scroll does not race the later post-message scroll. Existing scroll coverage pins the far-from-bottom catch-up path, but not this near-bottom skip. A regression could reintroduce the old-target smooth scroll and visible stutter without failing the current suite.

**Current behavior:**
- Near-bottom sends skip the old-bottom smooth-scroll effect.
- Far-from-bottom prompt catch-up is covered.
- No test starts near bottom, keeps the POST pending, grows `scrollHeight`, and asserts no old-target smooth scroll fires before the prompt lands.

**Proposal:**
- Add an `App.scroll-behavior.test.tsx` case that starts near bottom, sends with a pending POST, grows `scrollHeight`, and asserts no old-target smooth scroll occurs before the prompt lands.

## Telegram command suffix parsing conflates foreign-bot commands with unknown commands

**Severity:** Medium - commands addressed to another bot can make TermAl respond, while valid suffixed setup commands can be ignored.

`src/telegram.rs:790` treats `parse_telegram_command_for_bot` returning `None` as an unknown command and sends help, even when the reason is "this command was addressed to a different bot." The unlinked setup branch still uses `parse_telegram_command(text)` without the resolved bot username, so `/start@termal_bot` and `/help@termal_bot` can be ignored in standard Telegram group-command form.

**Current behavior:**
- Foreign-bot suffixes and unknown commands share the same `None` outcome.
- Linked chats can receive TermAl help for commands addressed to another bot.
- Unlinked suffixed `/start@termal_bot` / `/help@termal_bot` are not parsed with the bot-aware parser.

**Proposal:**
- Return a typed command parse outcome such as parsed / unknown / foreign-bot.
- Ignore foreign-bot commands.
- Use the bot-aware parser in the unlinked `/start` / `/help` path once the username is known.

## Telegram-forwarded text has no per-chat rate cap

**Severity:** Medium - any linked chat can still fan out prompt submissions quickly enough to create a burst of local backend and agent work.

`src/telegram.rs:1654-1666` now rejects Telegram prompts above `MAX_DELEGATION_PROMPT_BYTES = 64 * 1024` before calling `forward_telegram_text_to_project`, but accepted prompts are still not rate-limited per chat. Command and callback actions dispatch backend work at `src/telegram.rs:1633` and `src/telegram.rs:1710`. A linked chat can submit many below-limit prompts or action commands in quick succession, each becoming local backend work and possibly an agent turn.

**Current behavior:**
- Oversized Telegram prompts are rejected by UTF-8 byte length.
- Below-limit prompts and action commands are forwarded unchanged.
- No per-minute or burst cap exists per linked chat before backend work starts.
- The default 1-second poll cadence can ingest those bursts quickly.

**Proposal:**
- Add a per-minute / per-chat prompt and action-command rate cap so a linked chat cannot fan out N HTTP calls per second.

## Telegram relay forwards full assistant text to Telegram by default

**Severity:** Medium - assistant replies can include code, local file paths, file contents, or secrets and are sent to a third-party service without an explicit opt-in.

`src/telegram.rs:1151-1160`. The relay chunks and forwards the full settled assistant message body to Telegram once the session is no longer active. This goes beyond the compact project digest and sends arbitrary model output off-machine by default.

**Current behavior:**
- The Telegram digest path is compact, but settled assistant messages are forwarded in full.
- Assistant text may contain local workspace details or user-provided secrets.
- Users enabling the relay do not get a separate opt-in for full-content forwarding.

**Proposal:**
- Make full assistant text forwarding an explicit opt-in setting.
- Keep digest-only forwarding as the default for Telegram integrations.
- Document the third-party content exposure and add any practical redaction/truncation before full forwarding.

## Telegram `getUpdates` batch processing is unbounded and re-runs on poll-iteration panic

**Severity:** Medium - a Telegram update burst (real attack, retry storm, or accidental flood) becomes a multiplicative wave of HTTP calls into the local backend, and a panic mid-batch re-runs the same updates on the next poll.

`src/telegram.rs:44-78` and `src/telegram.rs:304`. The relay accepts the entire `Vec<TelegramUpdate>` Telegram returns and walks each update through `handle_telegram_update`, which can issue multiple outbound HTTP calls per update (digest fetch, send_message, action dispatch, session fetch). State is persisted once at the end of the iteration; a panic mid-batch leaves `next_update_id` un-advanced and Telegram resends the same batch on the next poll, amplifying the effect.

**Current behavior:**
- `getUpdates` does not pass an explicit `limit`, so Telegram returns up to its server-side default (100).
- A 100-update batch can fan out to several hundred backend HTTP calls.
- Mid-batch panic loses all per-update state (including advanced `next_update_id`), and Telegram replays.

**Proposal:**
- Cap `getUpdates` `limit` (e.g., 25) on the request side.
- Persist `next_update_id` per update inside the batch loop rather than once at the end.
- Add a per-iteration backoff after errors so a sustained failure does not tight-loop.

## CSS bubble `width: fit-content` transition causes horizontal layout reflow at turn end

**Severity:** Medium - the "stable component subtree across stream → settle" goal is partially undermined by a CSS-driven layout jump.

`ui/src/styles.css:4448-4452`. `:has(.markdown-table-scroll)` applies `width: fit-content; max-width: min(96rem, 96%)` to the bubble. With the new `deferAllBlocks: true` policy a streaming bubble has NO `.markdown-table-scroll` until the turn settles (the table sits in `.markdown-streaming-fragment` instead). When streaming ends, the bubble's effective `max-width` jumps from default `42rem` to `96rem` AND its `width` switches to `fit-content` — the bubble grows wider, producing a visible horizontal reflow at the same moment the React subtree was supposed to be stable.

**Current behavior:**
- During streaming the bubble follows the prose-default sizing (`42rem` cap).
- On settle the `:has(.markdown-table-scroll)` selector engages and the bubble jumps to `fit-content` / 96rem cap.

**Proposal:**
- Anticipate `width: fit-content` while streaming if a `|`-line has been seen (e.g., a class on the streaming-fragment placeholder that triggers the same selector).
- Or: document the layout shift as accepted and add a regression that asserts bubble width remains stable across the stream→settle transition so any future drift is visible in tests.

## Mermaid aspect-ratio sizing can clip constrained diagrams

**Severity:** Medium - constrained Mermaid iframes can hide diagram content instead of only removing blank frame space.

`ui/src/mermaid-render.ts:134-152` sizes Mermaid iframes with `aspectRatio: ${frameWidth} / ${frameHeight}` + `height: auto`. The change fixes the wide-blank-frame regression, but the iframe document still keeps the SVG at intrinsic width while hiding vertical overflow. When `max-width: 100%` constrains the iframe, the frame height can shrink without the inner SVG scaling with it, so wide or tall diagrams can lose bottom content instead of simply removing unused whitespace.

**Current behavior:**
- The outer iframe height is driven by CSS `aspect-ratio` and constrained width.
- The inner Mermaid SVG can remain at intrinsic dimensions.
- The iframe hides vertical overflow, so constrained diagrams can be clipped at the bottom.

**Proposal:**
- Keep explicit intrinsic height for horizontally-scrollable diagrams, or make the inner SVG scale with the iframe width before using aspect-ratio sizing.
- Add visual/regression coverage for constrained wide and tall diagrams.

## Asymmetric `orchestrator_auto_dispatch_blocked` between two persist-failure rollback sites

**Severity:** Medium - an Error session can remain auto-dispatch-eligible after a runtime-exit commit failure while disk and memory disagree.

`src/session_lifecycle.rs:449` defensively sets `record.orchestrator_auto_dispatch_blocked = true` on persist-failure rollback in the stop-session path. `src/turn_lifecycle.rs:455` does NOT mirror that defensive set in the runtime-exit rollback path, and the inner block at `turn_lifecycle.rs:413` has already explicitly cleared the flag to `false` before the failed `commit_locked`. Net effect: if the runtime-exit commit fails, the session in-memory state is `SessionStatus::Error` with the "Turn failed: …" message, but the orchestrator can still observe it as eligible for auto-dispatch.

**Current behavior:**
- `stop_session` rollback sets `orchestrator_auto_dispatch_blocked = true` defensively.
- `handle_runtime_exit_if_matches` rollback leaves the flag at whatever the inner block last wrote (`false`).
- An Error session with a failed persist commit can still be re-dispatched.
- The new tests do not pin `orchestrator_auto_dispatch_blocked` in either rollback path.

**Proposal:**
- Either mirror the `session_lifecycle.rs` defensive set (`true`) in `turn_lifecycle.rs`, or document why the asymmetry is intentional.
- Tighten the persist-failure tests to also pin `orchestrator_auto_dispatch_blocked`, `runtime`, `runtime_stop_in_progress`, and the "stopped/failed" message presence.

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

**Severity:** Low - `ui/src/SessionPaneView.tsx` is now 3,160 lines and `ui/src/app-session-actions.ts` is 1,968 lines, both past the architecture rubric Â§9 thresholds (~2,000 for TSX components, ~1,500 for utility modules). The round-11 extractions of `connection-retry.ts`, `app-live-state-resync-options.ts`, `session-hydration-adoption.ts`, and `SessionPaneView.render-callbacks.tsx`, plus the later `action-state-adoption.ts` split, reduced these files but left them over their respective thresholds.

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

**Severity:** Low - `ui/src/App.live-state.deltas.test.tsx`. File is now 3,435 lines and 18 `it` blocks after this round's cross-instance regression coverage, well past the architecture rubric Â§9 ~2,000-line threshold for TSX files. The header already lists three sibling files split out (`reconnect`, `visibility`, `watchdog`), establishing the per-cluster split pattern.

The newest tests still cluster around hydration/restart races and cross-instance recovery, which is a coherent split boundary. Pure code move per CLAUDE.md.

**Current behavior:**
- Single test file mixes hydration races, watchdog resync, ignored deltas, orchestrator-only deltas, scroll/render coalescing, and resync-after-mismatch flows.
- 18 `it` blocks; the newest coverage adds another cross-instance state-adoption scenario.
- Per-cluster grep tax growing.

**Proposal:**
- Pure code move: extract the 4–5 hydration-focused tests into `ui/src/App.live-state.hydration.test.tsx`, mirroring the sibling-split pattern.
- Defer to a dedicated split commit; do not couple with feature changes.

## `app-live-state.ts` past 1,500-line review threshold for TypeScript utility modules

**Severity:** Low - `ui/src/app-live-state.ts`. File is now 2,435 lines after this round. The architecture rubric Â§9 sets a pragmatic ~1,500-line threshold for TypeScript utility modules. The hydration adoption helpers have moved out, but the module still mixes retry scheduling, profiling, JSON peek helpers, and the main state machine.

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

- [ ] P2: Cover real composer-to-overview focus detection:
  render the real composer/overview path or assert the real composer emits `data-conversation-composer-input`, so `ConversationOverviewRail` deferral does not depend only on synthetic test fixtures.
- [ ] P2: Cover first-chunk Telegram forward failure:
  force the first chunk of a long assistant message to fail and assert bounded retry/escalation behavior instead of an endless replay loop.
- [ ] P2: Cover first-settled active-baseline same-message growth policy:
  pin the current conservative behavior and, if a future turn-boundary signal lands, add the positive forwarding case for same-message reply text already present on first settled poll.
- [ ] P2: Add Telegram settings API/security regressions:
  cover plaintext token-at-rest exposure, corrupt-backup permission hardening, and Windows ACL/secret-store fallback behavior.
- [ ] P2: Cover post-validation Telegram settings sanitization:
  delete a project/session after validation but before the second sanitize path, or extract a deterministic helper seam, and assert the persisted response cannot retain stale references. The current stale-reference test at `src/tests/telegram.rs:1573` seeds invalid state before validation, so removing the post-validation sanitize in `src/telegram_settings.rs:73` would still pass.
- [ ] P2: Cover Telegram project-target invariant boundaries:
  pin `enabled + no token + []`, blank-token rejection precedence, and saved-token/no-project saves so the UI/backend/prune paths share one enabled-relay target contract.
- [ ] P2: Add Telegram settings file concurrency regressions:
  simulate UI config save racing relay state persistence across separate processes or an OS-lock harness, assert atomic writes prevent partial JSON reads, and assert token/config plus `chatId`/`nextUpdateId` are not lost.
- [ ] P2: Add Telegram preferences panel RTL coverage:
  cover API error display, stale default-session clearing, default-project auto-subscription, `inProcess` running/stopped lifecycle labels including stopped-over-linked precedence, AppDialogs Telegram tab path, and StrictMode-mounted save/test/remove flows proving post-await UI updates still land.
- [ ] P2: Add near-bottom prompt-send early-return scroll coverage:
  start near bottom, send with a pending POST, grow `scrollHeight`, and assert no old-target smooth scroll fires before the prompt lands.
- [ ] P2: Add waiting-indicator bottom-follow negative coverage:
  cover no duplicate scroll while the live indicator remains visible, far-from-bottom no-op, inactive pane/view not consuming the rising edge, and virtualized-transcript bottom-follow behavior.
- [ ] P2: Add virtualized bottom re-entry scroll-kind expiry coverage:
  return to bottom, cancel idle compaction, then issue a native scroll without wheel/touch/key prelude and assert stale `lastUserScrollKindRef` classification cannot leak.
- [ ] P2: Add Telegram startup-message coverage:
  assert the no-chat startup message points to `TERMAL_TELEGRAM_CHAT_ID` / trusted state binding rather than first-touch `/start`.
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
- [ ] P2: Add frontend stop/failure delta-before-snapshot terminal-message coverage:
  dispatch cancellation/update deltas before the same-revision snapshot and assert appended stop/failure terminal messages remain rendered without relying on a later unrelated refresh.
- [ ] P1: Add Telegram-relay unit tests for the pure helpers introduced in `src/telegram.rs`:
  cover `chunk_telegram_message_text` (empty, exact-3500-char, under-limit, no-newline-in-window hard-split, newline-in-window soft-split, multi-byte / emoji char-vs-UTF16-unit, trailing-newline preservation), `telegram_turn_settled_footer` for `idle` / `approval` / `error` / unknown-status arms, `telegram_error_is_message_not_modified` against the Telegram error wording, and a serde-decode round-trip for `TelegramUpdate` / `TelegramChatMessage` against a real-shape `getUpdates` JSON snapshot to pin the snake_case contract.
- [ ] P1: Add `forward_new_assistant_message_if_any` logic-level coverage:
  refactor the message-walking branch into a pure helper that takes a `Vec<TelegramSessionFetchMessage>` + state and returns a forwarding plan (or use a fake `TelegramApiClient` / `TermalApiClient`). Cover the active-status gate, the cold-start baseline policy, a Telegram-originated first reply that must be forwarded, the streaming-then-settled re-forward via char-count growth, and per-message progress recording on mid-batch send failure.
- [ ] P2: Add genuine-divergence reconciliation coverage for same-revision unknown-session deltas:
  `docs/architecture.md` now documents that session creation advances the main revision, and `ui/src/app-live-state.ts` cross-links that contract. Add a coverage test that (a) sets the client up with a session list missing `session-X`, (b) dispatches a same-revision session delta for `session-X` (where `latestStateRevisionRef.current === delta.revision`) — asserting NO immediate `/api/state` fetch, then (c) dispatches the next authoritative `state` event including `session-X` and asserts it adopts cleanly.
- [ ] P2: Finish splitting the remaining marker-menu create/remove test:
  the marker-menu coverage now has focused cases for keyboard trigger, portal cleanup, scroll/resize close, explicit trigger contract, and clamp fallback. The original create/remove test still combines add/remove, Escape focus restore, ArrowDown navigation, and rect-based clamp behavior; split the remaining assertions if it grows again.
- [ ] P2: Pin `mountedRangeWillChange` early-return absence of stale-rect scroll write:
  during the prepend integration test, capture `harness.scrollWrites` between the prepend and the followup effect and assert no scroll write lands at the stale `targetScrollTop` value computed from pre-mutation rects.
- [ ] P2: Cover Telegram relay active-project reconciliation:
  start an in-process relay with subscribed projects but no default and assert startup fails or status exposes the effective `activeProjectId`; delete a project used by a running relay and assert the relay is stopped or restarted without the deleted id.
- [ ] P2: Cover Telegram relay runtime lifecycle seam:
  add an injectable or testable relay runtime so startup from saved settings, implicit first subscribed-project fallback, invalid/missing config stop, config-save start/stop/restart, deleted-project reconciliation, runtime status `running: true` + `inProcess`, and graceful-shutdown stop are covered despite the production path's `#[cfg(not(test))]` guards.
- [ ] P2: Cover Telegram relay stop/restart quiescence:
  simulate disable or config retarget while an old relay is in flight and assert stale-generation polling/action handling cannot continue after status reports the replacement or stopped state.
- [ ] P2: Add component-level session tab tooltip remote-owner coverage:
  render tab tooltips for projectless, missing-project, missing-remote, and conflicting session/project remote proxy sessions whose summaries carry `remoteId`, complementing formatter-level coverage for session-owner precedence.
- [ ] P2: Cover remote-sync embedded remote-owner clearing:
  seed a remote snapshot session with attacker-chosen `remoteId`, localize it, and assert trusted `SessionRecord.remote_id` metadata is preserved while the embedded `record.session.remote_id` is cleared and local wire projections re-emit only trusted ownership.
- [ ] P2: Cover production-path tool/delegation id collision:
  add a Rust test that drives both the Claude task path and the delegation creation path with overlapping ids (or document the assumption that uuid id spaces don't collide deterministically). The current test manually inserts the collision.
- [ ] P2: Clean up AgentSessionPanel `act(...)` warnings:
  targeted AgentSessionPanel Vitest still emits React `act(...)` warnings around async rerenders/events; identify the warned updates and wrap or await them so timing-sensitive failures are not hidden by noisy test output.
- [ ] P2: Strengthen unmount race-condition delegation test:
  assert `console.error` was not called with `act`/`unmounted` warnings, or stub `setIsDelegationSpawning` to verify it isn't invoked post-unmount.
