// app-live-state-transport-events.ts
//
// Owns: EventSource event handlers for app-live-state-transport.
// The surrounding transport hook owns timers, EventSource setup/teardown,
// and the state-resync loop; app-live-state-reconnect-state owns the
// reconnect proof flags consumed here.
//
// Split out of: ui/src/app-live-state-transport.ts.

import {
  startTransition,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import type { StateResponse } from "./api";
import type { SessionReadRef, TranscriptRepairAuthority } from "./transcript-repair-authority";
import { isBodyDelta, bodySequenceProof } from "./transcript-body-sequence";
import {
  applyDeltaToSessions,
  applyMetadataOnlySessionDelta,
  pruneLiveTransportActivitySessions,
  sessionDeltaAdvancesCurrentMutationStamp,
  type DeltaApplyResult,
} from "./live-updates";
import {
  classifyDeltaServerIdentity,
  decideDeltaRevisionAction,
  shouldAdoptSnapshotRevision,
} from "./state-revision";
import {
  isAuthoritativeRepairDeltaEvent,
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
import type { ReconnectRecoveryStateSnapshot } from "./app-live-state-reconnect-state";
import type { SessionHydrationOptions } from "./app-live-state-hydration";
import type { HydrationDeltaObservation } from "./session-hydration-adoption";

type AppLiveStateTransportEventHandlersContext = {
  sessionAuthority: TranscriptRepairAuthority;
  requestSessionTailRead: (sessionId: string) => void;
  observeHydrationDelta?: (observation: HydrationDeltaObservation) => void;
  hasPartialTailAppendProof?: (sessionId?: string) => boolean;
  markTranscriptsDirty?: () => void;
  adoptState: (state: StateResponse, options?: AdoptStateOptions) => boolean;
  applyDelegationWaitDeltaLocally: (delta: DeltaEvent) => void;
  applyTestRunDeltaLocally?: (delta: DeltaEvent) => void;
  beginBadLiveEventRecovery: () => void;
  cancelStaleSendResponseRecoveryPollForSessions: (
    sessionIds: Iterable<string>,
  ) => void;
  clearForceAdoptNextStateEvent: (transcriptsRepaired?: boolean) => void;
  clearInitialStateResyncRetryTimeout: () => void;
  clearRecoveredBackendRequestError: () => void;
  clearReconnectStateResyncTimeoutAfterConfirmedReopen: () => void;
  codexStateRef: MutableRefObject<CodexState>;
  confirmReconnectRecoveryFromAuthoritativeSnapshot: () => boolean;
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
  publishQueuedSessionSlices: (sessionSnapshot?: readonly Session[]) => void;
  queueSessionSliceForRender: (sessionId: string) => void;
  requestStateResync: (options?: RequestStateResyncOptions) => void;
  scheduleCodexStateRender: () => void;
  scheduleSessionRender: () => void;
  seenServerInstanceIdsRef: MutableRefObject<Set<string>>;
  sessionsRef: SessionReadRef;
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
    sessions: readonly Session[],
    now?: number,
  ) => void;
  syncLiveTransportActivityFromState: (
    sessions: readonly Session[],
    now?: number,
    options?: { clearWatchdogCooldown?: boolean },
  ) => void;
  transportState: ReconnectRecoveryStateSnapshot;
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
  // Observation is optional and cannot change transport recovery or publication.
  // Called before refs are published, so both reducer inputs and outputs exist.
  function observeDelta(
    delta: DeltaEvent | null,
    revisionAction: HydrationDeltaObservation["revisionAction"],
    result?: DeltaApplyResult,
  ) {
    if (!context.observeHydrationDelta) return;
    try {
      const sessionId = delta && "sessionId" in delta ? delta.sessionId : null;
      // Global invalidation (including a gap on another session) must reach
      // every tracked proof. Ordinary session deltas only need their own proof.
      const proofSessionId = revisionAction === "resync" ? undefined : sessionId ?? undefined;
      let hasProof: boolean | undefined;
      try {
        hasProof = context.hasPartialTailAppendProof?.(proofSessionId);
      } catch {
        // An unavailable/uncertain predicate must retain the observation fence.
      }
      if (hasProof === false) return;
      const previousSession = context.sessionsRef.current.find(
        (entry) => entry.id === sessionId,
      ) ?? null;
      const nextSession = result && "sessions" in result
        ? result.sessions.find((entry) => entry.id === sessionId) ?? null : null;
      context.observeHydrationDelta({
        delta,
        revisionAction,
        resultKind: result?.kind ?? null,
        previousSession,
        nextSession,
        targetPresent: Boolean(
          delta && "messageId" in delta &&
          previousSession?.messages.some((message) => message.id === delta.messageId),
        ),
      });
    } catch {
      // A subscriber failure must never interrupt the event's original path.
    }
  }
  const {
    adoptState,
    markTranscriptsDirty,
    applyDelegationWaitDeltaLocally,
    applyTestRunDeltaLocally,
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

  function applyTranscriptDelta(delta: Parameters<typeof applyDeltaToSessions>[1], revisionAction: HydrationDeltaObservation["revisionAction"]) {
    const result = applyDeltaToSessions(sessionsRef.current, delta);
    // The append proof needs both the resident input and reducer output.
    // Observe before the single publication gate replaces the live projection.
    observeDelta(delta, revisionAction, result);
    return "sessions" in result
      ? { ...result, sessions: context.sessionAuthority.commit(result.sessions, "delta") }
      : result;
  }

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
      const forceStateEvent = shouldForceAdoptNextStateEvent() ||
        // A delta after the loss marker cancels rollback permission, not
        // transcript repair from a snapshot covering that newer delta.
        (laggedRecoveryBaselineRevisionRef.current !== null &&
          rawRevision !== null &&
          (latestStateRevisionRef.current === null ||
            rawRevision >= latestStateRevisionRef.current));
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
            void confirmReconnectRecoveryFromStateEvent();
          } else {
            void confirmReconnectRecoveryFromLiveEvent();
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
        observeDelta(null, "resync");
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
      const previousSessions = sessionsRef.current;
      // SSE state events are always the first event on a new connection
      // (before any deltas), so there is no risk of a delta racing ahead
      // and being overwritten. Allow revision downgrade so a restarted
      // server (whose persisted revision may be lower) is adopted.
      const adopted = adoptState(state, {
        force,
        allowRevisionDowngrade: force,
        allowUnknownServerInstance: force,
        // Loss marks local attached tails dirty without evicting their bodies.
        // The ordinary hydration flight covers that demand separately.
        forceMessagesUnloaded: false,
      });
      profiler?.mark("adoptState");
      profiledAdopted = adopted;
      clearForceAdoptNextStateEvent(adopted);
      // Confirm recovery after an adopted state. A parseable but rejected
      // state is still useful stream proof in ordinary reconnects, but it
      // must not clear pending bad-event recovery because it did not repair
      // the malformed event.
      if (adopted || !transportState.pendingBadLiveEventRecovery) {
        void confirmReconnectRecoveryFromStateEvent();
      }
      if (adopted) {
        const previousById = new Map(
          previousSessions.map((session) => [session.id, session]),
        );
        for (const summary of state.sessions) {
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
            // Metadata-first snapshots intentionally omit pending prompt
            // bodies. The opaque identity/disposition hash changes for queue
            // additions, removals, replacements, retained-state changes and
            // pause changes without exposing those bodies. Older remotes that
            // omit the hash keep the bounded legacy fallback. Equal-revision
            // private replay never reaches this branch.
            startSessionHydration(summary.id, { queueAfterCurrent: true });
          }
        }
        cancelStaleSendResponseRecoveryPollForSessions(
          state.sessions.map((session) => session.id),
        );
        clearInitialStateResyncRetryTimeout();
        const adoptedAt = Date.now();
        clearReconnectStateResyncTimeoutAfterConfirmedReopen();
        // A live SSE state payload proves the stream is healthy again, so it also
        // clears any residual watchdog retry cooldown from an earlier fallback probe.
        syncLiveTransportActivityFromState(sessionsRef.current, adoptedAt);
        pruneLiveTransportActivitySessions(
          lastLiveTransportActivityAtBySessionId,
          sessionsRef.current,
        );
        syncLiveSessionResumeWatchdogBaselines(
          sessionsRef.current,
          adoptedAt,
        );
      }
      profiler?.mark("postAdoption");
      setBackendConnectionIssueDetail(null);
      clearRecoveredBackendRequestError();
      profiler?.mark("clearErrors");
    } catch (error) {
      observeDelta(null, "resync");
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
        } else {
          requestStateResync({ rearmOnFailure: true });
        }
      }
    } finally {
      if (!isCancelled()) {
        setIsLoading(latestStateRevisionRef.current === null);
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
      if (isBodyDelta(delta)) {
        const beforeBody = sessionsRef.current;
        // Global HTTP progress cannot hide a session frame from this ledger.
        // The owner buffers it even when its revision would be ignored below.
        const decision = context.sessionAuthority.receiveBodyDelta(delta);
        if (decision === "legacy" && bodySequenceProof(delta)) {
          // Let the legacy reducer publish first, including a first message in
          // an empty creation response. The same read coalesces display/frame
          // demand and starts only for a visible uncertified resident window.
          queueMicrotask(() => {
            if (!isCancelled()) context.requestSessionTailRead(delta.sessionId);
          });
        }
        if (decision !== "legacy") {
          // The two ledgers answer different questions. Body placement cannot
          // account for a missing global frame (possibly session metadata).
          const revisionAction = decideDeltaRevisionAction(currentRevision, delta.revision);
          observeDelta(delta, revisionAction);
          const metadataAllowed = revisionAction === "apply" ||
            (revisionAction === "ignore" && delta.revision === currentRevision &&
              isSameRevisionReplayableSessionDelta(delta)) ||
            (revisionAction === "resync" && sessionDeltaAdvancesCurrentMutationStamp(beforeBody, delta));
          // A duplicate body may still carry globally current metadata. The
          // owner orders its stamp independently at the publication gate.
          const previous = beforeBody.find(session => session.id === delta.sessionId);
          if (metadataAllowed && previous) {
            // Compute queue evidence against the pre-placement window, but
            // publish through the owner which retains the already placed body.
            const metadata = applyMetadataOnlySessionDelta(previous, delta);
            context.sessionAuthority.commit(sessionsRef.current.map(session =>
              session.id === delta.sessionId ? metadata : session), "metadata");
          }
          if (revisionAction === "apply") latestStateRevisionRef.current = delta.revision;
          if (revisionAction === "resync") requestStateResync({ rearmOnFailure: true });
          queueMicrotask(() => {
            if (!isCancelled()) context.requestSessionTailRead(delta.sessionId);
          });
          if (decision === "applied") {
            const appliedAt = Date.now();
            cancelStaleSendResponseRecoveryPollForSessions([delta.sessionId]);
            markLiveTransportActivity([delta.sessionId], appliedAt);
            markLiveSessionResumeWatchdogBaseline([delta.sessionId], appliedAt);
            queueSessionSliceForRender(delta.sessionId);
            publishQueuedSessionSlices(sessionsRef.current);
            scheduleSessionRender();
          }
          if (revisionAction !== "resync" &&
              (revisionAction !== "ignore" || !transportState.pendingBadLiveEventRecovery ||
                delta.revision === currentRevision)) {
            void confirmReconnectRecoveryFromDeltaEvent();
            setBackendConnectionIssueDetail(null);
            clearRecoveredBackendRequestError();
          }
          if (metadataAllowed && decision !== "applied") {
            queueSessionSliceForRender(delta.sessionId);
            publishQueuedSessionSlices(sessionsRef.current);
            scheduleSessionRender();
          }
          return;
        }
      }
      if (delta.type === "testRunWaitCreated" || delta.type === "testRunWaitConsumed" ||
          delta.type === "delegationWaitCreated" || delta.type === "delegationWaitConsumed") {
        const identity = classifyDeltaServerIdentity(
          delta.serverInstanceId, lastSeenServerInstanceIdRef.current, seenServerInstanceIdsRef.current,
        );
        if (identity !== "current") {
          // A retired server cannot mutate a replacement's waits or request
          // repair at its old revision. Unknown/missing identity needs a
          // snapshot; never compare that delta's revision to the current one.
          if (identity === "unproven") {
            requestStateResync({
              allowSameServerEqualRevision: true,
              // A tagged replacement hint survives retries only while the
              // observed origin is still current, and only for that target.
              waitRepair: {
                observedServerInstanceId: lastSeenServerInstanceIdRef.current,
                replacementServerInstanceId: delta.serverInstanceId || undefined,
              },
            });
          }
          return;
        }
        if ((delta.type === "testRunWaitCreated" || delta.type === "delegationWaitCreated") &&
            currentRevision !== null && delta.revision < currentRevision) {
          // A session-only response may have advanced the revision without
          // refreshing waits. Do not apply the stale create; repair the list.
          requestStateResync({
            allowSameServerEqualRevision: true,
            waitRepair: { observedServerInstanceId: lastSeenServerInstanceIdRef.current },
          });
          // This current-server frame still proves live delivery, independently
          // of the wait-list repair. A stale frame cannot repair a bad event.
          if (!transportState.pendingBadLiveEventRecovery) {
            void confirmReconnectRecoveryFromDeltaEvent();
          }
          setBackendConnectionIssueDetail(null);
          clearRecoveredBackendRequestError();
          return;
        }
      }
      // Dispatch outcomes have their own ordered revision on current servers.
      // Consume a contiguous current-server revision so the next delta does
      // not look lost. Older hosts/replays may share the consumed revision;
      // keep their notices without advancing. The hook deduplicates outcomes
      // independently of snapshot adoption and rejects retired-server notices.
      if (delta.type === "testRunWaitResumeDispatchFailed") {
        applyTestRunDeltaLocally?.(delta);
        if (classifyDeltaServerIdentity(
          delta.serverInstanceId, lastSeenServerInstanceIdRef.current, seenServerInstanceIdsRef.current,
        ) === "current") {
          const action = decideDeltaRevisionAction(currentRevision, delta.revision);
          observeDelta(delta, action);
          if (action === "apply") {
            latestStateRevisionRef.current = delta.revision;
          } else if (action === "resync") {
            requestStateResync({ rearmOnFailure: true });
          }
        }
        return;
      }
      // Delegation and test-run wait deltas carry only part of their commit
      // (a consumed wait also queued a resume prompt; sibling waits share the
      // revision). Apply the wait locally by id, never move the global
      // revision from it, and repair from the authoritative snapshot.
      if (isAuthoritativeRepairDeltaEvent(delta)) {
        if (currentRevision === null || delta.revision > currentRevision) {
          observeDelta(delta, "resync");
        }
        if (isDelegationDeltaEvent(delta)) {
          applyDelegationWaitDeltaLocally(delta);
        } else {
          applyTestRunDeltaLocally?.(delta);
        }
        if (currentRevision === null || delta.revision >= currentRevision) {
          if (transportState.delegationRepairAdoptedSinceLastReconnectError) {
            void confirmReconnectRecoveryFromDeltaEvent();
          }
          setLastDelegationRepairRequestedRevision(delta.revision);
          requestStateResync({
            allowAuthoritativeRollback: currentRevision !== null,
            // A delegation or wait delta can require an authoritative
            // `/api/state` repair for state its commit changed, but adopting
            // that snapshot is not proof the reopened SSE stream is healthy.
            // Keep reconnect polling armed until a later data-bearing SSE
            // event confirms live delivery after the repair.
            forceAdoptEqualOrNewerRevision: delta.revision,
            rearmOnFailure: true,
          });
        } else if (!transportState.pendingBadLiveEventRecovery) {
          void confirmReconnectRecoveryFromDeltaEvent();
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
          const result = applyTranscriptDelta(delta, revisionAction);
          const replayableMaterialApply =
            result.kind === "applied" ||
            result.kind === "appliedNeedsResync";
          if (replayableMaterialApply) {
            void confirmReconnectRecoveryFromDeltaEvent();
            const appliedAt = Date.now();
            cancelStaleSendResponseRecoveryPollForSessions([delta.sessionId]);
            markLiveTransportActivity([delta.sessionId], appliedAt);
            markLiveSessionResumeWatchdogBaseline(
              [delta.sessionId],
              appliedAt,
            );
            latestStateRevisionRef.current = delta.revision;
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
                  forceTailRepair: true,
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
          void confirmReconnectRecoveryFromDeltaEvent();
        }
        setBackendConnectionIssueDetail(null);
        clearRecoveredBackendRequestError();
        return;
      }
      if (revisionAction === "resync") {
        observeDelta(delta, revisionAction);
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
          const result = applyTranscriptDelta(delta, revisionAction);
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
                      forceTailRepair: delta.type !== "textDelta",
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
        // Force a bounded per-session repair as well so the affected session's
        // recent tail is re-fetched even if the /api/state summary's reconcile
        // decides the session looks fresh enough to keep
        // `messagesLoaded: true`. `hydratingSessionIdsRef` deduplicates so
        // a no-op when hydration is already in flight or queued.
        triggerRecoveryForDelta(delta, {
          hydrationOptions: {
            forceTailRepair: true,
            queueAfterCurrent: true,
          },
        });
        return;
      }

      if (delta.type === "codexUpdated") {
        void confirmReconnectRecoveryFromDeltaEvent();
        latestStateRevisionRef.current = delta.revision;
        codexStateRef.current = delta.codex;
        scheduleCodexStateRender();
        setBackendConnectionIssueDetail(null);
        clearRecoveredBackendRequestError();
        return;
      }

      // Each test-run index delta has a commit of its own, so it may move the
      // revision. Wait deltas take the authoritative-repair branch above.
      if (delta.type === "testRunChanged" || delta.type === "testRunRemoved") {
        if (!applyTestRunDeltaLocally) {
          requestStateResync({ rearmOnFailure: true });
          return;
        }
        void confirmReconnectRecoveryFromDeltaEvent();
        latestStateRevisionRef.current = delta.revision;
        setBackendConnectionIssueDetail(null);
        clearRecoveredBackendRequestError();
        applyTestRunDeltaLocally(delta);
        return;
      }

      if (delta.type === "orchestratorsUpdated") {
        if (delta.sessions?.length) observeDelta(delta, "apply");
        // Global orchestrator updates prove the SSE stream is healthy enough to
        // clear reconnect fallback state. When the delta also carries session
        // snapshots, treat those specific ids as live data for watchdog baselines.
        void confirmReconnectRecoveryFromDeltaEvent();
        const appliedAt = Date.now();
        if (delta.sessions?.length) {
          const deltaSessionIds = delta.sessions.map((session) => session.id);
          cancelStaleSendResponseRecoveryPollForSessions(deltaSessionIds);
          markLiveTransportActivity(deltaSessionIds, appliedAt);
          markLiveSessionResumeWatchdogBaseline(deltaSessionIds, appliedAt);
        }
        latestStateRevisionRef.current = delta.revision;
        const mergedSessions = mergeOrchestratorDeltaSessions(
          sessionsRef.current,
          delta.sessions,
        );
        const nextSessions = context.sessionAuthority.commit(mergedSessions, "metadata");
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
      const result = applyTranscriptDelta(delta, revisionAction);
      if (result.kind === "appliedNoOp") {
        void confirmReconnectRecoveryFromDeltaEvent();
        cancelStaleSendResponseRecoveryPollForSessions([delta.sessionId]);
        latestStateRevisionRef.current = delta.revision;
        setBackendConnectionIssueDetail(null);
        clearRecoveredBackendRequestError();
        return;
      }
      if (
        result.kind === "applied" ||
        result.kind === "appliedNeedsResync"
      ) {
        void confirmReconnectRecoveryFromDeltaEvent();
        const appliedAt = Date.now();
        // Every session-scoped delta proves liveness for that session, including
        // any future delta shape that revives it back to "active".
        cancelStaleSendResponseRecoveryPollForSessions([delta.sessionId]);
        markLiveTransportActivity([delta.sessionId], appliedAt);
        markLiveSessionResumeWatchdogBaseline([delta.sessionId], appliedAt);
        latestStateRevisionRef.current = delta.revision;
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
          // the authoritative bounded tail via `/api/sessions/{id}` so a
          // missing recent message body can appear. See bugs.md "Stuck assistant
          // reply visible only after refresh".
          triggerRecoveryForDelta(delta, {
            hydrationOptions: {
              forceTailRepair: true,
              queueAfterCurrent: true,
            },
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
      triggerRecoveryForDelta(delta, {
        hydrationOptions: { forceTailRepair: true },
      });
    } catch {
      observeDelta(null, "resync");
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
        void confirmReconnectRecoveryFromLiveEvent();
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
    observeDelta(null, "resync");
    markTranscriptsDirty?.();
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
