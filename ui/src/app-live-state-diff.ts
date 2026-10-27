// app-live-state-diff.ts
//
// Owns: the `stateDiff` SSE event — its wire type, its admission against the
// client's diff baseline (the revision of the last complete adoption of a
// server-marked full state, or of the last applied diff), the pure merges
// that fold a diff into the current sessions and keyed rows, and the
// transport handler that admits and applies one diff, or ignores it and asks
// the server once for a marked full state to align on.
//
// Does not own: writing adopted slices into React state and refs
// (`adoptStateDiff` in app-live-state.ts, beside `adoptState`), the baseline
// writers on full and partial adoptions, EventSource wiring
// (app-live-state-transport.ts), or the server producer
// (src/state_broadcast_diff.rs).
//
// New module, not split from an existing file. It sits beside
// app-live-state-transport-events.ts, which owns the `state` and `delta`
// handlers it mirrors, so that file does not grow.

import type { MutableRefObject } from "react";
import type { DelegationStateSummary, DelegationWaitRecord, StateResponse } from "./api";
import { describeBackendConnectionIssueDetail } from "./backend-connection";
import { pruneLiveTransportActivitySessions } from "./live-updates";
import { isServerInstanceMismatch } from "./state-revision";
import type { SessionHydrationOptions } from "./app-live-state-hydration";
import type { ReconnectRecoveryStateSnapshot } from "./app-live-state-reconnect-state";
import type { RequestStateResyncOptions } from "./app-live-state-resync-options";
import type { SessionReadRef } from "./transcript-repair-authority";
import type { TestRunSummary } from "./test-runs";
import type { TestRunWaitRecord } from "./test-run-waits";
import type { Session, StateSessionSummary } from "./types";

/** Every header slice is absolute in every diff, with its `StateResponse` shape. */
export type StateDiffHeader = Pick<
  StateResponse,
  "codex" | "agentReadiness" | "preferences" | "projects" | "orchestrators" | "workspaces"
> & {
  delegationWaits: DelegationWaitRecord[];
  testRunWaits: TestRunWaitRecord[];
  pendingEngramMcpRevocationSessionIds: string[];
};

export type StateDiffRows<T> = { changed: T[]; removed: string[] };

/** A cumulative diff from the server's cut `baseRevision` to `revision`. */
export type StateDiffEvent = {
  baseRevision: number;
  revision: number;
  serverInstanceId: string;
  header: StateDiffHeader;
  sessions: StateDiffRows<StateSessionSummary> & {
    /** Present only when membership or order changed against the base. */
    order?: string[];
  };
  delegations: StateDiffRows<DelegationStateSummary>;
  testRuns: StateDiffRows<TestRunSummary>;
};

/**
 * - `apply`: the diff builds on this client's baseline and is newer.
 * - `ignore`: nothing to do (a retired server, or no newer than the baseline).
 * - `align`: the diff cannot build on this client; ignore it and ask the
 *   server for a marked full state instead of an HTTP resync.
 * - `resync`: an unknown server instance, adoptable only from a full state.
 */
export type StateDiffAdmission = "apply" | "ignore" | "align" | "resync";

export function decideStateDiffAdmission(input: {
  /** The `stateDiff` baseline revision, or null when there is none. */
  baselineRevision: number | null;
  currentRevision: number | null;
  baseRevision: number;
  revision: number;
  lastSeenServerInstanceId: string | null;
  serverInstanceId: string;
  seenServerInstanceIds: ReadonlySet<string>;
}): StateDiffAdmission {
  if (isServerInstanceMismatch(input.lastSeenServerInstanceId, input.serverInstanceId)) {
    // A retired server's late event changes nothing. An unknown instance can
    // only be adopted from a full snapshot, never patched onto this one.
    return input.seenServerInstanceIds.has(input.serverInstanceId) ? "ignore" : "resync";
  }
  const baseline = input.baselineRevision;
  if (baseline !== null && input.revision <= baseline) return "ignore";
  if (
    baseline !== null &&
    baseline === input.baseRevision &&
    (input.currentRevision === null || input.revision > input.currentRevision)
  ) {
    return "apply";
  }
  // No baseline, another base, or a diff behind state this client already
  // took from elsewhere: only a marked full state realigns it.
  return "align";
}

/**
 * Folds a diff into an id-keyed list. Changed entries replace or insert by id
 * and removed ids leave. With `order` the result follows it; without, current
 * order is kept and new ids append in diff order. Ids no entry exists for are
 * skipped rather than invented. Returns `previous` itself when nothing moved.
 */
export function mergeStateDiffSessions<S extends { id: string }>(
  previous: readonly S[],
  changed: readonly S[],
  removedIds: readonly string[],
  order?: readonly string[],
): S[] {
  const removed = new Set(removedIds);
  const byId = new Map(previous.map((session) => [session.id, session]));
  for (const session of changed) byId.set(session.id, session);
  let ids: readonly string[];
  if (order) {
    ids = order;
  } else {
    const kept = previous.map((session) => session.id);
    const listed = new Set(kept);
    ids = [...kept, ...changed.map((session) => session.id).filter((id) => !listed.has(id))];
  }
  const merged: S[] = [];
  const placed = new Set<string>();
  for (const id of ids) {
    const session = byId.get(id);
    if (removed.has(id) || placed.has(id) || !session) continue;
    placed.add(id);
    merged.push(session);
  }
  // A header-only diff (or one whose rows reconcile to the same objects)
  // keeps the list identity, so adopters can skip their unchanged path.
  return merged.length === previous.length &&
    merged.every((session, index) => session === previous[index])
    ? (previous as S[])
    : merged;
}

/** Keyed merge for delegation and test-run rows: absolute rows, removals. */
export function mergeStateDiffRows<T>(
  previous: readonly T[],
  diff: StateDiffRows<T>,
  key: (row: T) => string,
): T[] {
  const removed = new Set(diff.removed);
  const changed = new Map(diff.changed.map((row) => [key(row), row]));
  const merged = previous
    .filter((row) => !removed.has(key(row)))
    .map((row) => changed.get(key(row)) ?? row);
  const present = new Set(merged.map(key));
  for (const row of diff.changed) {
    if (!present.has(key(row))) merged.push(row);
  }
  return merged;
}

export type StateDiffEventHandlerContext = {
  adoptStateDiff: (diff: StateDiffEvent) => boolean;
  beginBadLiveEventRecovery: () => void;
  cancelStaleSendResponseRecoveryPollForSessions: (sessionIds: Iterable<string>) => void;
  clearInitialStateResyncRetryTimeout: () => void;
  clearReconnectStateResyncTimeoutAfterConfirmedReopen: () => void;
  clearRecoveredBackendRequestError: () => void;
  confirmReconnectRecoveryFromLiveEvent: () => boolean;
  confirmReconnectRecoveryFromStateEvent: () => boolean;
  isCancelled: () => boolean;
  lastLiveTransportActivityAtBySessionId: Map<string, number>;
  lastSeenServerInstanceIdRef: MutableRefObject<string | null>;
  latestStateRevisionRef: MutableRefObject<number | null>;
  requestStateResync: (options?: RequestStateResyncOptions) => void;
  /** `POST /api/events/full-state`. */
  requestStateDiffAlignment: () => Promise<unknown>;
  seenServerInstanceIdsRef: MutableRefObject<Set<string>>;
  sessionsRef: SessionReadRef;
  setBackendConnectionIssueDetail: (detail: string | null) => void;
  setIsLoading: (loading: boolean) => void;
  startSessionHydration: (sessionId: string, options?: SessionHydrationOptions) => void;
  stateDiffBaselineRevisionRef: MutableRefObject<number | null>;
  /**
   * An alignment request is outstanding. Cleared when a marked full state
   * arrives (handleStateEvent) or when the request fails.
   */
  stateDiffAlignmentPendingRef: MutableRefObject<boolean>;
  syncLiveSessionResumeWatchdogBaselines: (sessions: readonly Session[], now?: number) => void;
  syncLiveTransportActivityFromState: (sessions: readonly Session[], now?: number) => void;
  transportState: ReconnectRecoveryStateSnapshot;
};

/**
 * Handles one `stateDiff` event. It never reads or clears the lagged
 * force-adopt flag: loss recovery always arrives as a full `state`.
 */
export function createStateDiffEventHandler(context: StateDiffEventHandlerContext) {
  function requestAlignmentOnce() {
    if (context.stateDiffAlignmentPendingRef.current) return;
    context.stateDiffAlignmentPendingRef.current = true;
    // A failed request lets a later diff ask again, and recovers now over
    // HTTP: on an idle host no later diff may come.
    const failed = () => {
      context.stateDiffAlignmentPendingRef.current = false;
      if (!context.isCancelled()) {
        context.requestStateResync({ rearmOnFailure: true });
      }
    };
    let request: Promise<unknown>;
    try {
      request = context.requestStateDiffAlignment();
    } catch {
      failed();
      return;
    }
    void request.catch(failed);
  }

  return function handleStateDiffEvent(event: MessageEvent<string>) {
    if (context.isCancelled()) return;
    try {
      const diff = JSON.parse(event.data) as StateDiffEvent;
      const admission = decideStateDiffAdmission({
        baselineRevision: context.stateDiffBaselineRevisionRef.current,
        currentRevision: context.latestStateRevisionRef.current,
        baseRevision: diff.baseRevision,
        revision: diff.revision,
        lastSeenServerInstanceId: context.lastSeenServerInstanceIdRef.current,
        serverInstanceId: diff.serverInstanceId,
        seenServerInstanceIds: context.seenServerInstanceIdsRef.current,
      });
      if (admission === "align") {
        // Not an HTTP resync: the server's next cut, sent as a marked full
        // state, carries everything this diff would have and becomes the
        // baseline. Reconnect recovery stays as it is until then.
        requestAlignmentOnce();
        return;
      }
      if (admission === "resync") {
        // An unknown server instance is adoptable only from a full state.
        context.requestStateResync({ rearmOnFailure: true });
        return;
      }
      if (admission === "ignore") {
        if (!context.transportState.pendingBadLiveEventRecovery) {
          void context.confirmReconnectRecoveryFromLiveEvent();
        }
        context.setBackendConnectionIssueDetail(null);
        context.clearRecoveredBackendRequestError();
        return;
      }
      const previousById = new Map(
        context.sessionsRef.current.map((session) => [session.id, session]),
      );
      if (!context.adoptStateDiff(diff)) return;
      void context.confirmReconnectRecoveryFromStateEvent();
      for (const summary of diff.sessions.changed) {
        // Same queue-body repair a full adoption starts: summaries omit
        // pending prompt bodies, so a changed queue identity needs a read.
        const previous = previousById.get(summary.id);
        const mutationAdvanced =
          previous != null &&
          summary.sessionMutationStamp != null &&
          previous.sessionMutationStamp !== summary.sessionMutationStamp;
        const queueProjectionChanged =
          summary.queueProjectionHash != null
            ? previous?.queueProjectionHash !== summary.queueProjectionHash
            : (previous?.pendingPrompts?.length ?? 0) > 0 ||
              previous?.queuePaused !== summary.queuePaused;
        if (mutationAdvanced && queueProjectionChanged) {
          context.startSessionHydration(summary.id, { queueAfterCurrent: true });
        }
      }
      const sessions = context.sessionsRef.current;
      context.cancelStaleSendResponseRecoveryPollForSessions(
        sessions.map((session) => session.id),
      );
      context.clearInitialStateResyncRetryTimeout();
      const adoptedAt = Date.now();
      context.clearReconnectStateResyncTimeoutAfterConfirmedReopen();
      // As for an adopted full state: every known session's transport is live.
      context.syncLiveTransportActivityFromState(sessions, adoptedAt);
      pruneLiveTransportActivitySessions(context.lastLiveTransportActivityAtBySessionId, sessions);
      context.syncLiveSessionResumeWatchdogBaselines(sessions, adoptedAt);
      context.setBackendConnectionIssueDetail(null);
      context.clearRecoveredBackendRequestError();
    } catch (error) {
      if (context.isCancelled()) return;
      // A malformed or failed diff leaves no baseline; recover from a full state.
      context.stateDiffBaselineRevisionRef.current = null;
      context.setBackendConnectionIssueDetail(describeBackendConnectionIssueDetail(error));
      if (context.transportState.sawReconnectOpenSinceLastError) {
        context.beginBadLiveEventRecovery();
      } else {
        context.requestStateResync({ rearmOnFailure: true });
      }
    } finally {
      if (!context.isCancelled()) {
        context.setIsLoading(context.latestStateRevisionRef.current === null);
      }
    }
  };
}
