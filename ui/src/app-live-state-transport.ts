// app-live-state-transport.ts
//
// Owns: EventSource setup/teardown, SSE state/delta/workspace event
// handlers, reconnect fallback polling, live-session watchdogs,
// visibility/focus/pagehide/pageshow resume recovery, and the
// per-mount state-resync loop used by useAppLiveState.
//
// Does not own: state adoption, session hydration fetches, workspace-file
// event gate internals, or UI state. Those remain in app-live-state.ts
// and are supplied here as callbacks/refs.
//
// Split out of: ui/src/app-live-state.ts.

import {
  useEffect,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import {
  fetchState,
  isBackendUnavailableError,
  type StateResponse,
} from "./api";
import {
  LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS,
  LIVE_SESSION_WATCHDOG_RESYNC_RETRY_COOLDOWN_MS,
  pruneLiveTransportActivitySessions,
  sessionHasPotentiallyStaleTransport,
} from "./live-updates";
import {
  isServerInstanceMismatch,
  shouldAdoptSnapshotRevision,
} from "./state-revision";
import {
  coalescePendingStateResyncOptions,
  consumePendingStateResyncOptions,
  type PendingStateResyncOptions,
  type RequestStateResyncOptions,
} from "./app-live-state-resync-options";
import { isSessionDeltaEvent } from "./app-live-state-delta-events";
import {
  markLiveSessionResumeWatchdogBaseline as markLiveSessionResumeWatchdogBaselineActivity,
  markLiveTransportActivity as markLiveTransportSessionActivity,
  syncLiveSessionResumeWatchdogBaselines as syncLiveSessionResumeWatchdogBaselineActivity,
  syncLiveTransportActivityFromState as syncLiveTransportSessionActivityFromState,
} from "./app-live-state-activity";
import { readNavigatorOnline } from "./app-utils";
import type {
  CodexState,
  DeltaEvent,
  OrchestratorInstance,
  Session,
  WorkspaceFilesChangedEvent,
} from "./types";
import {
  clearWorkspaceFilesChangedEventBuffer,
  type WorkspaceFilesChangedEventGateRefs,
} from "./app-live-state-workspace-events";
import {
  describeBackendConnectionIssueDetail,
  type BackendConnectionState,
} from "./backend-connection";
import {
  LIVE_SESSION_RESUME_WATCHDOG_INTERVAL_MS,
  RECONNECT_STATE_RESYNC_DELAY_MS,
  RECONNECT_STATE_RESYNC_MAX_DELAY_MS,
} from "./app-shell-internals";
import type { AdoptStateOptions } from "./app-live-state-types";
import { createAppLiveStateTransportEventHandlers } from "./app-live-state-transport-events";
import { ReconnectStateMachine } from "./app-live-state-reconnect-state";

type SessionHydrationOptions = {
  allowDivergentTextRepairAfterNewerRevision?: boolean;
  queueAfterCurrent?: boolean;
};

type UseAppLiveStateTransportParams = {
  adoptState: (state: StateResponse, options?: AdoptStateOptions) => boolean;
  applyDelegationWaitDeltaLocally: (delta: DeltaEvent) => void;
  cancelStaleSendResponseRecoveryPollForSessions: (
    sessionIds: Iterable<string>,
  ) => void;
  clearRecoveredBackendRequestError: () => void;
  codexStateRef: MutableRefObject<CodexState>;
  enqueueWorkspaceFilesChangedEvent: (
    filesChanged: WorkspaceFilesChangedEvent,
  ) => void;
  forceAdoptNextStateEventRef: MutableRefObject<boolean>;
  hydrationRestartResyncPendingRef: MutableRefObject<boolean>;
  laggedRecoveryBaselineRevisionRef: MutableRefObject<number | null>;
  lastSeenServerInstanceIdRef: MutableRefObject<string | null>;
  latestStateRevisionRef: MutableRefObject<number | null>;
  orchestratorsRef: MutableRefObject<OrchestratorInstance[]>;
  seenServerInstanceIdsRef: MutableRefObject<Set<string>>;
  pendingRecoveryOpenSessionIdRef: MutableRefObject<string | undefined>;
  pendingRecoveryPaneIdRef: MutableRefObject<string | null | undefined>;
  pendingStateResyncOptionsRef: MutableRefObject<PendingStateResyncOptions | null>;
  publishQueuedSessionSlices: (sessionSnapshot?: Session[]) => void;
  queueSessionSliceForRender: (sessionId: string) => void;
  requestActionRecoveryResyncRef: MutableRefObject<
    (options?: {
      openSessionId?: string;
      paneId?: string | null;
      allowUnknownServerInstance?: boolean;
      sseReconnectRequestId?: number;
    }) => void
  >;
  requestBackendReconnectRef: MutableRefObject<() => void>;
  resetWorkspaceFilesChangedEventGate: () => void;
  scheduleCodexStateRender: () => void;
  scheduleSessionRender: () => void;
  sessionsRef: MutableRefObject<Session[]>;
  setBackendConnectionIssueDetail: Dispatch<SetStateAction<string | null>>;
  setBackendConnectionState: (next: BackendConnectionState) => void;
  setIsLoading: Dispatch<SetStateAction<boolean>>;
  setOrchestrators: Dispatch<SetStateAction<OrchestratorInstance[]>>;
  setSseEpoch: Dispatch<SetStateAction<number>>;
  sseEpoch: number;
  sseRecoveryAttemptRef: MutableRefObject<number>;
  sseRecoveryTimerRef: MutableRefObject<ReturnType<
    typeof window.setTimeout
  > | null>;
  startSessionHydration: (
    sessionId: string,
    options?: SessionHydrationOptions,
  ) => void;
  stateResyncInFlightRef: MutableRefObject<boolean>;
  stateResyncPendingRef: MutableRefObject<boolean>;
  syncAdoptedLiveSessionResumeWatchdogBaselinesRef: MutableRefObject<
    (sessions: Session[], now?: number) => void
  >;
  workspaceFilesChangedEventGateRefs: WorkspaceFilesChangedEventGateRefs;
};

export function useAppLiveStateTransport(
  params: UseAppLiveStateTransportParams,
) {
  const {
    adoptState,
    applyDelegationWaitDeltaLocally,
    cancelStaleSendResponseRecoveryPollForSessions,
    clearRecoveredBackendRequestError,
    codexStateRef,
    enqueueWorkspaceFilesChangedEvent,
    forceAdoptNextStateEventRef,
    hydrationRestartResyncPendingRef,
    laggedRecoveryBaselineRevisionRef,
    lastSeenServerInstanceIdRef,
    latestStateRevisionRef,
    orchestratorsRef,
    seenServerInstanceIdsRef,
    pendingRecoveryOpenSessionIdRef,
    pendingRecoveryPaneIdRef,
    pendingStateResyncOptionsRef,
    publishQueuedSessionSlices,
    queueSessionSliceForRender,
    requestActionRecoveryResyncRef,
    requestBackendReconnectRef,
    resetWorkspaceFilesChangedEventGate,
    scheduleCodexStateRender,
    scheduleSessionRender,
    sessionsRef,
    setBackendConnectionIssueDetail,
    setBackendConnectionState,
    setIsLoading,
    setOrchestrators,
    setSseEpoch,
    sseEpoch,
    sseRecoveryAttemptRef,
    sseRecoveryTimerRef,
    startSessionHydration,
    stateResyncInFlightRef,
    stateResyncPendingRef,
    syncAdoptedLiveSessionResumeWatchdogBaselinesRef,
    workspaceFilesChangedEventGateRefs,
  } = params;

  useEffect(() => {
    let cancelled = false;
    let initialStateResyncRetryTimeoutId: ReturnType<
      typeof window.setTimeout
    > | null = null;
    let reconnectStateResyncTimeoutId: ReturnType<
      typeof window.setTimeout
    > | null = null;
    let observedEventSourceOpen = false;
    const reconnectState = new ReconnectStateMachine();
    let nextReconnectStateResyncDelayMs = RECONNECT_STATE_RESYNC_DELAY_MS;
    let liveSessionResumeWatchdogIntervalId: ReturnType<
      typeof window.setInterval
    > | null = null;
    let shouldResyncOnResume = false;
    // These refs are component-scoped, so Strict Mode effect remounts must reset
    // any stale in-flight resync bookkeeping from the previous mount.
    stateResyncInFlightRef.current = false;
    stateResyncPendingRef.current = false;
    pendingStateResyncOptionsRef.current = null;
    pendingRecoveryOpenSessionIdRef.current = undefined;
    pendingRecoveryPaneIdRef.current = undefined;
    // Track transport activity per session so one noisy active session cannot
    // mask another stalled one.
    let lastLiveTransportActivityAtBySessionId = new Map<string, number>();
    // Drift-gap detection tracks a baseline per session so unrelated live traffic
    // cannot mask a wake gap for a different stalled active session.
    let lastLiveSessionResumeWatchdogTickAtBySessionId = new Map<
      string,
      number
    >();
    let lastWatchdogResyncAttemptAt: number | null = null;
    const eventSource = new EventSource("/api/events");

    function clearInitialStateResyncRetryTimeout() {
      if (initialStateResyncRetryTimeoutId === null) {
        return;
      }

      window.clearTimeout(initialStateResyncRetryTimeoutId);
      initialStateResyncRetryTimeoutId = null;
    }

    function clearReconnectStateResyncTimeout() {
      if (reconnectStateResyncTimeoutId === null) {
        return;
      }

      window.clearTimeout(reconnectStateResyncTimeoutId);
      reconnectStateResyncTimeoutId = null;
    }

    function resetReconnectStateResyncBackoff() {
      nextReconnectStateResyncDelayMs = RECONNECT_STATE_RESYNC_DELAY_MS;
    }

    /**
     * Schedules a fresh EventSource via `setSseEpoch`, with exponential
     * backoff (500 ms → 5 s cap). Idempotent — does nothing if a recovery
     * timer is already pending. Called from two paths:
     *   1. `onerror` when `readyState === 2` (CLOSED): the browser has
     *      permanently given up on the current socket.
     *   2. The periodic `handleSseHealthWatchdogTick` below: the socket has
     *      been non-OPEN for too long, e.g. a stuck CONNECTING state where
     *      `onerror` somehow stopped firing.
     * `onopen` resets `sseRecoveryAttemptRef` and clears any pending timer
     * so a healthy connection always starts the next failure cycle at the
     * lowest backoff.
     */
    function scheduleSseEventSourceRecovery() {
      if (sseRecoveryTimerRef.current !== null) {
        return;
      }
      const attempt = sseRecoveryAttemptRef.current;
      sseRecoveryAttemptRef.current = attempt + 1;
      const delayMs = Math.min(500 * 2 ** attempt, 5000);
      sseRecoveryTimerRef.current = window.setTimeout(() => {
        sseRecoveryTimerRef.current = null;
        if (cancelled) {
          return;
        }
        setSseEpoch((current) => current + 1);
      }, delayMs);
    }

    function consumeReconnectStateResyncDelayMs() {
      const delayMs = nextReconnectStateResyncDelayMs;
      nextReconnectStateResyncDelayMs = Math.min(
        nextReconnectStateResyncDelayMs * 2,
        RECONNECT_STATE_RESYNC_MAX_DELAY_MS,
      );
      return delayMs;
    }

    function scheduleFallbackStateResyncRetry(
      requestedRevision: number | null,
      options: {
        allowAuthoritativeRollback?: boolean;
        preserveReconnectFallback?: boolean;
        preserveWatchdogCooldown?: boolean;
      },
    ) {
      clearInitialStateResyncRetryTimeout();
      const delayMs = consumeReconnectStateResyncDelayMs();
      initialStateResyncRetryTimeoutId = window.setTimeout(() => {
        initialStateResyncRetryTimeoutId = null;
        if (
          cancelled ||
          !readNavigatorOnline() ||
          latestStateRevisionRef.current !== requestedRevision
        ) {
          return;
        }

        requestStateResync(options);
      }, delayMs);
    }

    function clearReconnectStateResyncTimeoutAfterConfirmedReopen({
      allowWithoutConfirmedOpen = false,
    }: { allowWithoutConfirmedOpen?: boolean } = {}) {
      // Call only from data-bearing SSE handlers. A bare EventSource `onopen`
      // means the socket handshook, not that fresh state is flowing again.
      if (
        !reconnectState.markRecoveryConfirmedAfterReopen({
          allowWithoutConfirmedOpen,
        })
      ) {
        return;
      }

      clearReconnectStateResyncTimeout();
      resetReconnectStateResyncBackoff();
    }

    function markEventSourceOpen() {
      observedEventSourceOpen = true;
      resetWorkspaceFilesChangedEventGate();
      reconnectState.onSseOpen();
      // Reset the post-CLOSED recovery counter and clear any pending
      // recovery timer — the live stream is healthy again, so the next
      // failure cycle should start fresh at the lowest backoff.
      sseRecoveryAttemptRef.current = 0;
      if (sseRecoveryTimerRef.current !== null) {
        window.clearTimeout(sseRecoveryTimerRef.current);
        sseRecoveryTimerRef.current = null;
      }
      if (latestStateRevisionRef.current !== null) {
        // A restarted backend can reconnect with the same persisted revision but a
        // more complete authoritative snapshot than the client currently has.
        forceAdoptNextStateEventRef.current = true;
      }
      setBackendConnectionState("connected");
      setBackendConnectionIssueDetail(null);
      clearRecoveredBackendRequestError();
    }

    function readEventSourceReadyState() {
      const readyState = (eventSource as { readyState?: unknown }).readyState;
      return typeof readyState === "number" ? readyState : null;
    }

    function eventSourceReadyStateIsOpen(
      readyState = readEventSourceReadyState(),
    ) {
      return readyState === 1;
    }

    function finishReconnectRecoveryConfirmation() {
      clearReconnectStateResyncTimeout();
      resetReconnectStateResyncBackoff();
      setBackendConnectionState("connected");
      return true;
    }

    function confirmReconnectRecoveryFromLiveEvent({
      allowWithoutConfirmedOpen = false,
    }: { allowWithoutConfirmedOpen?: boolean } = {}): boolean {
      if (
        !reconnectState.confirmLiveEvent({
          allowWithoutConfirmedOpen,
        })
      ) {
        return false;
      }

      return finishReconnectRecoveryConfirmation();
    }

    function confirmReconnectRecoveryFromDeltaEvent() {
      if (
        !reconnectState.confirmDeltaEvent({
          eventSourceReadyStateIsOpen: eventSourceReadyStateIsOpen(),
        })
      ) {
        return false;
      }

      return finishReconnectRecoveryConfirmation();
    }

    function confirmReconnectRecoveryFromStateEvent() {
      if (
        !reconnectState.confirmStateEvent({
          eventSourceReadyStateIsOpen: eventSourceReadyStateIsOpen(),
        })
      ) {
        return false;
      }

      return finishReconnectRecoveryConfirmation();
    }

    function confirmReconnectRecoveryFromAuthoritativeSnapshot() {
      reconnectState.confirmAuthoritativeSnapshot();
      finishReconnectRecoveryConfirmation();
    }

    function beginBadLiveEventRecovery() {
      reconnectState.onBadLiveEvent();
      setBackendConnectionState("reconnecting");
      if (reconnectStateResyncTimeoutId === null) {
        scheduleReconnectStateResync();
      }
    }

    /**
     * Arms the delayed `/api/state` reconnect poll. Manual retry uses
     * `rearmAfterSameInstanceProgressUntilLiveEvent` so a stale-then-fresh
     * same-instance recovery still waits for data-bearing SSE confirmation
     * before the reconnect loop stops.
     */
    function scheduleReconnectStateResync({
      rearmAfterSameInstanceProgressUntilLiveEvent = false,
      requestOptions,
    }: {
      rearmAfterSameInstanceProgressUntilLiveEvent?: boolean;
      requestOptions?: RequestStateResyncOptions;
    } = {}) {
      // Preserve any reopen proof until a real EventSource.onerror starts a new
      // outage cycle. Otherwise a bad post-reopen event can arm fallback polling
      // and strand the client in "reconnecting" even when later events on the
      // same socket are healthy.
      clearReconnectStateResyncTimeout();
      const delayMs = consumeReconnectStateResyncDelayMs();
      reconnectStateResyncTimeoutId = window.setTimeout(() => {
        reconnectStateResyncTimeoutId = null;
        if (
          cancelled ||
          !readNavigatorOnline() ||
          latestStateRevisionRef.current === null
        ) {
          return;
        }

        requestStateResync({
          allowAuthoritativeRollback: true,
          rearmOnSuccess: true,
          rearmUntilLiveEventOnSuccess: true,
          rearmAfterSameInstanceProgressUntilLiveEvent,
          rearmOnFailure: true,
          ...requestOptions,
        });
      }, delayMs);
    }

    function markLiveTransportActivity(
      sessionIds: Iterable<string>,
      now = Date.now(),
      {
        clearWatchdogCooldown = true,
      }: { clearWatchdogCooldown?: boolean } = {},
    ) {
      markLiveTransportSessionActivity(
        lastLiveTransportActivityAtBySessionId,
        sessionIds,
        now,
      );
      if (clearWatchdogCooldown) {
        lastWatchdogResyncAttemptAt = null;
      }
    }

    function syncLiveTransportActivityFromState(
      sessions: Session[],
      now = Date.now(),
      {
        clearWatchdogCooldown = true,
      }: { clearWatchdogCooldown?: boolean } = {},
    ) {
      syncLiveTransportSessionActivityFromState(
        lastLiveTransportActivityAtBySessionId,
        sessions,
        now,
      );
      if (clearWatchdogCooldown) {
        lastWatchdogResyncAttemptAt = null;
      }
    }

    function markLiveSessionResumeWatchdogBaseline(
      sessionIds: Iterable<string>,
      now = Date.now(),
    ) {
      markLiveSessionResumeWatchdogBaselineActivity(
        lastLiveSessionResumeWatchdogTickAtBySessionId,
        sessionIds,
        now,
      );
    }

    function syncLiveSessionResumeWatchdogBaselines(
      sessions: Session[],
      now = Date.now(),
    ) {
      syncLiveSessionResumeWatchdogBaselineActivity(
        lastLiveSessionResumeWatchdogTickAtBySessionId,
        sessions,
        now,
      );
    }
    syncAdoptedLiveSessionResumeWatchdogBaselinesRef.current =
      syncLiveSessionResumeWatchdogBaselines;

    function startStateResyncLoop() {
      if (
        cancelled ||
        stateResyncInFlightRef.current ||
        !stateResyncPendingRef.current
      ) {
        return;
      }

      stateResyncInFlightRef.current = true;
      void (async () => {
        try {
          while (!cancelled && stateResyncPendingRef.current) {
            stateResyncPendingRef.current = false;
            const {
              allowAuthoritativeRollback,
              allowUnknownServerInstance,
              preserveReconnectFallback,
              preserveWatchdogCooldown,
              rearmOnSuccess,
              rearmUntilLiveEventOnSuccess,
              rearmAfterSameInstanceProgressUntilLiveEvent,
              confirmReconnectRecoveryOnAdoption,
              forceAdoptEqualOrNewerRevision,
              sseReconnectRequestId,
              rearmOnFailure,
              openSessionId,
              paneId,
            } = consumePendingStateResyncOptions(pendingStateResyncOptionsRef);
            const requestedRevision = latestStateRevisionRef.current;
            const requestedServerInstanceId =
              lastSeenServerInstanceIdRef.current;

            try {
              const state = await fetchState();
              if (cancelled) {
                break;
              }

              const shouldPreferAuthoritativeSnapshot =
                preserveReconnectFallback ||
                preserveWatchdogCooldown ||
                rearmOnSuccess ||
                rearmOnFailure;
              const receivedReplacementInstance = isServerInstanceMismatch(
                requestedServerInstanceId,
                state.serverInstanceId,
              );
              const shouldConsiderAuthoritativeSnapshot =
                allowAuthoritativeRollback &&
                shouldPreferAuthoritativeSnapshot &&
                requestedRevision !== null &&
                latestStateRevisionRef.current === requestedRevision;
              const shouldConsiderTargetedRepairSnapshot =
                allowAuthoritativeRollback &&
                shouldPreferAuthoritativeSnapshot &&
                forceAdoptEqualOrNewerRevision !== null;
              const shouldTrustAuthoritativeReplacementInstance =
                shouldConsiderAuthoritativeSnapshot &&
                receivedReplacementInstance;
              const isEqualRevisionSnapshot =
                requestedRevision !== null &&
                state.revision === requestedRevision;
              const isNotNewerReplacementSnapshot =
                shouldTrustAuthoritativeReplacementInstance &&
                requestedRevision !== null &&
                state.revision <= requestedRevision;
              // Same-instance reconnect/fallback recovery is intentionally
              // restricted to equal-revision force-adopts. Same-server-instance
              // revisions are monotonic, so a lower `state.revision` than
              // `requestedRevision` cannot be authoritative without explicit
              // restart or replacement-instance evidence. Allowing
              // `state.revision <= requestedRevision` here previously could
              // roll the client backward after a newer SSE delta sequence
              // already advanced local state — for example: SSE recreates
              // after a backend restart, deltas stream the assistant reply,
              // local advances; the earlier-scheduled reconnect /api/state
              // probe finally returns at a revision strictly less than
              // current, force-adoption rolls back, and the just-rendered
              // assistant message disappears with no further deltas to
              // re-add it. Equal-revision adoption is still handled by
              // `isEqualRevisionSnapshot`; replacement-instance rollback is
              // still handled by `isNotNewerReplacementSnapshot`. See bugs.md
              // "Same-instance reconnect recovery can force-adopt lower
              // revision snapshots".
              const isEqualRevisionAutomaticReconnectSnapshot =
                shouldConsiderAuthoritativeSnapshot &&
                !receivedReplacementInstance &&
                requestedRevision !== null &&
                state.revision === requestedRevision &&
                (preserveReconnectFallback ||
                  (rearmOnSuccess &&
                    rearmUntilLiveEventOnSuccess &&
                    !rearmAfterSameInstanceProgressUntilLiveEvent));
              // This is currently subsumed by `isEqualRevisionSnapshot`
              // below because both require equal same-instance revisions.
              // Keep the named alias so the reconnect fallback's
              // stricter `===` contract remains visible next to the
              // watchdog branch that intentionally retains `<=`.
              // The watchdog branch deliberately retains `<=` (rather than
              // mirroring the reconnect path's `===`) because the watchdog
              // only fires after stale-live-transport detection. A
              // legitimate use case is when orchestrator-only deltas
              // advance the global revision counter without updating
              // session content (orchestrator deltas update `latestState
              // RevisionRef` but do not refresh active-session messages or
              // baselines for sessions absent from `delta.sessions`). The
              // watchdog fetches /api/state to recover the canonical
              // session content; that response carries the SESSION's
              // revision (lower than the orchestrator-bumped local
              // counter) and force-adopting it is the recovery mechanism.
              // The L197 hazard the reconnect path closed (a stale fetch
              // queued behind newer SSE assistant deltas) does NOT apply
              // here because session deltas update watchdog baselines via
              // `markLiveSessionResumeWatchdogBaseline`, so the watchdog
              // would not even fire while session-content deltas are
              // recent. See bugs.md "Watchdog recovery still allows lower
              // same-instance revisions for orchestrator-noise recovery".
              const shouldTrustWatchdogSnapshot =
                shouldConsiderAuthoritativeSnapshot &&
                preserveWatchdogCooldown &&
                !receivedReplacementInstance &&
                requestedRevision !== null &&
                state.revision <= requestedRevision;
              const shouldTrustTargetedEqualRevisionRepair =
                shouldConsiderTargetedRepairSnapshot &&
                !receivedReplacementInstance &&
                latestStateRevisionRef.current !== null &&
                state.revision === latestStateRevisionRef.current &&
                state.revision >= forceAdoptEqualOrNewerRevision;
              // Trusting a replacement instance lets newer restart snapshots
              // through the server-instance guard. Force/downgrade remains
              // limited to equal-revision snapshots, not-newer replacement
              // snapshots, and watchdog probes that intentionally ask
              // /api/state to repair a stale active session after unrelated
              // live deltas advanced the global revision gate.
              const shouldForceAuthoritativeSnapshot =
                shouldConsiderAuthoritativeSnapshot &&
                (isEqualRevisionSnapshot ||
                  isNotNewerReplacementSnapshot ||
                  isEqualRevisionAutomaticReconnectSnapshot ||
                  shouldTrustWatchdogSnapshot ||
                  shouldTrustTargetedEqualRevisionRepair);
              const shouldAllowUnknownServerInstance =
                allowUnknownServerInstance ||
                shouldTrustAuthoritativeReplacementInstance;

              const adopted = adoptState(state, {
                // A reconnect fallback snapshot is authoritative if no newer
                // SSE state landed while it was in flight. Same-instance
                // downgrades stay limited to automatic reconnect/fallback and
                // watchdog probes; manual retry still waits for catch-up.
                force: shouldForceAuthoritativeSnapshot,
                allowRevisionDowngrade: shouldForceAuthoritativeSnapshot,
                allowUnknownServerInstance: shouldAllowUnknownServerInstance,
                sseReconnectRequestId,
                openSessionId,
                paneId,
              });
              const shouldRetryStaleSameInstanceSnapshot =
                !adopted &&
                allowAuthoritativeRollback &&
                requestedRevision !== null &&
                latestStateRevisionRef.current === requestedRevision &&
                state.revision < requestedRevision &&
                !reconnectStateResyncTimeoutId;
              const shouldRetryTargetedEqualRevisionRepair =
                !adopted &&
                shouldConsiderTargetedRepairSnapshot &&
                !receivedReplacementInstance &&
                latestStateRevisionRef.current !== null &&
                state.revision >= forceAdoptEqualOrNewerRevision &&
                state.revision < latestStateRevisionRef.current;
              if (adopted) {
                clearInitialStateResyncRetryTimeout();
                if (
                  reconnectStateResyncTimeoutId !== null &&
                  reconnectState.recoveryConfirmedSinceLastError
                ) {
                  // Once live SSE data has confirmed recovery, an adopted
                  // snapshot can disarm any leftover reconnect fallback timer
                  // and reset the next reconnect cycle back to the fast
                  // initial delay.
                  clearReconnectStateResyncTimeout();
                  resetReconnectStateResyncBackoff();
                }
                const adoptedAt = Date.now();
                syncLiveTransportActivityFromState(state.sessions, adoptedAt, {
                  clearWatchdogCooldown: !preserveWatchdogCooldown,
                });
                pruneLiveTransportActivitySessions(
                  lastLiveTransportActivityAtBySessionId,
                  state.sessions,
                );
                syncLiveSessionResumeWatchdogBaselines(
                  state.sessions,
                  adoptedAt,
                );
                if (
                  !confirmReconnectRecoveryOnAdoption &&
                  !reconnectState.recoveryConfirmedSinceLastError
                ) {
                  reconnectState.markDelegationRepairAdoptedIfCoversRevision(
                    state.revision,
                    forceAdoptEqualOrNewerRevision,
                  );
                }
                if (confirmReconnectRecoveryOnAdoption) {
                  confirmReconnectRecoveryFromDeltaEvent();
                }
              } else if (shouldRetryTargetedEqualRevisionRepair) {
                scheduleReconnectStateResync({
                  requestOptions: {
                    allowAuthoritativeRollback: true,
                    confirmReconnectRecoveryOnAdoption,
                    forceAdoptEqualOrNewerRevision:
                      forceAdoptEqualOrNewerRevision ?? undefined,
                    rearmOnFailure: true,
                  },
                });
              } else if (shouldRetryStaleSameInstanceSnapshot) {
                // A same-instance snapshot that still lags behind the client's
                // locally adopted revision can arrive during create/fork wake-gap
                // recovery. Do not roll back to it, but keep probing until the
                // authoritative snapshot catches up.
                scheduleReconnectStateResync({
                  // Preserve manual-retry semantics only for probes that were
                  // already allowed to require live-SSE proof after same-instance
                  // progress; automatic reconnect probes keep the narrower guard.
                  rearmAfterSameInstanceProgressUntilLiveEvent,
                });
              }
              setBackendConnectionIssueDetail(null);
              clearRecoveredBackendRequestError();
              if (
                rearmOnSuccess &&
                !reconnectState.recoveryConfirmedSinceLastError &&
                reconnectStateResyncTimeoutId === null
              ) {
                const adoptedReplacementInstance =
                  adopted &&
                  isServerInstanceMismatch(
                    requestedServerInstanceId,
                    state.serverInstanceId,
                  );
                const adoptedSameInstanceProgress =
                  adopted &&
                  !adoptedReplacementInstance &&
                  requestedRevision !== null &&
                  state.revision > requestedRevision;
                if (
                  adoptedSameInstanceProgress &&
                  rearmAfterSameInstanceProgressUntilLiveEvent
                ) {
                  // Manual retry keeps polling until live SSE proves recovery.
                  // In real EventSource delivery, a data event implies an open
                  // socket; tests may omit `onopen`, so a later data frame can
                  // satisfy this specific manual-retry contract.
                  reconnectState.onManualRetrySameInstanceProgress();
                }
                // Timer-driven reconnect fallbacks keep probing until a live
                // EventSource data frame proves the SSE stream is healthy
                // again. A same-instance /api/state snapshot can refresh the
                // visible UI, but polling success alone does not prove later
                // assistant deltas will arrive.
                const shouldRearmUntilLiveEvent =
                  rearmUntilLiveEventOnSuccess &&
                  !reconnectState.recoveryConfirmedSinceLastError;
                const carryManualRetryContractForward =
                  rearmAfterSameInstanceProgressUntilLiveEvent &&
                  shouldRearmUntilLiveEvent;
                // Keep polling after success only for one of these contracts:
                // explicit live-SSE proof is still required, a bad reopened SSE
                // event needs recovery, no newer snapshot progress was made, or
                // a replacement instance was adopted and should be confirmed.
                const shouldRearmAfterSuccess =
                  shouldRearmUntilLiveEvent ||
                  (reconnectState.pendingBadLiveEventRecovery &&
                    !reconnectState.recoveryConfirmedSinceLastError) ||
                  (requestedRevision !== null &&
                    latestStateRevisionRef.current === requestedRevision) ||
                  adoptedReplacementInstance;
                if (shouldRearmAfterSuccess) {
                  scheduleReconnectStateResync({
                    // Carry the manual-retry contract forward only while this
                    // poll is still waiting for live-SSE proof.
                    rearmAfterSameInstanceProgressUntilLiveEvent:
                      carryManualRetryContractForward,
                  });
                } else if (adopted) {
                  // Non-reconnect resync paths that do not require live-SSE
                  // proof can clear the reconnect badge after adopting their
                  // authoritative snapshot. Timer-driven reconnect fallback
                  // requests keep polling through `shouldRearmUntilLiveEvent`.
                  confirmReconnectRecoveryFromAuthoritativeSnapshot();
                }
              }
            } catch (error) {
              if (!cancelled) {
                setBackendConnectionIssueDetail(
                  describeBackendConnectionIssueDetail(error),
                );
                const errorRequiresRestart =
                  isBackendUnavailableError(error) && error.restartRequired;
                if (errorRequiresRestart) {
                  // Incompatible backend serving HTML — retrying will produce
                  // the same result until the user restarts.
                } else if (
                  preserveReconnectFallback &&
                  reconnectStateResyncTimeoutId === null
                ) {
                  // Marked fallback snapshots can arrive without a preceding reconnect
                  // onerror, so transient /api/state failures need their own retry.
                  scheduleFallbackStateResyncRetry(requestedRevision, {
                    allowAuthoritativeRollback,
                    preserveReconnectFallback,
                    preserveWatchdogCooldown,
                  });
                } else if (
                  rearmOnFailure &&
                  reconnectStateResyncTimeoutId === null &&
                  requestedRevision !== null
                ) {
                  // Re-arm reconnect polling so a failed one-shot probe (e.g.
                  // manual retry) does not leave the client without any automatic
                  // recovery path until the next EventSource onerror fires.
                  scheduleReconnectStateResync({
                    rearmAfterSameInstanceProgressUntilLiveEvent,
                  });
                }
              }
              break;
            } finally {
              if (!cancelled) {
                setIsLoading(false);
              }
            }
          }
        } finally {
          if (!cancelled) {
            // Clear before restarting so startStateResyncLoop's entry guard passes.
            stateResyncInFlightRef.current = false;
            if (stateResyncPendingRef.current) {
              startStateResyncLoop();
            }
          }
        }
      })();
    }

    function requestStateResync(options?: RequestStateResyncOptions) {
      if (cancelled) {
        return;
      }

      if (
        reconnectState.recoveryConfirmedSinceLastError &&
        !options?.preserveReconnectFallback
      ) {
        clearReconnectStateResyncTimeout();
      }
      // Otherwise preserve the armed reconnect fallback; startStateResyncLoop()
      // will only disarm it once a /api/state snapshot is actually adopted.
      // Coalesced resync requests retain the strongest semantics until the next
      // loop iteration consumes them.
      pendingStateResyncOptionsRef.current = coalescePendingStateResyncOptions(
        pendingStateResyncOptionsRef.current,
        options,
      );
      stateResyncPendingRef.current = true;
      startStateResyncLoop();
    }

    function triggerRecoveryForDelta(
      delta: DeltaEvent,
      options?: {
        requestOptions?: RequestStateResyncOptions;
        hydrationOptions?: SessionHydrationOptions;
      },
    ) {
      requestStateResync(options?.requestOptions ?? { rearmOnFailure: true });
      if (isSessionDeltaEvent(delta)) {
        // Recovery-triggered hydration is intentionally limited only by the
        // in-flight/queued sets in `startSessionHydration`. Phase-1 transport is
        // local, and freshness is more important than adding a cooldown that
        // could defer the only full-transcript fetch after a problematic delta.
        // Revisit with a per-session cooldown if remote/flaky networks make
        // repeated completed hydrations expensive.
        startSessionHydration(delta.sessionId, options?.hydrationOptions);
      }
    }

    requestBackendReconnectRef.current = () => {
      if (cancelled || !readNavigatorOnline()) {
        return;
      }

      reconnectState.onManualRetry();
      clearInitialStateResyncRetryTimeout();
      clearReconnectStateResyncTimeout();
      resetReconnectStateResyncBackoff();
      // Manual retry is intentionally an immediate `/api/state` probe. It does
      // not preserve any currently armed reconnect timer, but it does keep
      // polling after snapshot progress until live SSE confirms recovery.
      requestStateResync({
        allowAuthoritativeRollback: latestStateRevisionRef.current !== null,
        rearmOnSuccess: true,
        rearmUntilLiveEventOnSuccess: true,
        rearmAfterSameInstanceProgressUntilLiveEvent: true,
        rearmOnFailure: true,
      });
    };
    requestActionRecoveryResyncRef.current = (options) => {
      if (cancelled || !readNavigatorOnline()) {
        // Preserve hydration restart intent across offline observations; the
        // next online recovery should still be allowed to adopt a replacement
        // server instance.
        return;
      }
      const allowUnknownServerInstance =
        options?.allowUnknownServerInstance === true ||
        hydrationRestartResyncPendingRef.current;
      hydrationRestartResyncPendingRef.current = false;

      // Action-error recovery is a plain one-shot `/api/state` probe. Unlike
      // the manual retry path it must NOT reset `sawReconnectOpenSinceLastError`
      // or re-arm reconnect polling, because the SSE stream may still be
      // healthy — only the individual request endpoint returned a transient
      // backend-unavailable error (e.g. a one-off 502). Touching SSE-level
      // state here would cause the successful probe to schedule reconnect
      // polling that never disarms until an unrelated EventSource onerror/onopen
      // cycle resets the flag.
      if (options?.openSessionId !== undefined) {
        pendingRecoveryOpenSessionIdRef.current = options.openSessionId;
        pendingRecoveryPaneIdRef.current = options.paneId ?? null;
      }
      requestStateResync({
        allowAuthoritativeRollback: latestStateRevisionRef.current !== null,
        allowUnknownServerInstance,
        // Explicit false is self-documentation; coalescing cannot lower an
        // already-queued stronger recovery request.
        rearmOnSuccess: false,
        rearmUntilLiveEventOnSuccess: false,
        sseReconnectRequestId: options?.sseReconnectRequestId,
        openSessionId: options?.openSessionId,
        paneId: options?.paneId,
      });
    };
    if (hydrationRestartResyncPendingRef.current) {
      requestActionRecoveryResyncRef.current();
    }

    function hasPotentiallyStaleLiveSession() {
      return sessionsRef.current.some((session) => session.status !== "idle");
    }

    function hasActivelyStreamingSession() {
      return sessionsRef.current.some((session) => session.status === "active");
    }

    function hasPotentiallyStaleTransportSession(now: number) {
      return sessionsRef.current.some((session) =>
        sessionHasPotentiallyStaleTransport(
          session,
          lastLiveTransportActivityAtBySessionId.get(session.id),
          now,
        ),
      );
    }

    function markResumeResyncIfNeeded() {
      if (latestStateRevisionRef.current === null) {
        return;
      }

      shouldResyncOnResume = hasPotentiallyStaleLiveSession();
    }

    function resumeStateIfNeeded() {
      if (
        cancelled ||
        !shouldResyncOnResume ||
        !readNavigatorOnline() ||
        latestStateRevisionRef.current === null
      ) {
        return;
      }

      shouldResyncOnResume = false;
      requestStateResync({
        allowAuthoritativeRollback: true,
        allowUnknownServerInstance: true,
      });
    }

    function handleLiveSessionResumeWatchdogTick() {
      const now = Date.now();
      if (cancelled) {
        return;
      }

      if (document.visibilityState === "hidden") {
        return;
      }

      if (
        !readNavigatorOnline() ||
        stateResyncInFlightRef.current ||
        stateResyncPendingRef.current
      ) {
        return;
      }

      const detectedResumeGap = sessionsRef.current.some((session) => {
        if (session.status !== "active") {
          return false;
        }

        const lastWatchdogTickAt =
          lastLiveSessionResumeWatchdogTickAtBySessionId.get(session.id) ?? now;
        return (
          now - lastWatchdogTickAt >= LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS
        );
      });
      // Only advance baselines when this tick is actually evaluating drift.
      // Pausing during in-flight/pending resyncs avoids consuming a real wake gap.
      syncLiveSessionResumeWatchdogBaselines(sessionsRef.current, now);
      if (
        latestStateRevisionRef.current === null ||
        !hasActivelyStreamingSession()
      ) {
        return;
      }

      // Wake-gap recovery stays broader than the stale-transport path so a first
      // assistant reply that finished while the machine was asleep can still recover,
      // even when queued follow-ups exist behind the active turn.
      const transportLooksStale = hasPotentiallyStaleTransportSession(now);
      if (!detectedResumeGap && !transportLooksStale) {
        return;
      }

      const watchdogRetryCooldownElapsed =
        lastWatchdogResyncAttemptAt === null ||
        now - lastWatchdogResyncAttemptAt >=
          LIVE_SESSION_WATCHDOG_RESYNC_RETRY_COOLDOWN_MS;
      if (!watchdogRetryCooldownElapsed) {
        return;
      }

      // A focused window can wake without blur/focus or visibility transitions.
      lastWatchdogResyncAttemptAt = now;
      requestStateResync({
        allowAuthoritativeRollback: true,
        allowUnknownServerInstance: true,
        preserveWatchdogCooldown: true,
        rearmUntilLiveEventOnSuccess: false,
      });
    }

    function shouldForceAdoptNextStateEvent() {
      if (!forceAdoptNextStateEventRef.current) {
        return false;
      }
      const laggedBaselineRevision = laggedRecoveryBaselineRevisionRef.current;
      return (
        laggedBaselineRevision === null ||
        latestStateRevisionRef.current === laggedBaselineRevision
      );
    }

    function clearForceAdoptNextStateEvent() {
      forceAdoptNextStateEventRef.current = false;
      laggedRecoveryBaselineRevisionRef.current = null;
    }

    const {
      handleStateEvent,
      handleDeltaEvent,
      handleWorkspaceFilesChangedEvent,
      handleLaggedEvent,
    } = createAppLiveStateTransportEventHandlers({
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
      isCancelled: () => cancelled,
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
      setLastDelegationRepairRequestedRevision: (revision: number) => {
        reconnectState.setLastDelegationRepairRequestedRevision(revision);
      },
      startSessionHydration,
      transportState: reconnectState,
      clearInitialStateResyncRetryTimeout,
      shouldForceAdoptNextStateEvent,
      syncLiveSessionResumeWatchdogBaselines,
      syncLiveTransportActivityFromState,
      triggerRecoveryForDelta,
    });

    eventSource.addEventListener("state", handleStateEvent as EventListener);
    eventSource.addEventListener("delta", handleDeltaEvent as EventListener);
    eventSource.addEventListener("lagged", handleLaggedEvent as EventListener);
    eventSource.addEventListener(
      "workspaceFilesChanged",
      handleWorkspaceFilesChangedEvent as EventListener,
    );
    eventSource.onopen = () => {
      if (!cancelled) {
        markEventSourceOpen();
      }
    };
    eventSource.onerror = () => {
      if (cancelled) {
        return;
      }

      const { hadReconnectOpenSinceLastError } = reconnectState.onSseError();
      observedEventSourceOpen = false;
      clearForceAdoptNextStateEvent();
      const readyState = readEventSourceReadyState();
      if (eventSourceReadyStateIsOpen(readyState)) {
        markEventSourceOpen();
        return;
      }
      const isOnline = readNavigatorOnline();
      const hasHydratedState = latestStateRevisionRef.current !== null;
      setBackendConnectionState(
        isOnline
          ? hasHydratedState
            ? "reconnecting"
            : "connecting"
          : "offline",
      );

      // EventSource permanently closed (`readyState === CLOSED`, numeric
      // value 2 per the WHATWG spec)? The browser is done with this socket
      // and will not auto-reconnect. This is what happens when the dev-mode
      // Vite proxy returns 502 during the brief backend-restart gap, and is
      // also seen with some browsers on certain clean stream ends. Schedule
      // a fresh EventSource via `setSseEpoch` re-running this effect, with
      // backoff to avoid hammering the proxy. Numeric `2` instead of
      // `EventSource.CLOSED`: tests stub the global `EventSource` with
      // `EventSourceMock`, so `EventSource.CLOSED` is `undefined`.
      if (readyState === 2 && isOnline) {
        scheduleSseEventSourceRecovery();
      }

      if (!isOnline) {
        clearInitialStateResyncRetryTimeout();
        clearReconnectStateResyncTimeout();
        resetReconnectStateResyncBackoff();
        return;
      }

      if (!hasHydratedState) {
        requestStateResync();
        return;
      }

      // Prefer the SSE reconnect snapshot when it arrives quickly, but fall back to /api/state
      // so completed assistant replies do not stay hidden until another user action forces a refresh.
      if (hadReconnectOpenSinceLastError) {
        // The stream had reopened since the previous error, so this is a new
        // failure cycle. Discard any stale timer and start fresh at the fast
        // initial delay.
        clearReconnectStateResyncTimeout();
        resetReconnectStateResyncBackoff();
        scheduleReconnectStateResync();
      } else if (reconnectStateResyncTimeoutId === null) {
        // Repeated error during the same outage — only schedule if no
        // fallback timer is already pending so the exponential backoff
        // is preserved.
        scheduleReconnectStateResync();
      }
    };
    liveSessionResumeWatchdogIntervalId = window.setInterval(
      handleLiveSessionResumeWatchdogTick,
      LIVE_SESSION_RESUME_WATCHDOG_INTERVAL_MS,
    );

    // Defense in depth: even though `onerror` schedules recovery for the
    // common cases (browser-emitted error events on each failed retry),
    // some networks / browsers can leave the EventSource stuck in
    // `readyState === CONNECTING` (0) without firing `onerror` for long
    // stretches. The periodic watchdog notices "we've been non-OPEN for
    // too long" and forces an EventSource recreation through the same
    // backoff path the `readyState === CLOSED` branch uses. Threshold of
    // 5 s leaves room for one normal browser auto-reconnect attempt while
    // avoiding the "stale until hard refresh" feel during local restarts. See bugs.md
    // "Browser auto-reconnect gives up after a non-200 SSE response and
    // the client gets stuck".
    let sseStaleSinceMs: number | null = null;
    const sseHealthWatchdogIntervalId = window.setInterval(() => {
      if (cancelled) {
        return;
      }
      if (!readNavigatorOnline()) {
        sseStaleSinceMs = null;
        return;
      }

      const readyState = readEventSourceReadyState();
      if (eventSourceReadyStateIsOpen(readyState)) {
        sseStaleSinceMs = null;
        if (
          !observedEventSourceOpen &&
          !reconnectState.pendingBadLiveEventRecovery
        ) {
          markEventSourceOpen();
        }
        return;
      }
      observedEventSourceOpen = false;
      if (readyState === null) {
        // The mock used in tests leaves `readyState` undefined unless a
        // test explicitly opts in. Treat that as "watchdog inert" so
        // existing test scenarios are not perturbed.
        return;
      }

      const now = Date.now();
      if (sseStaleSinceMs === null) {
        sseStaleSinceMs = now;
        return;
      }
      if (now - sseStaleSinceMs >= 5000) {
        sseStaleSinceMs = null;
        scheduleSseEventSourceRecovery();
      }
    }, 5000);

    function handleWindowBlur() {
      markResumeResyncIfNeeded();
    }

    function handlePageHide() {
      markResumeResyncIfNeeded();
    }

    function handlePageShow() {
      resumeStateIfNeeded();
    }

    function handleWindowFocus() {
      if (document.visibilityState === "hidden") {
        return;
      }

      resumeStateIfNeeded();
    }

    function handleVisibilityChange() {
      if (document.visibilityState === "hidden") {
        markResumeResyncIfNeeded();
        return;
      }

      resumeStateIfNeeded();
    }

    window.addEventListener("blur", handleWindowBlur);
    window.addEventListener("focus", handleWindowFocus);
    window.addEventListener("pagehide", handlePageHide);
    window.addEventListener("pageshow", handlePageShow);
    document.addEventListener("visibilitychange", handleVisibilityChange);

    return () => {
      cancelled = true;
      requestBackendReconnectRef.current = () => {};
      requestActionRecoveryResyncRef.current = () => {};
      syncAdoptedLiveSessionResumeWatchdogBaselinesRef.current = () => {};
      clearInitialStateResyncRetryTimeout();
      clearReconnectStateResyncTimeout();
      clearForceAdoptNextStateEvent();
      clearWorkspaceFilesChangedEventBuffer(workspaceFilesChangedEventGateRefs);
      if (liveSessionResumeWatchdogIntervalId !== null) {
        window.clearInterval(liveSessionResumeWatchdogIntervalId);
      }
      window.clearInterval(sseHealthWatchdogIntervalId);
      window.removeEventListener("blur", handleWindowBlur);
      window.removeEventListener("focus", handleWindowFocus);
      window.removeEventListener("pagehide", handlePageHide);
      window.removeEventListener("pageshow", handlePageShow);
      document.removeEventListener("visibilitychange", handleVisibilityChange);
      eventSource.removeEventListener(
        "state",
        handleStateEvent as EventListener,
      );
      eventSource.removeEventListener(
        "delta",
        handleDeltaEvent as EventListener,
      );
      eventSource.removeEventListener(
        "lagged",
        handleLaggedEvent as EventListener,
      );
      eventSource.removeEventListener(
        "workspaceFilesChanged",
        handleWorkspaceFilesChangedEvent as EventListener,
      );
      eventSource.close();
      // The recovery timer fires `setSseEpoch` to re-run this effect; if the
      // effect is being cleaned up for any other reason (component unmount,
      // explicit re-mount via state change), drop the pending bump so we
      // don't churn after teardown.
      if (sseRecoveryTimerRef.current !== null) {
        window.clearTimeout(sseRecoveryTimerRef.current);
        sseRecoveryTimerRef.current = null;
      }
    };
    // `sseEpoch` re-runs the effect to recreate a permanently-CLOSED
    // EventSource (see the `onerror` handler). All other deps are read via
    // refs by design — the effect installs every closure-captured handler
    // on mount and resets them on cleanup.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sseEpoch]);
}
