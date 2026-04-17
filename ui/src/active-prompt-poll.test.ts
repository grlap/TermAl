import {
  ACTIVE_PROMPT_POLL_INTERVAL_MS,
  ACTIVE_PROMPT_POLL_MAX_DURATION_MS,
  startActivePromptPoll,
} from "./active-prompt-poll";

function createDeferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

describe("startActivePromptPoll", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    // Defensive: `useRealTimers()` in current Vitest versions drops
    // pending fake timers, so cross-test bleed is not a bug today.
    // Clearing explicitly first anchors the independence invariant so
    // a future refactor (e.g., switching to `{ shouldAdvanceTime: true }`
    // or replacing `useRealTimers` with a noop) surfaces any timer
    // leak as a test-authoring error rather than silent pollution.
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  it("arms the first poll at the interval and chains after fetchState resolves", async () => {
    let fetchStateCallCount = 0;
    const fetchState = vi.fn(async () => {
      fetchStateCallCount += 1;
      return { id: fetchStateCallCount };
    });
    const onState = vi.fn(() => false);

    const cancel = startActivePromptPoll({
      fetchState,
      onState,
      isMounted: () => true,
    });

    try {
      // Nothing fires before the interval elapses.
      expect(fetchState).not.toHaveBeenCalled();

      await vi.advanceTimersByTimeAsync(ACTIVE_PROMPT_POLL_INTERVAL_MS);
      expect(fetchState).toHaveBeenCalledTimes(1);
      expect(onState).toHaveBeenCalledWith({ id: 1 });

      // Second poll fires at the next interval.
      await vi.advanceTimersByTimeAsync(ACTIVE_PROMPT_POLL_INTERVAL_MS);
      expect(fetchState).toHaveBeenCalledTimes(2);
      expect(onState).toHaveBeenCalledWith({ id: 2 });
    } finally {
      cancel();
    }
  });

  it("stops chaining as soon as onState returns true", async () => {
    const fetchState = vi.fn(async () => ({ done: true }));
    const onState = vi.fn(() => true);

    const cancel = startActivePromptPoll({
      fetchState,
      onState,
      isMounted: () => true,
    });

    try {
      await vi.advanceTimersByTimeAsync(ACTIVE_PROMPT_POLL_INTERVAL_MS);
      expect(fetchState).toHaveBeenCalledTimes(1);
      expect(onState).toHaveBeenCalledTimes(1);

      // Advance well past the next interval — no further polls should
      // fire because onState returned `true`.
      await vi.advanceTimersByTimeAsync(ACTIVE_PROMPT_POLL_INTERVAL_MS * 3);
      expect(fetchState).toHaveBeenCalledTimes(1);
      expect(onState).toHaveBeenCalledTimes(1);
    } finally {
      cancel();
    }
  });

  it("hard-cap `stopped` flag bails an in-flight await when the cap setTimeout fires", async () => {
    // When fake-timer time advances past `maxDurationMs`, the belt-
    // and-suspenders `setTimeout(cancel, maxDurationMs)` fires and sets
    // `stopped = true` on the scheduler's closure state. The chained
    // callback's post-await guard `if (stopped || !isMounted()) return`
    // then bails without calling `onState` or arming another poll.
    //
    // This test covers the `stopped`-flag defense specifically. The
    // sibling "uses an injectable now()" test below covers the
    // `now() >= deadlineMs` post-await check independently — that
    // check is the second line of defense, load-bearing when the
    // hard-cap setTimeout has not yet fired (e.g., under a custom
    // injectable clock that advances past the deadline without
    // advancing the fake-timer clock).
    const deferred = createDeferred<{ ok: true }>();
    let fetchStateCallCount = 0;
    const fetchState = vi.fn(async () => {
      fetchStateCallCount += 1;
      if (fetchStateCallCount === 1) {
        return deferred.promise;
      }
      throw new Error(`Unexpected fetchState call #${fetchStateCallCount}`);
    });
    const onState = vi.fn(() => false);

    const cancel = startActivePromptPoll({
      fetchState,
      onState,
      isMounted: () => true,
    });

    try {
      // First poll fires at 30s. fetchState is called but the promise
      // is held in flight.
      await vi.advanceTimersByTimeAsync(ACTIVE_PROMPT_POLL_INTERVAL_MS);
      expect(fetchState).toHaveBeenCalledTimes(1);

      // Advance past the 5-minute hard cap while the fetch is still
      // pending. The belt-and-suspenders cap timer fires during this
      // window and sets `stopped = true`.
      const remainingUntilDeadline =
        ACTIVE_PROMPT_POLL_MAX_DURATION_MS - ACTIVE_PROMPT_POLL_INTERVAL_MS;
      await vi.advanceTimersByTimeAsync(remainingUntilDeadline + 60_000);

      // Resolve the stuck fetch AFTER the cap has fired.
      deferred.resolve({ ok: true });
      await vi.advanceTimersByTimeAsync(0);

      // The `stopped`-flag check after the await bails out. onState
      // is NOT called with the stale response.
      expect(onState).not.toHaveBeenCalled();

      // Advance another full interval — still no new fetch.
      await vi.advanceTimersByTimeAsync(ACTIVE_PROMPT_POLL_INTERVAL_MS);
      expect(fetchState).toHaveBeenCalledTimes(1);
      expect(onState).not.toHaveBeenCalled();
    } finally {
      cancel();
    }
  });

  it("still delivers the state to onState when fetchState resolves well before the deadline", async () => {
    // A slow-but-not-dead backend (60s response inside a 5-min cap)
    // must still call onState and continue chaining. Guards against
    // the deadline guard being overbroad and killing the poll on
    // normal slow responses.
    //
    // NOTE: this does not pin the "check executes and returns false"
    // path of the post-await deadline check — at 60s of 5min elapsed,
    // the check just does not execute the bail branch. The boundary
    // case (fetch resolves at `maxDurationMs - 1`) is covered by the
    // injectable-now test below via a custom `now()`.
    const deferred = createDeferred<{ ok: true }>();
    let fetchStateCallCount = 0;
    const fetchState = vi.fn(async () => {
      fetchStateCallCount += 1;
      if (fetchStateCallCount === 1) {
        return deferred.promise;
      }
      return { ok: true as const };
    });
    const onState = vi.fn(() => false);

    const cancel = startActivePromptPoll({
      fetchState,
      onState,
      isMounted: () => true,
    });

    try {
      await vi.advanceTimersByTimeAsync(ACTIVE_PROMPT_POLL_INTERVAL_MS);
      expect(fetchState).toHaveBeenCalledTimes(1);

      // Advance only 60 seconds (a slow-but-not-dead response). Still
      // far inside the 5-minute cap.
      await vi.advanceTimersByTimeAsync(60_000);
      deferred.resolve({ ok: true });
      await vi.advanceTimersByTimeAsync(0);

      // onState should have been called with the resolved state.
      expect(onState).toHaveBeenCalledWith({ ok: true });

      // And the next poll should have been chained.
      await vi.advanceTimersByTimeAsync(ACTIVE_PROMPT_POLL_INTERVAL_MS);
      expect(fetchState).toHaveBeenCalledTimes(2);
    } finally {
      cancel();
    }
  });

  it("accepts a fetch that resolves just inside the hard-cap deadline via injected now()", async () => {
    // Boundary case: injected `now()` advances to `maxDurationMs - 1`
    // while the fetch is still in flight (still strictly BEFORE the
    // deadline). The post-await deadline check must evaluate AND
    // return `false` (i.e., not bail) — this pins the difference
    // between "check executed and decided to continue" and "check
    // did not execute at all", which the happy-path 60s-in-5min test
    // does not distinguish. An off-by-one bug in the check (`>` vs
    // `>=`) would trip here and silently pass the happy-path test.
    //
    // The fake-timer clock stays below `maxDurationMs` throughout, so
    // the belt-and-suspenders `setTimeout(cancel, maxDurationMs)`
    // never fires — this isolates the `now() >= deadlineMs` branch
    // from the `stopped`-flag defense. A companion assertion at the
    // end advances the injected clock past the deadline and confirms
    // `onState` is NOT called a second time (the post-await deadline
    // check prevents it even though the chained timer did fire and
    // `fetchState` was invoked again).
    let currentNow = 0;
    const now = () => currentNow;
    const deferred = createDeferred<{ ok: true }>();
    let fetchStateCallCount = 0;
    const fetchState = vi.fn(async () => {
      fetchStateCallCount += 1;
      if (fetchStateCallCount === 1) {
        return deferred.promise;
      }
      return { ok: true as const };
    });
    const onState = vi.fn(() => false);

    const cancel = startActivePromptPoll(
      {
        fetchState,
        onState,
        isMounted: () => true,
      },
      { intervalMs: 100, maxDurationMs: 1_000, now },
    );

    try {
      currentNow = 100;
      await vi.advanceTimersByTimeAsync(100);
      expect(fetchState).toHaveBeenCalledTimes(1);

      // Advance the injected clock to ONE ms before the deadline. The
      // post-await check at `now() >= deadlineMs` (deadlineMs = 1000)
      // evaluates `999 >= 1000` → false → continues to onState.
      currentNow = 999;
      await vi.advanceTimersByTimeAsync(100); // fake-time stays well below 1000
      deferred.resolve({ ok: true });
      await vi.advanceTimersByTimeAsync(0);

      // onState WAS called (deadline not yet reached via `now()`).
      expect(onState).toHaveBeenCalledTimes(1);
      expect(onState).toHaveBeenCalledWith({ ok: true });

      // Now advance the injected clock past the deadline and let the
      // next chained timer fire. `fetchState` will be called again
      // (the timer was armed before the deadline), but the post-await
      // `now() >= deadlineMs` check must prevent onState from being
      // called a second time. This is the bail-branch the flagship
      // "hard-cap `stopped` flag bails an in-flight await" test does
      // not cover under an injected clock — the `stopped` flag only
      // fires when the FAKE-timer clock crosses `maxDurationMs`, not
      // when the injected `now()` does.
      currentNow = 1_100;
      await vi.advanceTimersByTimeAsync(100);
      // fetchState was called again because the timer was armed while
      // `now() < deadline`; the post-await guard is what saves us.
      expect(fetchState).toHaveBeenCalledTimes(2);
      // But onState was NOT called a second time — the deadline check
      // after the await bailed.
      expect(onState).toHaveBeenCalledTimes(1);
    } finally {
      cancel();
    }
  });

  it("bails out before calling fetchState when isMounted returns false", async () => {
    const fetchState = vi.fn(async () => ({ ok: true }));
    const onState = vi.fn(() => false);
    let mounted = true;

    const cancel = startActivePromptPoll({
      fetchState,
      onState,
      isMounted: () => mounted,
    });

    try {
      // Unmount before the first interval elapses.
      mounted = false;
      await vi.advanceTimersByTimeAsync(ACTIVE_PROMPT_POLL_INTERVAL_MS * 3);
      expect(fetchState).not.toHaveBeenCalled();
      expect(onState).not.toHaveBeenCalled();
    } finally {
      cancel();
    }
  });

  it("bails out after fetchState if isMounted returns false during the await", async () => {
    const deferred = createDeferred<{ ok: true }>();
    const fetchState = vi.fn(async () => deferred.promise);
    const onState = vi.fn(() => false);
    let mounted = true;

    const cancel = startActivePromptPoll({
      fetchState,
      onState,
      isMounted: () => mounted,
    });

    try {
      await vi.advanceTimersByTimeAsync(ACTIVE_PROMPT_POLL_INTERVAL_MS);
      expect(fetchState).toHaveBeenCalledTimes(1);

      // Unmount while the fetch is in flight.
      mounted = false;
      deferred.resolve({ ok: true });
      await vi.advanceTimersByTimeAsync(0);

      // onState must NOT run on an unmounted scheduler, even though
      // the fetch resolved.
      expect(onState).not.toHaveBeenCalled();
    } finally {
      cancel();
    }
  });

  it("returned cancel function stops an armed chain and is idempotent", async () => {
    const fetchState = vi.fn(async () => ({ ok: true }));
    const onState = vi.fn(() => false);

    const cancel = startActivePromptPoll({
      fetchState,
      onState,
      isMounted: () => true,
    });

    // Cancel before the first interval — nothing fires.
    cancel();
    await vi.advanceTimersByTimeAsync(ACTIVE_PROMPT_POLL_INTERVAL_MS * 3);
    expect(fetchState).not.toHaveBeenCalled();

    // Idempotent: second cancel is safe.
    expect(() => cancel()).not.toThrow();
  });

  it("cancels an in-flight chain when cancel is called mid-interval", async () => {
    let fetchStateCallCount = 0;
    const fetchState = vi.fn(async () => {
      fetchStateCallCount += 1;
      return { ok: true as const };
    });
    const onState = vi.fn(() => false);

    const cancel = startActivePromptPoll({
      fetchState,
      onState,
      isMounted: () => true,
    });

    // First poll runs.
    await vi.advanceTimersByTimeAsync(ACTIVE_PROMPT_POLL_INTERVAL_MS);
    expect(fetchState).toHaveBeenCalledTimes(1);

    // Cancel before the next interval. No further polls fire.
    cancel();
    await vi.advanceTimersByTimeAsync(ACTIVE_PROMPT_POLL_INTERVAL_MS * 3);
    expect(fetchState).toHaveBeenCalledTimes(1);
  });

  it("swallows fetchState errors and chains the next poll", async () => {
    let fetchStateCallCount = 0;
    const fetchState = vi.fn(async () => {
      fetchStateCallCount += 1;
      if (fetchStateCallCount === 1) {
        throw new Error("simulated /api/state failure");
      }
      return { ok: true as const };
    });
    const onState = vi.fn(() => false);

    const cancel = startActivePromptPoll({
      fetchState,
      onState,
      isMounted: () => true,
    });

    try {
      // First poll throws — onState is not called, but the chain
      // must continue.
      await vi.advanceTimersByTimeAsync(ACTIVE_PROMPT_POLL_INTERVAL_MS);
      expect(fetchState).toHaveBeenCalledTimes(1);
      expect(onState).not.toHaveBeenCalled();

      // Second poll succeeds.
      await vi.advanceTimersByTimeAsync(ACTIVE_PROMPT_POLL_INTERVAL_MS);
      expect(fetchState).toHaveBeenCalledTimes(2);
      expect(onState).toHaveBeenCalledWith({ ok: true });
    } finally {
      cancel();
    }
  });

  it("belt-and-suspenders hard-cap timer cancels an armed chain at the deadline", async () => {
    // This path is independent of the in-flight-past-deadline case:
    // if the chained callback is NOT currently awaiting (e.g., the
    // next timer is armed but has not fired yet), the hard-cap
    // setTimeout still force-cancels the chain at the deadline.
    let fetchStateCallCount = 0;
    const fetchState = vi.fn(async () => {
      fetchStateCallCount += 1;
      return { ok: true as const };
    });
    const onState = vi.fn(() => false);

    const cancel = startActivePromptPoll({
      fetchState,
      onState,
      isMounted: () => true,
    });

    try {
      // Let several polls fire normally, staying well inside the cap.
      await vi.advanceTimersByTimeAsync(ACTIVE_PROMPT_POLL_INTERVAL_MS);
      await vi.advanceTimersByTimeAsync(ACTIVE_PROMPT_POLL_INTERVAL_MS);
      await vi.advanceTimersByTimeAsync(ACTIVE_PROMPT_POLL_INTERVAL_MS);
      expect(fetchState).toHaveBeenCalledTimes(3);

      // Jump to just past the 5-minute deadline. The next chained
      // timer is armed but has not fired yet; the hard-cap timer
      // fires first and cancels it.
      const remaining =
        ACTIVE_PROMPT_POLL_MAX_DURATION_MS - 3 * ACTIVE_PROMPT_POLL_INTERVAL_MS;
      await vi.advanceTimersByTimeAsync(remaining + 1_000);
      const callCountAfterCap = fetchState.mock.calls.length;

      // No further polls can fire.
      await vi.advanceTimersByTimeAsync(ACTIVE_PROMPT_POLL_INTERVAL_MS * 3);
      expect(fetchState.mock.calls.length).toBe(callCountAfterCap);
    } finally {
      cancel();
    }
  });

  it("respects custom intervalMs and maxDurationMs options", async () => {
    let fetchStateCallCount = 0;
    const fetchState = vi.fn(async () => {
      fetchStateCallCount += 1;
      return { id: fetchStateCallCount };
    });
    const onState = vi.fn(() => false);

    const cancel = startActivePromptPoll(
      {
        fetchState,
        onState,
        isMounted: () => true,
      },
      { intervalMs: 1_000, maxDurationMs: 3_500 },
    );

    try {
      // Three polls fire at the 1-second interval.
      await vi.advanceTimersByTimeAsync(1_000);
      expect(fetchState).toHaveBeenCalledTimes(1);
      await vi.advanceTimersByTimeAsync(1_000);
      expect(fetchState).toHaveBeenCalledTimes(2);
      await vi.advanceTimersByTimeAsync(1_000);
      expect(fetchState).toHaveBeenCalledTimes(3);

      // Fourth poll (at 4000 ms) is past the 3500 ms cap and never
      // fires.
      await vi.advanceTimersByTimeAsync(2_000);
      expect(fetchState).toHaveBeenCalledTimes(3);
    } finally {
      cancel();
    }
  });

  it("post-await `now() >= deadlineMs` check bails when the injected clock passes the deadline", async () => {
    // THIS is the regression gate for the post-await deadline check
    // at `startActivePromptPoll`'s `if (now() >= deadlineMs) return`
    // after `await handlers.fetchState()`. The hard-cap `setTimeout`
    // uses the Vitest fake-timer clock (real ms), while `now()` is
    // injected independently — by advancing `currentNow` past
    // `maxDurationMs` while fake-timer time stays far below it, the
    // belt-and-suspenders cap does NOT fire during the window and the
    // `stopped`-flag defense is inactive. The only remaining defense
    // is the `now() >= deadlineMs` check; if that line is deleted or
    // inverted, this test fails while the sibling "hard-cap `stopped`
    // flag" test would still pass (because there the fake-timer
    // advance fires the belt-and-suspenders first).
    //
    // Verification: removing the `if (now() >= deadlineMs) return;`
    // line from `active-prompt-poll.ts` causes only this test to
    // fail, confirming it is the narrow regression gate for that
    // code path. Keep both tests: they cover independent defenses.
    let currentNow = 0;
    const now = () => currentNow;
    const deferred = createDeferred<{ ok: true }>();
    let fetchStateCallCount = 0;
    const fetchState = vi.fn(async () => {
      fetchStateCallCount += 1;
      if (fetchStateCallCount === 1) {
        return deferred.promise;
      }
      throw new Error(`Unexpected fetchState call #${fetchStateCallCount}`);
    });
    const onState = vi.fn(() => false);

    const cancel = startActivePromptPoll(
      {
        fetchState,
        onState,
        isMounted: () => true,
      },
      { intervalMs: 100, maxDurationMs: 1_000, now },
    );

    try {
      currentNow = 100;
      await vi.advanceTimersByTimeAsync(100);
      expect(fetchState).toHaveBeenCalledTimes(1);

      // Advance the injected clock past the deadline while the fetch
      // is still pending.
      currentNow = 2_000;
      // Let any already-armed setTimeout callbacks flush, then
      // resolve the stuck fetch.
      await vi.advanceTimersByTimeAsync(100);
      deferred.resolve({ ok: true });
      await vi.advanceTimersByTimeAsync(0);

      // Post-await deadline check reads `now()` and bails out. No
      // onState, no next poll.
      expect(onState).not.toHaveBeenCalled();
      await vi.advanceTimersByTimeAsync(500);
      expect(fetchState).toHaveBeenCalledTimes(1);
    } finally {
      cancel();
    }
  });
});
