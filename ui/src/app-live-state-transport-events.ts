// app-live-state-transport-events.ts
//
// Owns: EventSource event handlers for app-live-state-transport.
// The surrounding transport hook owns timers, reconnect state flags,
// EventSource setup/teardown, and the state-resync loop.
//
// Split out of: ui/src/app-live-state-transport.ts.

import {
  startTransition,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import type { StateResponse } from "./api";
import {
  applyDeltaToSessions,
  pruneLiveTransportActivitySessions,
  sessionDeltaAdvancesCurrentMutationStamp,
} from "./live-updates";
import {
  decideDeltaRevisionAction,
  shouldAdoptSnapshotRevision,
} from "./state-revision";
import {
  isDelegationDeltaEvent,
  isSameRevisionReplayableSessionDelta,
  isSessionDeltaEvent,
  staleSendRecoveryPollSessionIdsForDelta,
} from "./app-live-state-delta-events";
import { mergeOrchestratorDeltaSessions } from "./control-surface-state";
import {
  createStateEventProfiler,
  extractTopLevelJsonNumber,
  extractTopLevelJsonString,
  payloadHasTopLevelTrueBoolean,
} from "./app-live-state-event-utils";
import type { StateEventPayload } from "./app-shell-internals";
import type {
  CodexState,
  DeltaEvent,
  OrchestratorInstance,
  Session,
  WorkspaceFilesChangedEvent,
} from "./types";
import {
  describeBackendConnectionIssueDetail,
  type BackendConnectionState,
} from "./backend-connection";
import type { AdoptStateOptions } from "./app-live-state-types";
import type { RequestStateResyncOptions } from "./app-live-state-resync-options";

type SessionHydrationOptions = {
  allowDivergentTextRepairAfterNewerRevision?: boolean;
  queueAfterCurrent?: boolean;
};

type TransportRecoveryStateSnapshot = {
  readonly delegationRepairAdoptedSinceLastReconnectError: boolean;
  readonly pendingBadLiveEventRecovery: boolean;
  readonly sawReconnectOpenSinceLastError: boolean;
};

type AppLiveStateTransportEventHandlersContext = {
  adoptState: (state: StateResponse, options?: AdoptStateOptions) => boolean;
  applyDelegationWaitDeltaLocally: (delta: DeltaEvent) => void;
  beginBadLiveEventRecovery: () => void;
  cancelStaleSendResponseRecoveryPollForSessions: (
    sessionIds: Iterable<string>,
  ) => void;
  clearForceAdoptNextStateEvent: () => void;
  clearInitialStateResyncRetryTimeout: () => void;
  clearRecoveredBackendRequestError: () => void;
  clearReconnectStateResyncTimeoutAfterConfirmedReopen: () => void;
  codexStateRef: MutableRefObject<CodexState>;
  confirmReconnectRecoveryFromAuthoritativeSnapshot: () => void;
  confirmReconnectRecoveryFromDeltaEvent: () => boolean;
  confirmReconnectRecoveryFromLiveEvent: () => boolean;
  confirmReconnectRecoveryFromStateEvent: () => boolean;
  enqueueWorkspaceFilesChangedEvent: (
    filesChanged: WorkspaceFilesChangedEvent,
  ) => void;
  forceAdoptNextStateEventRef: MutableRefObject<boolean>;
  isCancelled: () => boolean;
  laggedRecoveryBaselineRevisionRef: MutableRefObject<number | null>;
  lastSeenServerInstanceIdRef: MutableRefObject<string | null>;
  lastLiveTransportActivityAtBySessionId: Map<string, number>;
  latestStateRevisionRef: MutableRefObject<number | null>;
  markLiveSessionResumeWatchdogBaseline: (
    sessionIds: Iterable<string>,
    now?: number,
  ) => void;
  markLiveTransportActivity: (
    sessionIds: Iterable<string>,
    now?: number,
    options?: { clearWatchdogCooldown?: boolean },
  ) => void;
  orchestratorsRef: MutableRefObject<OrchestratorInstance[]>;
  publishQueuedSessionSlices: (sessionSnapshot?: Session[]) => void;
  queueSessionSliceForRender: (sessionId: string) => void;
  requestStateResync: (options?: RequestStateResyncOptions) => void;
  scheduleCodexStateRender: () => void;
  scheduleSessionRender: () => void;
  seenServerInstanceIdsRef: MutableRefObject<Set<string>>;
  sessionsRef: MutableRefObject<Session[]>;
  setBackendConnectionIssueDetail: Dispatch<SetStateAction<string | null>>;
  setBackendConnectionState: (next: BackendConnectionState) => void;
  setIsLoading: Dispatch<SetStateAction<boolean>>;
  setLastDelegationRepairRequestedRevision: (revision: number) => void;
  setOrchestrators: Dispatch<SetStateAction<OrchestratorInstance[]>>;
  shouldForceAdoptNextStateEvent: () => boolean;
  startSessionHydration: (
    sessionId: string,
    options?: SessionHydrationOptions,
  ) => void;
  syncLiveSessionResumeWatchdogBaselines: (
    sessions: Session[],
    now?: number,
  ) => void;
  syncLiveTransportActivityFromState: (
    sessions: Session[],
    now?: number,
    options?: { clearWatchdogCooldown?: boolean },
  ) => void;
  transportState: TransportRecoveryStateSnapshot;
  triggerRecoveryForDelta: (
    delta: DeltaEvent,
    options?: {
      hydrationOptions?: SessionHydrationOptions;
      requestOptions?: RequestStateResyncOptions;
    },
  ) => void;
};

export function createAppLiveStateTransportEventHandlers(
  context: AppLiveStateTransportEventHandlersContext,
) {
  const {
    adoptState,
    applyDelegationWaitDeltaLocally,
    beginBadLiveEventRecovery,
    cancelStaleSendResponseRecoveryPollForSessions,
    clearForceAdoptNextStateEvent,
    clearRecoveredBackendRequestError,
    clearReconnectStateResyncTimeoutAfterConfirmedReopen,
    codexStateRef,
    confirmReconnectRecoveryFromAuthoritativeSnapshot,
    confirmReconnectRecoveryFromDeltaEvent,
    confirmReconnectRecoveryFromLiveEvent,
    confirmReconnectRecoveryFromStateEvent,
    enqueueWorkspaceFilesChangedEvent,
    forceAdoptNextStateEventRef,
    isCancelled,
    laggedRecoveryBaselineRevisionRef,
    lastSeenServerInstanceIdRef,
    lastLiveTransportActivityAtBySessionId,
    latestStateRevisionRef,
    markLiveSessionResumeWatchdogBaseline,
    markLiveTransportActivity,
    orchestratorsRef,
    publishQueuedSessionSlices,
    queueSessionSliceForRender,
    requestStateResync,
    scheduleCodexStateRender,
    scheduleSessionRender,
    sessionsRef,
    seenServerInstanceIdsRef,
    setBackendConnectionIssueDetail,
    setBackendConnectionState,
    setIsLoading,
    setOrchestrators,
    setLastDelegationRepairRequestedRevision,
    startSessionHydration,
    transportState,
    clearInitialStateResyncRetryTimeout,
    shouldForceAdoptNextStateEvent,
    syncLiveSessionResumeWatchdogBaselines,
    syncLiveTransportActivityFromState,
    triggerRecoveryForDelta,
  } = context;

  function handleStateEvent(event: MessageEvent<string>) {
    if (isCancelled()) {
      return;
    }

    const profiler = createStateEventProfiler();
    let profiledRevision: number | undefined;
    let profiledSessionCount: number | undefined;
    let profiledAdopted: boolean | undefined;
    try {
      const payload = event.data;
      const rawRevision = extractTopLevelJsonNumber(payload, "revision");
      const rawServerInstanceId = extractTopLevelJsonString(
        payload,
        "serverInstanceId",
      );
      const rawIsFallback = payloadHasTopLevelTrueBoolean(
        payload,
        "_sseFallback",
      );
      const forceStateEvent = shouldForceAdoptNextStateEvent();
      profiler?.mark("peek");
      if (
        rawRevision !== null &&
        !rawIsFallback &&
        !shouldAdoptSnapshotRevision(
          latestStateRevisionRef.current,
          rawRevision,
          {
            force: forceStateEvent,
            allowRevisionDowngrade: forceStateEvent,
            lastSeenServerInstanceId: lastSeenServerInstanceIdRef.current,
            nextServerInstanceId: rawServerInstanceId,
            seenServerInstanceIds: seenServerInstanceIdsRef.current,
            allowUnknownServerInstance: forceStateEvent,
          },
        )
      ) {
        profiledRevision = rawRevision;
        profiledAdopted = false;
        clearForceAdoptNextStateEvent();
        if (!transportState.pendingBadLiveEventRecovery) {
          const isEqualOrNewerRejectedState =
            latestStateRevisionRef.current === null ||
            rawRevision >= latestStateRevisionRef.current;
          if (isEqualOrNewerRejectedState) {
            confirmReconnectRecoveryFromStateEvent();
          } else {
            confirmReconnectRecoveryFromLiveEvent();
          }
        }
        profiler?.mark("stalePeekReject");
        setBackendConnectionIssueDetail(null);
        clearRecoveredBackendRequestError();
        profiler?.mark("clearErrors");
        return;
      }

      const state = JSON.parse(payload) as StateEventPayload;
      profiler?.mark("parse");
      profiledRevision = state.revision;
      profiledSessionCount = state.sessions?.length;
      if (state._sseFallback) {
        // Marked fallback payloads only signal that the client should refetch
        // the authoritative snapshot from /api/state.
        clearForceAdoptNextStateEvent();
        profiler?.mark("fallback");
        requestStateResync({
          allowAuthoritativeRollback: true,
          preserveReconnectFallback: true,
        });
        return;
      }

      const force = forceStateEvent;
      // SSE state events are always the first event on a new connection
      // (before any deltas), so there is no risk of a delta racing ahead
      // and being overwritten. Allow revision downgrade so a restarted
      // server (whose persisted revision may be lower) is adopted.
      const adopted = adoptState(state, {
        force,
        allowRevisionDowngrade: force,
        allowUnknownServerInstance: force,
      });
      profiler?.mark("adoptState");
      profiledAdopted = adopted;
      clearForceAdoptNextStateEvent();
      // Confirm recovery after an adopted state. A parseable but rejected
      // state is still useful stream proof in ordinary reconnects, but it
      // must not clear pending bad-event recovery because it did not repair
      // the malformed event.
      if (adopted || !transportState.pendingBadLiveEventRecovery) {
        confirmReconnectRecoveryFromStateEvent();
      }
      if (adopted) {
        cancelStaleSendResponseRecoveryPollForSessions(
          state.sessions.map((session) => session.id),
        );
        clearInitialStateResyncRetryTimeout();
        const adoptedAt = Date.now();
        clearReconnectStateResyncTimeoutAfterConfirmedReopen();
        // A live SSE state payload proves the stream is healthy again, so it also
        // clears any residual watchdog retry cooldown from an earlier fallback probe.
        syncLiveTransportActivityFromState(state.sessions, adoptedAt);
        pruneLiveTransportActivitySessions(
          lastLiveTransportActivityAtBySessionId,
          state.sessions,
        );
        syncLiveSessionResumeWatchdogBaselines(state.sessions, adoptedAt);
      }
      profiler?.mark("postAdoption");
      setBackendConnectionIssueDetail(null);
      clearRecoveredBackendRequestError();
      profiler?.mark("clearErrors");
    } catch (error) {
      clearForceAdoptNextStateEvent();
      if (!isCancelled()) {
        setBackendConnectionIssueDetail(
          describeBackendConnectionIssueDetail(error),
        );
        // A bad reconnect state payload must not leave the client marked as
        // connected without a usable snapshot. Restore "reconnecting" so the
        // retry affordance stays available (onopen already set "connected"),
        // and re-arm fallback polling so recovery continues via /api/state.
        if (transportState.sawReconnectOpenSinceLastError) {
          beginBadLiveEventRecovery();
        }
      }
    } finally {
      if (!isCancelled()) {
        setIsLoading(false);
      }
      profiler?.finish({
        adopted: profiledAdopted,
        revision: profiledRevision,
        sessionCount: profiledSessionCount,
      });
    }
  }

  function handleDeltaEvent(event: MessageEvent<string>) {
    if (isCancelled()) {
      return;
    }

    try {
      const delta = JSON.parse(event.data) as DeltaEvent;
      const currentRevision = latestStateRevisionRef.current;
      if (isDelegationDeltaEvent(delta)) {
        applyDelegationWaitDeltaLocally(delta);
        if (currentRevision === null || delta.revision >= currentRevision) {
          if (transportState.delegationRepairAdoptedSinceLastReconnectError) {
            confirmReconnectRecoveryFromDeltaEvent();
          }
          setLastDelegationRepairRequestedRevision(delta.revision);
          requestStateResync({
            allowAuthoritativeRollback: currentRevision !== null,
            // A delegation delta can require an authoritative `/api/state`
            // repair for broad delegation/project state, but adopting that
            // snapshot is not proof the reopened SSE stream is healthy.
            // Keep reconnect polling armed until a later data-bearing SSE
            // event confirms live delivery after the repair.
            forceAdoptEqualOrNewerRevision: delta.revision,
            rearmOnFailure: true,
          });
        } else if (!transportState.pendingBadLiveEventRecovery) {
          confirmReconnectRecoveryFromDeltaEvent();
        }
        setBackendConnectionIssueDetail(null);
        clearRecoveredBackendRequestError();
        return;
      }
      const revisionAction = decideDeltaRevisionAction(
        currentRevision,
        delta.revision,
      );
      if (revisionAction === "ignore") {
        if (
          currentRevision !== null &&
          delta.revision === currentRevision &&
          isSameRevisionReplayableSessionDelta(delta)
        ) {
          const result = applyDeltaToSessions(sessionsRef.current, delta);
          const replayableMaterialApply =
            result.kind === "applied" ||
            result.kind === "appliedNeedsResync";
          if (replayableMaterialApply) {
            confirmReconnectRecoveryFromDeltaEvent();
            const appliedAt = Date.now();
            cancelStaleSendResponseRecoveryPollForSessions([delta.sessionId]);
            markLiveTransportActivity([delta.sessionId], appliedAt);
            markLiveSessionResumeWatchdogBaseline(
              [delta.sessionId],
              appliedAt,
            );
            latestStateRevisionRef.current = delta.revision;
            sessionsRef.current = result.sessions;
            const updatedSession =
              result.sessions.find(
                (session) => session.id === delta.sessionId,
              ) ?? null;
            if (updatedSession) {
              queueSessionSliceForRender(updatedSession.id);
              publishQueuedSessionSlices(result.sessions);
            }
            scheduleSessionRender();
            setBackendConnectionIssueDetail(null);
            clearRecoveredBackendRequestError();
            if (result.kind === "appliedNeedsResync") {
              triggerRecoveryForDelta(delta, {
                requestOptions: {
                  allowAuthoritativeRollback: true,
                  forceAdoptEqualOrNewerRevision: delta.revision,
                  rearmOnFailure: true,
                },
                hydrationOptions: {
                  queueAfterCurrent: true,
                },
              });
            }
            return;
          }

          // Two cases reach here without a material apply:
          //   1. `result.kind === "needsResync"` — the delta references a
          //      session the client doesn't know about. For a same-revision
          //      delta, the global revision hasn't advanced, so this is
          //      most likely a stale stream replay (session GC'd / spurious
          //      re-emission); a `/api/state` probe at the SAME revision
          //      would just return what we already have. The protocol
          //      contract in `docs/architecture.md` says session creation
          //      advances the main revision, so any real divergence is
          //      reconciled by the next authoritative state event.
          //   2. `result.kind === "appliedNoOp"` — the delta's content
          //      matches what the session already has (e.g., a textReplace
          //      whose new text is identical to the existing message text).
          //      Marking transport activity / watchdog baseline here would
          //      mask a stalled active session that happens to be receiving
          //      these dead-replay deltas and prevent the watchdog from ever
          //      firing.
          // In both cases, fall through to the generic ignored-delta
          // confirmation block so the delta's arrival still serves as
          // proof the SSE stream is alive (cancels the reconnect fallback
          // / clears bad-live-event recovery when same-revision after a
          // bad event) without doing any spurious resync work.
        }

        cancelStaleSendResponseRecoveryPollForSessions(
          staleSendRecoveryPollSessionIdsForDelta(delta),
        );
        // An ignored delta normally proves the client already has data at
        // this revision or newer. After a bad reopened live event, only a
        // current-revision ignored delta proves the stream is healthy again;
        // lower stale frames do not repair the lost event.
        const ignoredDeltaConfirmsBadLiveEventRecovery =
          transportState.pendingBadLiveEventRecovery &&
          latestStateRevisionRef.current !== null &&
          delta.revision === latestStateRevisionRef.current;
        if (
          !transportState.pendingBadLiveEventRecovery ||
          ignoredDeltaConfirmsBadLiveEventRecovery
        ) {
          confirmReconnectRecoveryFromDeltaEvent();
        }
        setBackendConnectionIssueDetail(null);
        clearRecoveredBackendRequestError();
        return;
      }
      if (revisionAction === "resync") {
        cancelStaleSendResponseRecoveryPollForSessions(
          staleSendRecoveryPollSessionIdsForDelta(delta),
        );
        if (
          isSessionDeltaEvent(delta) &&
          sessionDeltaAdvancesCurrentMutationStamp(
            sessionsRef.current,
            delta,
          )
        ) {
          const result = applyDeltaToSessions(sessionsRef.current, delta);
          if (
            result.kind === "applied" ||
            result.kind === "appliedNeedsResync"
          ) {
            const appliedAt = Date.now();
            markLiveTransportActivity([delta.sessionId], appliedAt);
            markLiveSessionResumeWatchdogBaseline(
              [delta.sessionId],
              appliedAt,
            );
            latestStateRevisionRef.current = delta.revision;
            sessionsRef.current = result.sessions;
            const updatedSession =
              result.sessions.find(
                (session) => session.id === delta.sessionId,
              ) ?? null;
            if (updatedSession) {
              queueSessionSliceForRender(updatedSession.id);
              publishQueuedSessionSlices(result.sessions);
            }
            scheduleSessionRender();
            setBackendConnectionIssueDetail(null);
            clearRecoveredBackendRequestError();
            triggerRecoveryForDelta(delta, {
              requestOptions: {
                allowAuthoritativeRollback: true,
                rearmOnFailure: true,
              },
              hydrationOptions:
                result.kind === "appliedNeedsResync" ||
                delta.type === "textDelta"
                  ? {
                      allowDivergentTextRepairAfterNewerRevision:
                        delta.type === "textDelta",
                      queueAfterCurrent: result.kind === "appliedNeedsResync",
                    }
                  : undefined,
            });
            return;
          }
        }
        // A revision gap means we missed events but the stream IS working.
        // Do NOT confirm recovery yet — if the follow-up /api/state fetch
        // fails, the client must stay in the reconnecting state. Use
        // rearmOnFailure so a failed resync re-arms polling instead of
        // stalling recovery.
        // Force per-session re-hydration as well so the affected session's
        // full transcript is re-fetched even if the /api/state summary's
        // reconcile decides the session looks fresh enough to keep
        // `messagesLoaded: true`. `hydratingSessionIdsRef` deduplicates so
        // a no-op when hydration is already in flight or queued.
        triggerRecoveryForDelta(delta, {
          hydrationOptions: { queueAfterCurrent: true },
        });
        return;
      }

      if (delta.type === "codexUpdated") {
        confirmReconnectRecoveryFromDeltaEvent();
        latestStateRevisionRef.current = delta.revision;
        codexStateRef.current = delta.codex;
        scheduleCodexStateRender();
        setBackendConnectionIssueDetail(null);
        clearRecoveredBackendRequestError();
        return;
      }

      if (delta.type === "orchestratorsUpdated") {
        // Global orchestrator updates prove the SSE stream is healthy enough to
        // clear reconnect fallback state. When the delta also carries session
        // snapshots, treat those specific ids as live data for watchdog baselines.
        confirmReconnectRecoveryFromDeltaEvent();
        const appliedAt = Date.now();
        if (delta.sessions?.length) {
          const deltaSessionIds = delta.sessions.map((session) => session.id);
          cancelStaleSendResponseRecoveryPollForSessions(deltaSessionIds);
          markLiveTransportActivity(deltaSessionIds, appliedAt);
          markLiveSessionResumeWatchdogBaseline(deltaSessionIds, appliedAt);
        }
        latestStateRevisionRef.current = delta.revision;
        const nextSessions = mergeOrchestratorDeltaSessions(
          sessionsRef.current,
          delta.sessions,
        );
        sessionsRef.current = nextSessions;
        const deltaSessionIds = new Set(
          (delta.sessions ?? []).map((session) => session.id),
        );
        deltaSessionIds.forEach((sessionId) => {
          queueSessionSliceForRender(sessionId);
        });
        publishQueuedSessionSlices(nextSessions);
        orchestratorsRef.current = delta.orchestrators;
        startTransition(() => {
          setOrchestrators(delta.orchestrators);
        });
        scheduleSessionRender();
        setBackendConnectionIssueDetail(null);
        clearRecoveredBackendRequestError();
        return;
      }

      // Non-session deltas such as codexUpdated/orchestratorsUpdated are handled above; the
      // session reducer only accepts deltas that carry a concrete sessionId.
      const result = applyDeltaToSessions(sessionsRef.current, delta);
      if (result.kind === "appliedNoOp") {
        confirmReconnectRecoveryFromDeltaEvent();
        cancelStaleSendResponseRecoveryPollForSessions([delta.sessionId]);
        latestStateRevisionRef.current = delta.revision;
        sessionsRef.current = result.sessions;
        setBackendConnectionIssueDetail(null);
        clearRecoveredBackendRequestError();
        return;
      }
      if (
        result.kind === "applied" ||
        result.kind === "appliedNeedsResync"
      ) {
        confirmReconnectRecoveryFromDeltaEvent();
        const appliedAt = Date.now();
        // Every session-scoped delta proves liveness for that session, including
        // any future delta shape that revives it back to "active".
        cancelStaleSendResponseRecoveryPollForSessions([delta.sessionId]);
        markLiveTransportActivity([delta.sessionId], appliedAt);
        markLiveSessionResumeWatchdogBaseline([delta.sessionId], appliedAt);
        latestStateRevisionRef.current = delta.revision;
        sessionsRef.current = result.sessions;
        const updatedSession =
          result.sessions.find((session) => session.id === delta.sessionId) ??
          null;
        if (updatedSession) {
          queueSessionSliceForRender(updatedSession.id);
          publishQueuedSessionSlices(result.sessions);
        }
        scheduleSessionRender();
        setBackendConnectionIssueDetail(null);
        clearRecoveredBackendRequestError();
        if (result.kind === "appliedNeedsResync") {
          // Metadata-only fallback fired for an unhydrated session whose target
          // message is not in the retained transcript. The metadata patch keeps
          // the sidebar fresh, but the message body itself only arrives via
          // an authoritative state fetch — schedule one so a stuck/queued
          // hydration cannot leave the user staring at a stale transcript.
          // Force per-session re-hydration too: `/api/state` returns only the
          // metadata-first summary, and `applyMetadataOnlySessionDelta`
          // already advanced the local mutation stamp to match what the
          // delta carried. If the backend's stamp didn't move past that, the
          // summary's `reconcileSummarySession` would not flip
          // `messagesLoaded` back to false and the hydration effect would
          // not re-fire. Calling `startSessionHydration` directly fetches
          // the full transcript via `/api/sessions/{id}` so the missing
          // message body actually appears. See bugs.md "Stuck assistant
          // reply visible only after refresh".
          triggerRecoveryForDelta(delta, {
            hydrationOptions: { queueAfterCurrent: true },
          });
        }
        return;
      }
      // Reducer rejected the delta as out-of-sync (missing target on a
      // hydrated session, type/id mismatch, count regression, …). Schedule
      // the authoritative state resync as before AND force a per-session
      // re-hydration: the `/api/state` summary alone may not flip
      // `messagesLoaded` back to false (mutation stamps can match even
      // though the local transcript is missing a message), so without the
      // direct hydration the user can stay stuck on a stale transcript
      // until they refresh. Same reasoning as the appliedNeedsResync
      // branch above. See bugs.md "Stuck assistant reply visible only
      // after refresh".
      triggerRecoveryForDelta(delta);
    } catch {
      // Parse or reducer failure — restore reconnecting state so the retry
      // affordance stays available, and re-arm polling.
      if (transportState.sawReconnectOpenSinceLastError) {
        beginBadLiveEventRecovery();
      } else {
        requestStateResync({ rearmOnFailure: true });
      }
    }
  }

  function handleWorkspaceFilesChangedEvent(event: MessageEvent<string>) {
    if (isCancelled()) {
      return;
    }

    try {
      const filesChanged = JSON.parse(
        event.data,
      ) as WorkspaceFilesChangedEvent;
      if (!transportState.pendingBadLiveEventRecovery) {
        confirmReconnectRecoveryFromLiveEvent();
        clearReconnectStateResyncTimeoutAfterConfirmedReopen();
      }
      setBackendConnectionIssueDetail(null);
      clearRecoveredBackendRequestError();
      enqueueWorkspaceFilesChangedEvent(filesChanged);
    } catch {
      // File-change events are non-authoritative hints. If one is malformed,
      // keep the main state stream alive and wait for the next event/snapshot.
    }
  }

  function handleLaggedEvent() {
    if (isCancelled()) {
      return;
    }
    // The backend emits this when an SSE broadcast receiver fell past the
    // channel capacity and dropped events. A recovery state snapshot follows
    // immediately, but its revision may equal `latestStateRevisionRef.current`
    // (the client read some events from the burst before falling behind), so
    // the gate in `handleStateEvent` would otherwise reject it as a redundant
    // catch-up. Arm force-adopt so the next state event is taken regardless
    // of revision parity. See bugs.md "SSE Lagged-recovery snapshot can be
    // silently ignored".
    laggedRecoveryBaselineRevisionRef.current = latestStateRevisionRef.current;
    forceAdoptNextStateEventRef.current = true;
  }



  return {
    handleDeltaEvent,
    handleLaggedEvent,
    handleStateEvent,
    handleWorkspaceFilesChangedEvent,
  };
}
