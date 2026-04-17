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

  it("stops re-arming after the hard-cap deadline even when fetchState is in flight", async () => {
    // Regression for "Self-chained safety-net poll hard-cap can be
    // missed": under the previous design the cap setTimeout could fire
    // while the chained callback was awaiting fetchState(). At that
    // moment the chained ref had already been cleared to null at
    // callback entry, so the cap's `clearTimeout(ref)` no-op'd. When
    // fetchState then resolved, the callback continued and armed a
    // fresh 30s timer that outlived the 5-minute cap.
    //
    // The fix captures an absolute `deadlineMs` at scheduler start
    // and checks it BOTH before arming the next timer AND after
    // fetchState resolves. This test pins the "fetch in flight past
    // deadline" path specifically — resolving the deferred fetch
    // after the cap has lapsed must NOT produce another scheduled
    // poll.
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

      // Advance well past the 5-minute hard cap while the fetch is
      // still pending. The belt-and-suspenders cap timer fires during
      // this window, but that alone cannot stop an in-flight await —
      // the real guarantee is the post-await deadline check below.
      const remainingUntilDeadline =
        ACTIVE_PROMPT_POLL_MAX_DURATION_MS - ACTIVE_PROMPT_POLL_INTERVAL_MS;
      await vi.advanceTimersByTimeAsync(remainingUntilDeadline + 60_000);

      // Resolve the stuck fetch AFTER the deadline has lapsed.
      deferred.resolve({ ok: true });
      await vi.advanceTimersByTimeAsync(0);

      // Post-await deadline check must have bailed out. onState was
      // NOT called with the stale response, and no next poll was
      // armed.
      expect(onState).not.toHaveBeenCalled();

      // Advance another full interval — still no new fetch.
      await vi.advanceTimersByTimeAsync(ACTIVE_PROMPT_POLL_INTERVAL_MS);
      expect(fetchState).toHaveBeenCalledTimes(1);
      expect(onState).not.toHaveBeenCalled();
    } finally {
      cancel();
    }
  });

  it("still delivers the state to onState when fetchState resolves before the deadline", async () => {
    // Companion test to the in-flight-past-deadline case above: a
    // prompt-window fetch that takes several seconds but resolves
    // well inside the 5-minute cap must still call onState and
    // continue chaining. Otherwise the deadline guard would be
    // overbroad and the safety-net poll stops working on slow-but-
    // not-dead backends.
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

  it("uses an injectable now() so the deadline can be driven from fake time", async () => {
    // Mirrors the hard-cap case but using a custom `now` function so
    // the test controls the clock explicitly. Proves the scheduler
    // reads time via the injected primitive, not directly via
    // `Date.now()`.
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
