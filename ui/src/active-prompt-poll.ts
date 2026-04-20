// Chained-`setTimeout` safety-net poller for the active-prompt window
// (see `handleSend` in `App.tsx`). The runtime contract is:
//
// - After the user sends a prompt, `startActivePromptPoll` arms a
//   chained `setTimeout` that hits `fetchState` every `intervalMs`
//   until either (a) the session returns to Idle (via `onState`
//   returning `true`), (b) the caller calls the returned `cancel`
//   function (e.g. on component unmount or when the next prompt
//   starts its own poll), or (c) the `maxDurationMs` hard cap lapses.
//
// - Chaining (not `setInterval`) is load-bearing: `/api/state`
//   responses on large transcripts can take multiple seconds to
//   serialize, and `setInterval` would stack overlapping inflight
//   requests against the backend. The chain only schedules the next
//   poll after the previous one's `fetchState` has resolved.
//
// - The hard cap is enforced in TWO ways that together close a race
//   in the previous setTimeout-ref-based design:
//
//   1. An absolute `deadlineMs` captured at `startActivePromptPoll`
//      time, checked both BEFORE arming the next timer and AFTER
//      `fetchState` resolves. The post-await check is what closes
//      the hole: `fetchState` can take long enough on large
//      transcripts that the cap lapses while a request is in flight
//      — the previous design's companion `setTimeout(clearRef, cap)`
//      would fire during the await, find the ref already cleared at
//      the callback's entry, and no-op. When the fetch eventually
//      resolved, `schedule()` would run again and arm a fresh timer
//      past the cap. The post-await deadline check makes that path
//      bail out explicitly rather than relying on the ref.
//
//   2. A belt-and-suspenders `setTimeout(cancel, maxDurationMs)` that
//      forcibly cancels any armed chained timer at the deadline. This
//      cannot stop an in-flight `fetchState()` await (once the
//      microtask is scheduled, it will run), but the post-await
//      deadline check covers that path.
//
// The scheduler uses injectable `now()` / `setTimer` / `clearTimer` /
// `onState` / `isMounted` / `fetchState` functions so tests can drive
// fake clocks and observe call counts without standing up a full
// React render. `App.tsx`'s existing fake-timer tests live alongside
// `ui/src/App.test.tsx`, but the scheduler's decision logic is
// self-contained enough to test in isolation — see
// `ui/src/active-prompt-poll.test.ts`.

export const ACTIVE_PROMPT_POLL_INTERVAL_MS = 30_000;
export const ACTIVE_PROMPT_POLL_MAX_DURATION_MS = 5 * 60 * 1000;

export interface ActivePromptPollHandlers<T> {
  /**
   * Fetches the authoritative state snapshot. The chain schedules the
   * next poll only after this promise resolves (or rejects), so a slow
   * response cannot stack overlapping inflight requests.
   */
  fetchState: () => Promise<T>;
  /**
   * Called with each successful `fetchState` result. Return `true` to
   * stop polling (e.g. when the target session is no longer Active).
   * Return `false` or `undefined` to chain another poll.
   */
  onState: (state: T) => boolean | void;
  /**
   * Called before every schedule + after every await to guard against
   * firing work after the component unmounted.
   */
  isMounted: () => boolean;
}

export interface ActivePromptPollOptions {
  intervalMs?: number;
  maxDurationMs?: number;
  /** Injectable clock for tests. Defaults to `Date.now`. */
  now?: () => number;
}

/**
 * Starts the self-chained safety-net poll. Returns a `cancel`
 * function that clears any armed chained timer AND the belt-and-
 * suspenders hard-cap timer. Calling `cancel` multiple times is
 * safe; it is a no-op after the first call.
 */
export function startActivePromptPoll<T>(
  handlers: ActivePromptPollHandlers<T>,
  options: ActivePromptPollOptions = {},
): () => void {
  const intervalMs = options.intervalMs ?? ACTIVE_PROMPT_POLL_INTERVAL_MS;
  const maxDurationMs =
    options.maxDurationMs ?? ACTIVE_PROMPT_POLL_MAX_DURATION_MS;
  const now = options.now ?? (() => Date.now());
  const deadlineMs = now() + maxDurationMs;

  let chainedTimerId: ReturnType<typeof setTimeout> | null = null;
  // Forward declaration — the hard-cap timer id is needed inside
  // `stopPoll()` below but is assigned after `schedule()` kicks
  // the chain off for the first time.
  let hardCapTimerId: ReturnType<typeof setTimeout> | null = null;
  let stopped = false;

  function clearChainedTimer() {
    if (chainedTimerId !== null) {
      clearTimeout(chainedTimerId);
      chainedTimerId = null;
    }
  }

  // Single stop path used by every self-stop site (chain exit via
  // `shouldStop`, deadline-lapsed check, external `cancel()`). Clears
  // BOTH the chained poll timer AND the belt-and-suspenders hard-cap
  // timer, and marks `stopped = true` so any in-flight callbacks
  // observe the stop on their next tick.
  //
  // Previously the hard-cap timer was only cleared by the external
  // cancel function — a self-stop via `shouldStop` (the common case,
  // when the session leaves "active") left `hardCapTimerId` scheduled
  // for the full `maxDurationMs`. The callback did no harm when it
  // eventually fired (it just set `stopped = true` again and cleared
  // an already-null chained timer), but it held a timer slot for up
  // to 5 minutes per completed prompt. See docs/bugs.md →
  // "`startActivePromptPoll` leaves hard-cap timer pending after
  // natural onState stop".
  function stopPoll() {
    stopped = true;
    clearChainedTimer();
    if (hardCapTimerId !== null) {
      clearTimeout(hardCapTimerId);
      hardCapTimerId = null;
    }
  }

  function schedule() {
    if (stopped || !handlers.isMounted()) {
      stopPoll();
      return;
    }
    if (now() >= deadlineMs) {
      stopPoll();
      return;
    }
    chainedTimerId = setTimeout(async () => {
      chainedTimerId = null;
      if (stopped || !handlers.isMounted()) {
        stopPoll();
        return;
      }
      try {
        const state = await handlers.fetchState();
        if (stopped || !handlers.isMounted()) {
          stopPoll();
          return;
        }
        // Post-await deadline re-check: `fetchState` on large
        // transcripts can take multiple seconds, and the cap may have
        // lapsed while we were awaiting. Bail out without calling
        // `onState` so we never produce work past the hard cap.
        if (now() >= deadlineMs) {
          stopPoll();
          return;
        }
        const shouldStop = handlers.onState(state);
        if (shouldStop) {
          stopPoll();
          return;
        }
      } catch {
        // Best-effort: swallow `fetchState` errors and fall through to
        // the next schedule. A real `/api/state` failure is either
        // transient (network blip) or will surface via the SSE
        // watchdog; the safety-net poll's job is to keep trying, not
        // to handle errors itself.
      }
      schedule();
    }, intervalMs);
  }

  schedule();

  // Belt-and-suspenders hard cap. This timer forcibly cancels the
  // chained poll at the deadline, independent of the schedule/await
  // deadline checks inside `schedule()`. It cannot stop an in-flight
  // `fetchState()` await once the microtask has been entered — that
  // path is explicitly guarded by the `now() >= deadlineMs` check
  // after `await handlers.fetchState()`.
  //
  // Guarded on `!stopped`: the synchronous first `schedule()` call
  // above can hit an early-return branch (`!isMounted()` or
  // `now() >= deadlineMs`) that calls `stopPoll()` BEFORE
  // `hardCapTimerId` is assigned — `stopPoll()` then sees
  // `hardCapTimerId === null` and cannot clear what doesn't yet
  // exist. Without this guard we'd arm a fresh hard-cap timer
  // immediately after `stopPoll()` set `stopped = true`, stranding
  // a no-op timer slot for the full `maxDurationMs` (the exact
  // symptom docs/bugs.md → "`startActivePromptPoll` leaves
  // hard-cap timer pending after natural onState stop" was fixed
  // to eliminate).
  if (!stopped) {
    hardCapTimerId = setTimeout(() => {
      stopPoll();
    }, maxDurationMs);
  }

  return () => {
    stopPoll();
  };
}
