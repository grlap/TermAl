# Bugs & Known Issues

Updated against the current checked-in code in `src/main.rs`, `src/remote.rs`,
`ui/src/App.tsx`, `ui/src/App.test.tsx`, `ui/src/backend-connection.test.tsx`,
`ui/src/live-updates.ts`, `ui/src/live-updates.test.ts`, `ui/package.json`,
and `ui/vite.config.ts`.

Feature and follow-up implementation work remains in `docs/features/` and the
other planning docs under `docs/`.

This round fixed eighteen issues: (1) four `waitFor` assertions using
`toBeGreaterThan(0)` were replaced with `toHaveLength(2)`, making the two-location
render checks structurally enforced; (2) a preservation comment was added to
`requestStateResync` explaining why the `=== null` branch was removed and why the
negative case deliberately keeps the fallback armed; (3) the watchdog drift baseline
is now reset after any successful state adoption via `lastLiveSessionResumeWatchdogTickAt
= adoptedAt` inside `startStateResyncLoop`, preventing spurious follow-up resyncs; (4)
a comment at the 400 ms boundary in the new "restarts the resync loop from finally" test
explains that the reconnect timer fires but defers; (5) `orchestratorsUpdated` was added
to the SSE delta event table in `architecture.md` with its `{ revision, orchestrators[] }`
shape; (6) all three "17 CSS themes" occurrences in `architecture.md` were updated to "16";
(7) `handleWindowFocus` had its redundant `typeof document !== "undefined"` SSR guard
removed; (8) `vi.unstubAllGlobals()` was added to `afterEach` in
`backend-connection.test.tsx`; (9) the stale-snapshot test was strengthened with
`makeBackendStateResponse` and a content assertion; (10) `// Flush the React state
update from the adopted fetch response.` was added before the `await Promise.resolve()`
flush; (11) `// Mutates the effect-local transport-activity map in place.` was added to
`pruneLiveTransportActivitySessions`; (12) a comment was added to
`syncLiveTransportActivityFromState` explaining that idle entries are harmless; (13) the
new "restarts the resync loop from finally" test received an explicit
`throw new Error(\`Unexpected /api/state call #${stateRequestCount}\`)` overflow guard
and a timing synchronization comment; (14) the watchdog drift test was widened to
`LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS + 1000` ms to structurally falsify the baseline
reset; (15) overflow guards were added to two pre-existing tests ("retains rollback
permission" and adjacent); (16) `// Clear before restarting so startStateResyncLoop's
entry guard passes.` was added to the `finally` block in `App.tsx`; (17) the
`OrchestratorsUpdated` silent-drop arm in `src/remote.rs` received a TODO comment
explaining the ID-translation requirement; and (18) the `startStateResyncLoop` was
extracted from `requestStateResync` with a proper `if (!cancelled)` guard in the
`finally` block.

The following round fixed eleven more issues: (1) the `OrchestratorsUpdated`
architecture table row now carries an inline ID-scoping note ("IDs inside each
instance are scoped to the originating server; translate via `sync_remote_state_inner`
before forwarding remotely."); (2) `src/remote.rs` gained a diagnostic log and
recovery-path TODO for the `OrchestratorsUpdated` drop arm; (3) all three "16 CSS
themes" references in `architecture.md` were qualified to "16 selectable color themes
(defined in `themes.ts`)"; (4) the "finally restart" test now pins a session-name
field unique to the revision-1 response, structurally confirming `allowAuthoritativeRollback`
was preserved; (5) the "ignores focus-triggered resync while the document remains
hidden" test was added, covering the `handleWindowFocus` visibility guard; (6) a
delta-handler audit confirmed `markLiveTransportActivity` is called for every
`kind === "applied"` path, with a comment in `App.tsx` recording the invariant; (7)
the UTF-8 BOM was stripped from `ui/src/App.test.tsx` line 1; (8) the watchdog drift
test received an inline comment noting that 6000 ms <
`LIVE_SESSION_TRANSPORT_STALE_RESYNC_DELAY_MS` (15000 ms); (9) the `// Mutates the
effect-local transport-activity map in place.` comment on
`pruneLiveTransportActivitySessions` was moved above the `export function` line; (10)
the `orchestratorsUpdated` handler in `App.tsx` received a comment documenting the
intentional asymmetry with session-scoped transport-activity clocks; and (11) the
spurious blank line and nine trailing blank lines in `backend-connection.test.tsx`
were removed.

The following round fixed five more issues: (1) the live SSE `state` and
adopted `/api/state` recovery paths in `App.tsx` now resync the wake-gap
baseline for every known session, session-scoped deltas refresh only their own
session baseline, and `orchestratorsUpdated` intentionally leaves wake-gap
baselines alone, closing the slow-fetch case without letting unrelated SSE
traffic mask a different stalled session; (2) the "ignores focus-triggered
resync while the document remains hidden" test now awaits `settleAsyncUi()`
before asserting that no `/api/state` fetch was queued; (3) the queued
reconnect fallback test now comments why `revision: 1` is intentional under
`allowAuthoritativeRollback`; (4) the double blank line after the hidden-focus
test was removed; and (5) `src/remote.rs` no longer writes a per-event stderr
diagnostic for dropped remote `OrchestratorsUpdated` deltas and now explicitly
documents that remote orchestrator updates are unsupported until ID translation
plus remote orchestrator proxying exist.

The latest round fixed two more issues: (1) the hidden-focus test gained a
fetch-count overflow guard so any extra `/api/state` request now fails loudly;
and (2) a dedicated wake-gap regression now proves unrelated post-wake session
traffic plus `orchestratorsUpdated` deltas cannot suppress recovery for a
different active session before the stale-transport window becomes eligible.

The latest follow-up fixed one more issue: successful non-SSE `adoptState(...)`
paths now seed the per-session wake-gap baselines too, so locally resumed or
created active sessions still get their first drift-gap recovery if the machine
sleeps before the next watchdog tick or before any SSE event arrives. A new
approval-flow regression test covers that path directly.

The latest follow-up fixed one more test gap: a create-session regression now
proves a brand-new active session created through the REST `adoptState(...)`
path still hits the first wake-gap resync even before any SSE state arrives,
which directly covers the `syncAdoptedLiveSessionResumeWatchdogBaselinesRef`
bridge.

The latest follow-up fixed two more test-hygiene issues: (1) that create-session
wake-gap regression now restores `dateNowSpy` in `finally`, flushes deferred UI
work before asserting, documents why it intentionally keeps one real watchdog
interval instead of fake timers, and checks both the watchdog fetch plus the
disappearance of the streaming placeholder after resync; and (2) the adjacent
orchestrator-start regression was restored after an intermediate edit dropped it.

## Implementation Tasks

- None currently tracked from the latest review round.
