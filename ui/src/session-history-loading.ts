// Owns bounded session-history request admission, fetch/merge dispatch, and
// completion. Passive hydration and explicit navigation share the same
// in-flight older-page promise for one session/cursor boundary.
//
// Does not own React hydration scheduling, recent-tail/text repair, session
// state publication, or action-recovery policy. Callers inject those through
// a small context so this module stays independent of app-live-state.ts.
//
// Post-fetch unmounts fail silently: a dead hook can neither publish the page
// nor consume a recovery resync. Completable demand still settles in `finally`.
//
// Split from: ui/src/app-live-state.ts.

import { ApiRequestError, fetchSessionHistory } from "./api";
import { SESSION_HISTORY_PAGE_MESSAGE_COUNT } from "./session-tail-policy";
import {
  appendSessionHistoryPage,
  prependSessionHistoryPage,
  replaceSessionWithHistoryAroundPage,
  replaceSessionWithHistoryStartPage,
  replaceSessionWithHistoryTailPage,
  type SessionHistoryMergeOutcome,
} from "./session-history";
import {
  completeSessionHistoryPageDemand,
  type SessionHistoryPageDemand,
} from "./session-history-demand";
import { isServerInstanceMismatch } from "./state-revision";
import type { Session } from "./types";

export type OlderHistoryLoadResult =
  | { kind: "applied"; session: Session }
  | { kind: "failed" }
  | { kind: "unavailable" };

export type OlderHistoryLoadRegistry = Map<
  string,
  Promise<OlderHistoryLoadResult>
>;

export type SessionHistoryLoadingContext = {
  getLastSeenServerInstanceId: () => string | null;
  getSession: (sessionId: string) => Session | undefined;
  inFlightOlderLoads: OlderHistoryLoadRegistry;
  isMounted: () => boolean;
  publishSession: (session: Session) => boolean;
  reportRequestError: (error: unknown) => void;
  requestActionRecoveryResync: (options?: {
    allowUnknownServerInstance?: boolean;
  }) => void;
};

function applyHistoryMergeOutcome(
  outcome: SessionHistoryMergeOutcome,
  context: SessionHistoryLoadingContext,
) {
  switch (outcome.kind) {
    case "applied":
      return context.publishSession(outcome.session);
    case "cursorChanged":
    case "metadataChanged":
      context.requestActionRecoveryResync();
      return false;
    case "protocolError":
      throw new Error(outcome.message);
    default: {
      const _exhaustive: never = outcome;
      void _exhaustive;
      return false;
    }
  }
}

export function loadOlderHistoryPageOnce({
  context,
  requestedBefore,
  sessionId,
}: {
  context: SessionHistoryLoadingContext;
  requestedBefore: string;
  sessionId: string;
}): Promise<OlderHistoryLoadResult> {
  // The exact promise owns publication and removes only itself. Clearing the
  // registry during unmount/instance change is therefore safe, and every
  // terminal path permits a later retry for the same exclusive boundary.
  const key = `${sessionId}:${requestedBefore}`;
  const existing = context.inFlightOlderLoads.get(key);
  if (existing) {
    return existing;
  }

  const load = (async (): Promise<OlderHistoryLoadResult> => {
    try {
      const historyPage = await fetchSessionHistory(sessionId, {
        before: requestedBefore,
        limit: SESSION_HISTORY_PAGE_MESSAGE_COUNT,
      });
      if (!context.isMounted()) {
        return { kind: "unavailable" };
      }
      if (
        isServerInstanceMismatch(
          context.getLastSeenServerInstanceId(),
          historyPage.serverInstanceId,
        )
      ) {
        context.requestActionRecoveryResync({
          allowUnknownServerInstance: true,
        });
        return { kind: "unavailable" };
      }

      const currentSession = context.getSession(sessionId);
      if (!currentSession) {
        return { kind: "unavailable" };
      }
      const mergeOutcome = prependSessionHistoryPage({
        current: currentSession,
        page: historyPage,
        requestedBefore,
      });
      if (mergeOutcome.kind !== "applied") {
        applyHistoryMergeOutcome(mergeOutcome, context);
        return { kind: "unavailable" };
      }
      return context.publishSession(mergeOutcome.session)
        ? { kind: "applied", session: mergeOutcome.session }
        : { kind: "unavailable" };
    } catch (error) {
      if (!context.isMounted()) {
        return { kind: "unavailable" };
      }
      // Session disappearance and conflict are benign hydration races. A
      // shared request keeps the established silent action-recovery posture
      // for both passive and explicit waiters.
      if (
        error instanceof ApiRequestError &&
        (error.status === 404 || error.status === 409)
      ) {
        context.requestActionRecoveryResync();
        return { kind: "unavailable" };
      }
      context.reportRequestError(error);
      return { kind: "failed" };
    }
  })().finally(() => {
    if (context.inFlightOlderLoads.get(key) === load) {
      context.inFlightOlderLoads.delete(key);
    }
  });
  context.inFlightOlderLoads.set(key, load);
  return load;
}

export async function loadBoundedSessionHistoryWindow({
  context,
  demand,
}: {
  context: SessionHistoryLoadingContext;
  demand: SessionHistoryPageDemand;
}) {
  const { direction, requestId, sessionId } = demand;
  let applied = false;
  try {
    if (!context.isMounted()) {
      return;
    }
    const requestedSession = context.getSession(sessionId);
    if (!requestedSession) {
      return;
    }
    const requestedAfter =
      direction === "newer"
        ? (requestedSession.messages[requestedSession.messages.length - 1]
            ?.id ?? null)
        : null;
    const requestedBefore =
      direction === "older"
        ? (requestedSession.messages[0]?.id ?? null)
        : null;
    if (direction === "newer" && !requestedAfter) {
      return;
    }
    if (direction === "older" && !requestedBefore) {
      return;
    }
    if (direction === "older") {
      const result = await loadOlderHistoryPageOnce({
        context,
        requestedBefore: requestedBefore!,
        sessionId,
      });
      applied = result.kind === "applied";
      return;
    }

    let historyRequest:
      | { from: "start" }
      | { after: string }
      | { around: number }
      | Record<string, never>;
    switch (direction) {
      case "start":
        historyRequest = { from: "start" };
        break;
      case "newer":
        historyRequest = { after: requestedAfter! };
        break;
      case "around":
        historyRequest = { around: demand.position ?? 0 };
        break;
      case "tail":
        historyRequest = {};
        break;
      default: {
        const _exhaustive: never = direction;
        void _exhaustive;
        return;
      }
    }
    const historyPage = await fetchSessionHistory(sessionId, {
      ...historyRequest,
      limit: SESSION_HISTORY_PAGE_MESSAGE_COUNT,
    });
    if (!context.isMounted()) {
      // Do not schedule recovery work owned by a hook that has already gone
      // away. The `finally` block remains responsible for settling demand.
      return;
    }
    if (
      isServerInstanceMismatch(
        context.getLastSeenServerInstanceId(),
        historyPage.serverInstanceId,
      )
    ) {
      context.requestActionRecoveryResync({
        allowUnknownServerInstance: true,
      });
      return;
    }
    const currentSession = context.getSession(sessionId);
    if (!currentSession) {
      return;
    }
    let mergeOutcome: SessionHistoryMergeOutcome;
    switch (direction) {
      case "start":
        mergeOutcome = replaceSessionWithHistoryStartPage({
          current: currentSession,
          page: historyPage,
        });
        break;
      case "newer":
        mergeOutcome = appendSessionHistoryPage({
          current: currentSession,
          page: historyPage,
          requestedAfter: requestedAfter!,
        });
        break;
      case "around":
        mergeOutcome = replaceSessionWithHistoryAroundPage({
          current: currentSession,
          page: historyPage,
          requestedPosition: demand.position ?? 0,
        });
        break;
      case "tail":
        mergeOutcome = replaceSessionWithHistoryTailPage({
          current: currentSession,
          page: historyPage,
        });
        break;
      default: {
        const _exhaustive: never = direction;
        void _exhaustive;
        return;
      }
    }
    applied = applyHistoryMergeOutcome(mergeOutcome, context);
  } catch (error) {
    if (!context.isMounted()) {
      return;
    }
    // Non-older directions are explicit navigation requests, so preserve
    // their existing user-visible error posture. The shared older loader also
    // serves passive hydration and separately treats 404/409 as benign races.
    context.reportRequestError(error);
  } finally {
    completeSessionHistoryPageDemand(requestId, applied);
  }
}
