/*
 * board-panel-polling — live read lifecycle for BoardPanel.
 *
 * Owns: complete generation-bound board reads, request-token/claim ownership,
 * background polling and backoff, progress-aware stale takeover, visibility
 * refreshes, abort cleanup, and change-highlight timing. Deliberately does
 * NOT own presentation, board writes, backend authorization, or the panel
 * mount point. Extracted from BoardPanel for tm-mn9 after the live viewer's
 * concurrency state machine outgrew the rendering module.
 *
 * Polling note: the repo's chained-poll convention (active-prompt-poll.ts)
 * exists to prevent setInterval stacking overlapping requests. This hook
 * deliberately uses setInterval + an owner-token coalescing guard instead:
 * ticks are skipped (not queued) while a request is pending, which the
 * guard-ownership tests pin. Divergence acknowledged per review 2026-07-27.
 */

import { useCallback, useEffect, useRef, useState } from "react";

import { ApiRequestError, fetchCoordinationBoard } from "../api";
import type { BoardEntry } from "../types";

/** Bounded restarts when a write lands between pages (409 snapshot conflict). */
const BOARD_PAGINATION_RESTART_LIMIT = 5;
/**
 * Background probes prioritize bounded work over an exhaustive live read. A
 * foreground Refresh keeps the larger retry budget and its actionable error.
 */
export const BOARD_BACKGROUND_PAGINATION_RESTART_LIMIT = 1;
/**
 * Live-poll cadence. Each idle tick costs one indexed generation probe that
 * returns `unchanged: true` with zero rows, so the panel stays live without
 * meaningful load (visibility ask, 2026-07-27).
 */
export const BOARD_LIVE_POLL_INTERVAL_MS = 8_000;
/** How long a freshly-changed entry keeps its highlight. */
export const BOARD_CHANGE_HIGHLIGHT_MS = 4_000;
/**
 * A claim older than this is presumed hung (the HTTP layer has no abort
 * timeout), and the next refresh may supersede it: the token bump keeps the
 * abandoned request from ever applying or releasing someone else's claim.
 * Without this, one never-settling request would silently kill live polling
 * AND turn manual Refresh into a permanent no-op (review 2026-07-27).
 */
export const BOARD_STALE_CLAIM_MS = 2 * BOARD_LIVE_POLL_INTERVAL_MS;
/**
 * Longest no-progress window granted to a replacement probe. Two minutes is
 * generous for a local request while still guaranteeing that repeated hangs
 * remain observable and recoverable on a human timescale.
 */
export const BOARD_STALE_CLAIM_MAX_MS = 8 * BOARD_STALE_CLAIM_MS;
/** First retry delay after a silent background-probe failure. */
export const BOARD_BACKGROUND_RETRY_BASE_MS = 2 * BOARD_LIVE_POLL_INTERVAL_MS;
/** Upper bound for repeated background-probe failure backoff. */
export const BOARD_BACKGROUND_RETRY_MAX_MS = 8 * BOARD_LIVE_POLL_INTERVAL_MS;
const BOARD_FOREGROUND_STALE_ERROR =
  "The board refresh stopped making progress. Automatic polling is retrying; use Refresh to try again now.";

export function boardBackgroundRetryDelay(failureCount: number): number {
  const boundedExponent = Math.min(Math.max(failureCount - 1, 0), 30);
  return Math.min(
    BOARD_BACKGROUND_RETRY_BASE_MS * 2 ** boundedExponent,
    BOARD_BACKGROUND_RETRY_MAX_MS,
  );
}

/**
 * Replacement probes get progressively longer to produce a first response.
 * The cap lets unusually slow finite responses complete without allowing a
 * long chain of truly hung fetches to suspend recovery for hours or longer.
 */
export function boardStaleClaimWindow(staleTakeoverCount: number): number {
  const boundedExponent = Math.min(Math.max(staleTakeoverCount, 0), 30);
  return Math.min(
    BOARD_STALE_CLAIM_MS * 2 ** boundedExponent,
    BOARD_STALE_CLAIM_MAX_MS,
  );
}

/** Monotonic clock for request ages and polling deadlines. */
function boardPollingNow(): number {
  return typeof performance === "undefined" ? Date.now() : performance.now();
}

function isSnapshotConflict(error: unknown): boolean {
  return error instanceof ApiRequestError && error.status === 409;
}

/**
 * Fetches every page of the board under one generation snapshot. Returns
 * `null` when the board is unchanged versus `knownGeneration` (the caller
 * keeps its rendered entries). Restarts the whole listing on a 409 snapshot
 * conflict, at most `BOARD_PAGINATION_RESTART_LIMIT` times.
 */
async function fetchWholeBoard(
  sessionId: string,
  knownGeneration: number | null,
  restartLimit: number = BOARD_PAGINATION_RESTART_LIMIT,
  signal?: AbortSignal,
  onProgress?: () => void,
): Promise<{ entries: BoardEntry[]; generation: number } | null> {
  let restarts = 0;
  for (;;) {
    const first = await fetchCoordinationBoard(sessionId, {
      knownGeneration: knownGeneration ?? undefined,
      signal,
    });
    onProgress?.();
    if (first.unchanged) {
      return null;
    }
    const entries = [...first.entries];
    let afterKey = first.nextAfterKey ?? null;
    let restarted = false;
    while (afterKey !== null) {
      let page;
      try {
        page = await fetchCoordinationBoard(sessionId, {
          afterKey,
          snapshotGeneration: first.generation,
          signal,
        });
        onProgress?.();
      } catch (error) {
        // A conflict is still a completed backend response and therefore proof
        // that the request is progressing rather than hung.
        onProgress?.();
        if (isSnapshotConflict(error)) {
          if (restarts < restartLimit) {
            restarts += 1;
            restarted = true;
            break;
          }
          throw new Error(
            "The board kept changing while it was read. Refresh again when writes settle.",
          );
        }
        throw error;
      }
      entries.push(...page.entries);
      afterKey = page.nextAfterKey ?? null;
    }
    if (restarted) {
      continue;
    }
    return { entries, generation: first.generation };
  }
}

type RefreshOptions = {
  background?: boolean;
  bypassBackoff?: boolean;
  supersedeBackground?: boolean;
};

export type BoardPollingState = {
  visibleEntries: BoardEntry[];
  visibleGeneration: number | null;
  visibleError: string | null;
  visibleBackgroundError: string | null;
  visibleLoading: boolean;
  requestActive: boolean;
  highlightAboveGeneration: number | null;
  visibleDeletionObservedAt: string | null;
  refresh: () => Promise<void>;
};

export function useBoardPolling(sessionId: string): BoardPollingState {
  const [entries, setEntries] = useState<BoardEntry[]>([]);
  const [generation, setGeneration] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Automatic probes fail soft: keep the last good facts visible and degrade
  // the live chip to "stale" instead of raising an assertive alert.
  const [backgroundError, setBackgroundError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  // Entries whose updatedAtGeneration is ABOVE this threshold changed in the
  // most recent observed write burst and render highlighted until the
  // highlight timer clears the threshold.
  const [highlightAboveGeneration, setHighlightAboveGeneration] = useState<
    number | null
  >(null);
  // The list API has no timestamp for a deleted row. When a newer generation
  // removes entries without updating any surviving fact, preserve the wall
  // time at which that deletion-only generation was observed so the header
  // does not keep advertising the prior visible write as the latest change.
  const [deletionObservedAt, setDeletionObservedAt] = useState<string | null>(
    null,
  );
  // Bumped once per completed probe or skipped/coalesced poll tick: the
  // guaranteed re-render that keeps relative timestamps aging on an idle or
  // slow-reading board.
  const [, setProbeCount] = useState(0);
  // Reactive mirror of the in-flight claim so the Refresh button can
  // acknowledge coalesced clicks.
  const [requestActive, setRequestActive] = useState(false);
  // The latest generation we fully rendered; passed back as knownGeneration
  // so an unchanged board costs one cheap short-circuit round trip.
  const renderedGeneration = useRef<number | null>(null);
  // State updates from the session-change effect run after React's first
  // render for the new prop. Keep the owner alongside the rendered payload so
  // that first render cannot display the previous project's facts.
  const renderedSessionId = useRef<string | null>(null);
  // Only the newest in-flight refresh may apply state.
  const requestToken = useRef(0);
  // The token owns both application and release. A superseded request settling
  // late cannot release the current request's claim.
  const inFlightToken = useRef<number | null>(null);
  // Monotonic time the current claim last made network progress.
  const inFlightSince = useRef(0);
  const inFlightBackground = useRef<boolean | null>(null);
  const inFlightAbortController = useRef<AbortController | null>(null);
  // Background freshness state has its own session owner. Snapshot ownership
  // cannot serve this purpose: a hung first load has no rendered snapshot.
  const backgroundErrorSessionId = useRef<string | null>(null);
  // Network failure backoff and stale-takeover aging are deliberately separate:
  // a fast terminal error must not grant a later hung request a huge window.
  const backgroundFailureCount = useRef(0);
  const backgroundStaleTakeoverCount = useRef(0);
  const nextBackgroundPollAt = useRef(0);
  const highlightTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const refresh = useCallback(
    async (options?: RefreshOptions) => {
      const background = options?.background ?? false;
      const bypassBackoff = options?.bypassBackoff ?? false;
      const supersedeBackground = options?.supersedeBackground ?? false;
      if (inFlightToken.current !== null) {
        const currentIsBackground = inFlightBackground.current === true;
        const manualSupersedesBackground = !background && currentIsBackground;
        const visibilitySupersedesBackground =
          background && supersedeBackground && currentIsBackground;
        const staleClaimWindow = currentIsBackground
          ? boardStaleClaimWindow(backgroundStaleTakeoverCount.current)
          : BOARD_STALE_CLAIM_MS;
        const staleClaimExpired =
          boardPollingNow() - inFlightSince.current >= staleClaimWindow;
        if (
          !manualSupersedesBackground &&
          !visibilitySupersedesBackground &&
          !staleClaimExpired
        ) {
          if (!background) {
            setRequestActive(true);
          } else {
            setProbeCount((count) => count + 1);
          }
          return;
        }

        // Keep token supersession in the same synchronous turn as abort: the
        // shared request helper wraps AbortError as a transport error, so the
        // bumped token below guarantees that rejection is discarded.
        inFlightAbortController.current?.abort();
        if (!currentIsBackground) {
          setLoading(false);
          renderedSessionId.current = sessionId;
          setError(BOARD_FOREGROUND_STALE_ERROR);
        }
        if (
          background &&
          !visibilitySupersedesBackground &&
          staleClaimExpired
        ) {
          const staleTakeoverCount = backgroundStaleTakeoverCount.current + 1;
          backgroundStaleTakeoverCount.current = staleTakeoverCount;
          const failureCount = backgroundFailureCount.current + 1;
          backgroundFailureCount.current = failureCount;
          nextBackgroundPollAt.current =
            boardPollingNow() + boardBackgroundRetryDelay(failureCount);
          backgroundErrorSessionId.current = sessionId;
          setBackgroundError("Live board polling stopped making progress.");
          setProbeCount((count) => count + 1);
        }
      } else if (
        background &&
        !bypassBackoff &&
        boardPollingNow() < nextBackgroundPollAt.current
      ) {
        // Backoff governs admission of idle probes only. An active claim must
        // always be inspected on cadence so its no-progress deadline cannot
        // be hidden behind a longer retry delay.
        setProbeCount((count) => count + 1);
        return;
      }

      if (!background || supersedeBackground) {
        backgroundStaleTakeoverCount.current = 0;
      }
      const token = requestToken.current + 1;
      const abortController = new AbortController();
      requestToken.current = token;
      inFlightToken.current = token;
      inFlightSince.current = boardPollingNow();
      inFlightBackground.current = background;
      inFlightAbortController.current = abortController;
      setRequestActive(!background);
      if (!background) {
        setLoading(true);
        setError(null);
      }
      try {
        const previousGeneration = renderedGeneration.current;
        const board = await fetchWholeBoard(
          sessionId,
          previousGeneration,
          background
            ? BOARD_BACKGROUND_PAGINATION_RESTART_LIMIT
            : BOARD_PAGINATION_RESTART_LIMIT,
          abortController.signal,
          () => {
            if (inFlightToken.current === token) {
              inFlightSince.current = boardPollingNow();
            }
          },
        );
        if (token !== requestToken.current) {
          return;
        }
        if (board !== null) {
          renderedGeneration.current = board.generation;
          renderedSessionId.current = sessionId;
          setEntries(board.entries);
          setGeneration(board.generation);
          if (
            previousGeneration !== null &&
            board.generation > previousGeneration
          ) {
            const hasVisibleWrite = board.entries.some(
              (entry) => entry.updatedAtGeneration > previousGeneration,
            );
            setDeletionObservedAt(
              hasVisibleWrite ? null : new Date().toISOString(),
            );
            setHighlightAboveGeneration(previousGeneration);
            if (highlightTimer.current !== null) {
              clearTimeout(highlightTimer.current);
            }
            highlightTimer.current = setTimeout(() => {
              setHighlightAboveGeneration(null);
              highlightTimer.current = null;
            }, BOARD_CHANGE_HIGHLIGHT_MS);
          }
        } else {
          renderedSessionId.current = sessionId;
        }
        setError(null);
        setBackgroundError(null);
        backgroundErrorSessionId.current = null;
        backgroundFailureCount.current = 0;
        backgroundStaleTakeoverCount.current = 0;
        nextBackgroundPollAt.current = 0;
        setLoading(false);
        setProbeCount((count) => count + 1);
      } catch (fetchError) {
        if (token !== requestToken.current) {
          return;
        }
        // A terminal response proves this claim was not hung. Only subsequent
        // no-progress takeovers should lengthen the next stale window.
        backgroundStaleTakeoverCount.current = 0;
        const message =
          fetchError instanceof Error ? fetchError.message : String(fetchError);
        if (background) {
          const hasLastGoodSnapshot =
            renderedSessionId.current === sessionId &&
            renderedGeneration.current !== null;
          const failureCount = backgroundFailureCount.current + 1;
          backgroundFailureCount.current = failureCount;
          nextBackgroundPollAt.current =
            boardPollingNow() + boardBackgroundRetryDelay(failureCount);
          backgroundErrorSessionId.current = sessionId;
          setBackgroundError(message);
          setProbeCount((count) => count + 1);
          if (!hasLastGoodSnapshot) {
            renderedSessionId.current = sessionId;
            setError(message);
          }
        } else {
          renderedSessionId.current = sessionId;
          setError(message);
        }
      } finally {
        if (inFlightToken.current === token) {
          inFlightToken.current = null;
          inFlightBackground.current = null;
          inFlightAbortController.current = null;
          setRequestActive(false);
        }
        if (!background && token === requestToken.current) {
          setLoading(false);
        }
      }
    },
    [sessionId],
  );

  useEffect(() => {
    renderedGeneration.current = null;
    setEntries([]);
    setGeneration(null);
    setBackgroundError(null);
    backgroundErrorSessionId.current = null;
    setHighlightAboveGeneration(null);
    setDeletionObservedAt(null);
    backgroundFailureCount.current = 0;
    backgroundStaleTakeoverCount.current = 0;
    nextBackgroundPollAt.current = 0;
    if (highlightTimer.current !== null) {
      clearTimeout(highlightTimer.current);
      highlightTimer.current = null;
    }
    inFlightAbortController.current?.abort();
    inFlightToken.current = null;
    inFlightBackground.current = null;
    inFlightAbortController.current = null;
    void refresh();
    return () => {
      requestToken.current += 1;
      inFlightAbortController.current?.abort();
      inFlightToken.current = null;
      inFlightBackground.current = null;
      inFlightAbortController.current = null;
    };
  }, [refresh]);

  useEffect(() => {
    const interval = setInterval(() => {
      if (typeof document !== "undefined" && document.hidden) {
        return;
      }
      void refresh({ background: true });
    }, BOARD_LIVE_POLL_INTERVAL_MS);
    return () => {
      clearInterval(interval);
    };
  }, [refresh]);

  useEffect(() => {
    if (typeof document === "undefined") {
      return;
    }
    const onVisibilityChange = () => {
      if (!document.hidden) {
        void refresh({
          background: true,
          bypassBackoff: true,
          supersedeBackground: true,
        });
      }
    };
    document.addEventListener("visibilitychange", onVisibilityChange);
    return () => {
      document.removeEventListener("visibilitychange", onVisibilityChange);
    };
  }, [refresh]);

  useEffect(() => {
    return () => {
      if (highlightTimer.current !== null) {
        clearTimeout(highlightTimer.current);
      }
    };
  }, []);

  const scopeMatches = renderedSessionId.current === sessionId;
  return {
    visibleEntries: scopeMatches ? entries : [],
    visibleGeneration: scopeMatches ? generation : null,
    visibleError: scopeMatches ? error : null,
    visibleBackgroundError:
      backgroundErrorSessionId.current === sessionId ? backgroundError : null,
    visibleLoading: loading || !scopeMatches,
    requestActive,
    highlightAboveGeneration,
    visibleDeletionObservedAt: scopeMatches ? deletionObservedAt : null,
    refresh,
  };
}
