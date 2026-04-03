# Bugs & Known Issues

Updated against the current checked-in code in `src/main.rs`, `ui/src/App.tsx`,
`ui/src/App.test.tsx`, `ui/src/backend-connection.test.tsx`,
`ui/src/live-updates.ts`, `ui/src/live-updates.test.ts`, `ui/package.json`,
and `ui/vite.config.ts`.

Feature and follow-up implementation work remains in `docs/features/` and the
other planning docs under `docs/`.

## stateResyncInFlightRef finally block can be clobbered by old mount after Strict Mode remount

**Severity:** Medium - violates the single-in-flight resync invariant in React Strict Mode dev; produces at most a duplicate /api/state fetch, no data regression in production.

Inside the async resync loop, the outer `finally { stateResyncInFlightRef.current = false; }` runs on the old mount even after the new mount has reset the flag and started its own loop. The sequence: (1) Mount A's loop is in-flight, `stateResyncInFlightRef.current = true`. (2) Mount A's cleanup sets `cancelled_A = true`. (3) Mount B resets the ref to `false` and starts its own loop, setting it back to `true`. (4) Mount A's pending `fetchState()` resolves, hits `if (cancelled) break`, then `finally` sets `stateResyncInFlightRef.current = false`. (5) Mount B's running loop now has the flag at `false` — a subsequent `requestStateResync` call spawns a second concurrent IIFE.

In production (no Strict Mode double-mount) this never occurs. In dev the worst case is two parallel `/api/state` fetches; the monotonic revision guard in `adoptState` prevents state regression but the two-loop state is a correctness invariant violation.

**Current behavior:**
- Old mount's `finally` block runs unconditionally and writes `false` to `stateResyncInFlightRef.current`
- New mount's loop can have the flag silently cleared by the old mount mid-run
- A trigger arriving after the clobber spawns a second concurrent resync loop

**Proposal:**
- Guard the `finally` block with the effect-local `cancelled` variable:
  `finally { if (!cancelled) { stateResyncInFlightRef.current = false; } }`
  Since `cancelled` is `true` on the old mount after cleanup, this prevents the clobber. The new mount's explicit ref reset already handles the initial clear.
- Alternative: add `stateResyncGenerationRef = useRef(0)`, increment it at each effect mount, capture `const myGeneration` before the IIFE, and guard with `if (stateResyncGenerationRef.current === myGeneration)` in `finally`.

## shouldPreserveReconnectFallbackUntilSuccess captured at call time, not re-evaluated per loop iteration

**Severity:** Low - in a double-error scenario (second onerror fires during an in-flight fetch), the stale captured value may cancel the wrong reconnect fallback timer.

`shouldPreserveReconnectFallbackUntilSuccess` is evaluated once at `requestStateResync` call time and captured by the async IIFE closure. On the second-plus while-loop iteration (coalesced resyncs) or when a new `onerror` fires mid-fetch (creating a fresh `reconnectStateResyncTimeoutId` via `scheduleReconnectStateResync`), the stale boolean can incorrectly fire `clearReconnectStateResyncTimeout()` on a timer that belongs to the new error event, cancelling a legitimate safety-net fallback. In the worst case the client relies on the stream self-recovering without the 400ms probe.

**Current behavior:**
- `const shouldPreserveReconnectFallbackUntilSuccess = reconnectStateResyncTimeoutId !== null && !sawReconnectOpenSinceLastError` captured once at function entry
- If a second `onerror` fires during the fetch and sets a new timer, the captured `true` causes `clearReconnectStateResyncTimeout()` to cancel that new timer upon successful adoption
- The second error's 400ms fallback probe is silently skipped

**Proposal:**
- Re-read `reconnectStateResyncTimeoutId !== null && !sawReconnectOpenSinceLastError` directly inside the `if (adopted)` block instead of using the call-time snapshot, consistent with how `allowAuthoritativeRollback` and `preserveWatchdogCooldown` are consumed from refs per-iteration rather than captured once.

## P2

- [ ] Fix `stateResyncInFlightRef` clobber after Strict Mode remount: guard the `finally` block
  in the async resync loop with `if (!cancelled)` so the old mount cannot reset the ref after
  the new mount has taken ownership.
- [ ] Fix `shouldPreserveReconnectFallbackUntilSuccess` stale closure: re-read
  `reconnectStateResyncTimeoutId !== null && !sawReconnectOpenSinceLastError` inside
  `if (adopted)` instead of capturing the boolean at `requestStateResync` call time.
- [ ] Remove `typeof document !== "undefined"` guard from `handleWindowFocus` to match
  `handleLiveSessionResumeWatchdogTick` and `handleVisibilityChange` — this is a client-only SPA.
- [ ] Add `vi.unstubAllGlobals()` to the `afterEach` in `backend-connection.test.tsx` as a
  backstop when a test throws before its own `try` block.
- [ ] Add a post-fallback UI assertion to "keeps the reconnect fallback fetch armed when a
  pre-reopen gap resync only gets a stale state snapshot": change the second mock fetch to
  return a `makeBackendStateResponse` with a recognizable preview and assert
  `screen.getByText(...)` after the final `advanceTimersByTimeAsync(1)`.
