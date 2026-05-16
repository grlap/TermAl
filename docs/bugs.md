# Bugs & Known Issues

This file tracks only reproduced, current issues and open review follow-up
tasks. Resolved work, fixed-history notes, speculative refactors, cleanup notes,
and external limitations do not belong here. Review follow-up task items live in
the Implementation Tasks section.

## Active Repo Bugs

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

## `TelegramRelayRuntime` is a file-level global rather than `AppState`-owned state

**Severity:** Note - `src/telegram.rs:220-331`. `TelegramRelayRuntime` and `TELEGRAM_RELAY_RUNTIME` are file-level globals (`LazyLock<Mutex<...>>`). `AppState` has no visibility into the relay's running state, so any future health-monitor, restart-on-error, or readiness-signaling logic ends up reading globals instead of methods on `AppState`.

**Current behavior:**
- Runtime state lives in module-level statics.
- Test injection is harder; production-vs-test parity is structural.

**Proposal:**
- Move the runtime into `AppState` and own its lifecycle on the state object.

## `src/telegram.rs` past 1500-line architecture rubric threshold

**Severity:** Medium - file now exceeds 1766 lines after round 56. CLAUDE.md asks for smaller modules.

`src/telegram.rs`. Round 56 added `backup_corrupt_telegram_bot_file`, `telegram_command_mentions_other_bot`, and digest-failure branches on top of the round-55 baseline. Mixes: HTTP client, TermAl client, wire types, command parser, digest renderer, assistant-forwarding cursor logic, corrupt-file backup helper, and the relay loop. `telegram_settings.rs` already extracted the UI surface; the next natural cut is `telegram_relay.rs` + `telegram_clients.rs` + `telegram_wire.rs`.

**Current behavior:**
- One file owns seven concerns now.
- Continued growth pattern across recent rounds.

**Proposal:**
- Split into 2-3 modules mirroring the api.rs/wire.rs split shape.
- Defer to a dedicated pure-code-move commit per CLAUDE.md.

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

## `AgentSessionPanel.tsx` exceeds 2000-line architecture rubric threshold

**Severity:** Note - `ui/src/panels/AgentSessionPanel.tsx` remains over the documented TSX file-size budget after the composer auto-resize state machine was split out to `ui/src/panels/useComposerAutoResize.ts`, agent-command submission helpers moved to `ui/src/panels/session-agent-command-submission.ts`, and waiting-output helpers were reused from `ui/src/SessionPaneView.waiting-indicator.ts`.

The resize/transition refs are now isolated, but the panel still mixes session header, footer orchestration, command palette, attachments, and send/delegate control flow. The next split should keep reducing production TSX surface rather than adding more local state.

**Current behavior:**
- `AgentSessionPanel.tsx` is about 3,057 lines.
- `AgentSessionPanel.test.tsx` was split into focused sibling files; `AgentSessionPanel.tsx` remains over the production TSX threshold.
- Composer auto-resize, transition restoration, agent-command submission/error handling, and waiting-output classification now live in focused helpers, but the remaining composer orchestration is still embedded in the broader panel.

**Proposal:**
- Continue extracting focused panel concerns, such as composer command-palette orchestration or footer/send controls, into smaller hook/component modules with split-provenance headers.
- Keep targeted tests with the extracted concerns so the remaining panel can become mostly composition.

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

## `app-live-state.ts` reconnect state machine continues to grow

**Severity:** Low - `ui/src/app-live-state.ts:2504 lines`. TS utility threshold (1500) exceeded; new `pendingBadLiveEventRecovery` adds another flag-shaped piece of reconnect bookkeeping. The reconnect/resync state machine inside `useEffect` now coordinates 6+ pieces of cross-cutting state.

**Proposal:**
- Extract a `ReconnectStateMachine` (or similar) module that owns the flag set + transitions and exposes named events (`onSseError`, `onSseReopen`, `onBadLiveEvent`, `onSnapshotAdopted`, `onLiveEventConfirmed`).
- Defer to a pure code-move commit per CLAUDE.md.

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

**Severity:** Low - `ui/src/SessionPaneView.tsx` is still about 3,661 lines and `ui/src/app-session-actions.ts` is still about 2,089 lines after the latest small helper splits, both past the architecture rubric thresholds (~2,000 for TSX components, ~1,500 for utility modules).

The waiting-indicator helpers now live in `ui/src/SessionPaneView.waiting-indicator.ts`, the session-settings optimism helpers now live in `ui/src/app-session-settings-optimism.ts`, session-settings API payload construction now lives in `ui/src/app-session-settings-payload.ts`, conversation-marker response matching now lives in `ui/src/conversation-marker-response-match.ts`, optimistic pending prompt construction now lives in `ui/src/optimistic-pending-prompt.ts`, draft ref/store sync now lives in `ui/src/app-session-draft-sync.ts`, local marker session transforms now live in `ui/src/conversation-marker-session-mutations.ts`, marker create-request construction now lives in `ui/src/conversation-marker-requests.ts`, new-session model request selection now lives in `ui/src/app-session-model-requests.ts`, and draft attachment collection transforms now live in `ui/src/app-session-draft-attachments.ts`. Those moves reduced local clutter and gave the helpers direct unit-testable surfaces, but the main production modules remain over threshold.

**Current behavior:**
- `SessionPaneView.tsx` still owns pane orchestration, tab rendering, scroll/follow behavior, panel selection, and composer/footer wiring.
- `app-session-actions.ts` still owns prompt send, draft attachment lifecycle, session creation, stop/kill/rename, settings changes, model refresh, Codex thread actions, and marker mutations.
- Both files now have small helper splits, but neither production module is below the review threshold.

**Proposal:**
- Continue with dedicated pure-code-move commits per CLAUDE.md.
- For `SessionPaneView.tsx`: extract session-find/scroll-follow and panel tab orchestration clusters.
- For `app-session-actions.ts`: extract prompt send/draft lifecycle and marker/session-settings action groups into focused modules.

## `app-live-state.ts` past 1,500-line review threshold for TypeScript utility modules

**Severity:** Low - `ui/src/app-live-state.ts`. File is still about 3,420 lines after this round. The architecture rubric sets a pragmatic ~1,500-line threshold for TypeScript utility modules. Hydration adoption, lightweight state-event profiling/JSON metadata helpers, delta-event guards, delegation-wait list helpers, and unknown-model confirmation pruning have moved out, but the module still mixes retry scheduling, reconnect recovery, hydration, workspace-file events, and the main state machine.

**Current behavior:**
- Single module still mixes hydration matching, retry scheduling, reconnect recovery, workspace-file events, and the main state machine.
- Per-cluster grep tax growing with each round.

**Proposal:**
- Defer to a dedicated pure-code-move commit per CLAUDE.md.
- Extract reconnect recovery and hydration retry scheduling into focused helpers with matching unit tests.

## `src/tests/remote.rs` past the 5,000-line review threshold

**Severity:** Low - `src/tests/remote.rs` is now 9,202 lines after this round's +471-line addition, well past the project's review-threshold for test files. The new replay-cache work clusters cohesively between lines ~2,810 and ~4,040 (the `RemoteDeltaReplayCache` shape helper, the `local_replay_test_remote` / `seed_loaded_remote_proxy_session` / `assert_delta_publishes_once_then_replay_skips` / `assert_remote_delta_replay_cache_shape` / `test_remote_delta_replay_key` helpers, and the `remote_delta_replay_*` tests).

The growth is incremental across many rounds of replay-cache hardening, not a single landing — but extracting the cluster keeps the rest of the file's per-test density manageable. Per `CLAUDE.md`, splits must be pure code moves and live in their own commit.

**Current behavior:**
- Single `src/tests/remote.rs` mixes hydration tests, orchestrator-sync tests, replay-cache tests, and protocol-shape tests.
- Per-cluster grep is harder than necessary; future replay-cache work continues to grow the file.

**Proposal:**
- Extract the replay-cache cluster (lines ~2,810–4,040) into `src/tests/remote_delta_replay.rs` as a pure code move — including the helpers and all `remote_delta_replay_*` tests.
- Defer to a dedicated split commit; do not couple with feature changes.

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

## `message-cards.tsx` still owns Markdown, code, Mermaid, KaTeX, and diff rendering as one module

**Severity:** Low - `ui/src/message-cards.tsx` is still a broad renderer module even after the deferred heavy-content activation provider was split out.

The activation gate now lives in `ui/src/deferred-heavy-content-activation.tsx`, so virtualization policy has a clearer boundary. The remaining heavy rendering paths still share one file with message-card composition, which keeps future performance work coupled to a large renderer surface.

**Current behavior:**
- Markdown, code, Mermaid, KaTeX, diff, and message-card composition all live in `ui/src/message-cards.tsx`.
- The deferred activation provider/hook has its own focused module and direct consumers.

**Proposal:**
- Extract heavy Markdown/code rendering paths into focused modules so virtualization policy and content rendering can evolve independently.

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
- `/api/state` resync still reads full response bodies as text before JSON parsing; `looksLikeHtmlResponse(...)` now does only a narrow prefix check, so the remaining avoidable CPU is the text-body path itself on large successful snapshots.
- Transcript virtualization still spends measurable time on regex-heavy height estimation and synchronous layout reads, so live session churn compounds with scroll/measure work instead of staying isolated to the active status surface.

**Proposal:**
- Make the live state path more metadata-first so transcript arrays, workspace layout, and per-session maps are not reconciled or pruned when the incoming snapshot did not materially change those slices.
- Split the `/api/state` response handling into a cheap JSON-first path while preserving the narrow HTML fallback check for old-backend responses.
- Cache height-estimation inputs by message identity/revision and reduce repeated `getBoundingClientRect()` passes in the virtualized transcript.
- Re-profile the focused active-session path after each cut and keep this issue open until long-task bursts drop back below user-visible jank thresholds.

**Plan:**
- Start at the root of the profile: cut `handleStateEvent(...)` / `adoptState(...)` work first, because that is where both the passive and targeted rounds spend the most app CPU.
- Break the work into independently measurable slices: state adoption fan-out, `/api/state` parsing path, and transcript virtualization measurement/estimation.
- After each slice lands, rerun the live active-session profile and the focused typing round so reductions in `handleStateEvent(...)` self time, `TaskDuration`, and next-frame latency are verified instead of assumed.

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

## Implementation Tasks

- [ ] P2: Cover first-chunk Telegram forward failure:
  force the first chunk of a long assistant message to fail and assert bounded retry/escalation behavior instead of an endless replay loop.
- [ ] P2: Cover first-settled active-baseline same-message growth policy:
  pin the current conservative behavior and, if a future turn-boundary signal lands, add the positive forwarding case for same-message reply text already present on first settled poll.
- [ ] P2: Add Telegram settings API/security regressions:
  cover plaintext token-at-rest exposure, corrupt-backup permission hardening, and credential-store failure/fallback behavior beyond the native-store smoke test.
- [ ] P2: Cover post-validation Telegram settings sanitization:
  delete a project/session after validation but before the second sanitize path, or extract a deterministic helper seam, and assert the persisted response cannot retain stale references. The current stale-reference test at `src/tests/telegram.rs:1573` seeds invalid state before validation, so removing the post-validation sanitize in `src/telegram_settings.rs:73` would still pass.
- [ ] P2: Add Telegram settings file concurrency regressions:
  simulate UI config save racing relay state persistence across separate processes or an OS-lock harness, assert atomic writes prevent partial JSON reads, and assert token/config plus `chatId`/`nextUpdateId` are not lost.
- [ ] P2: Add Telegram preferences panel RTL coverage:
  cover API error display, stale default-session clearing, default-project auto-subscription, `inProcess` running/stopped lifecycle labels including stopped-over-linked precedence, AppDialogs Telegram tab path, and StrictMode-mounted save/test/remove flows proving post-await UI updates still land.
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
- [ ] P2: Add remote hydration dedupe production-path coverage:
  drive bursty same-session remote deltas through the production hydration path, assert only one remote session fetch is issued, and assert the in-flight guard is cleared after successful hydration.
- [ ] P2: Add failed manual retry reconnect-rearm regression:
  cover manual retry hitting a transient failure, then the next scheduled attempt adopting a newer same-instance snapshot while polling still continues until SSE confirms.
- [ ] P1: Add `forward_new_assistant_message_if_any` logic-level coverage:
  refactor the message-walking branch into a pure helper that takes a `Vec<TelegramSessionFetchMessage>` + state and returns a forwarding plan (or use a fake `TelegramApiClient` / `TermalApiClient`). Cover the active-status gate, the cold-start baseline policy, a Telegram-originated first reply that must be forwarded, the streaming-then-settled re-forward via char-count growth, and per-message progress recording on mid-batch send failure.
- [ ] P2: Document active Telegram reply forwarding invariants:
  add a short contract comment near the active forwarding gate in `src/telegram.rs` explaining when Telegram may see active output, what cursor metadata must preserve, and how settled replacement/divergence is expected to be handled.
- [ ] P2: Cover Telegram relay active-project reconciliation:
  start an in-process relay with subscribed projects but no default and assert startup fails or status exposes the effective `activeProjectId`; delete a project used by a running relay and assert the relay is stopped or restarted without the deleted id.
- [ ] P2: Cover Telegram relay runtime lifecycle seam:
  add an injectable or testable relay runtime so startup from saved settings, implicit first subscribed-project fallback, invalid/missing config stop, config-save start/stop/restart, deleted-project reconciliation, runtime status `running: true` + `inProcess`, and graceful-shutdown stop are covered despite the production path's `#[cfg(not(test))]` guards.
