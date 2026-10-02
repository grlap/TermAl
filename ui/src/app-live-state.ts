// app-live-state.ts
//
// Owns: the live-state adoption helpers and hydration orchestration
// that used to live inline in App.tsx. That includes `adoptState`,
// `adoptSessions`, `adoptCreatedSessionResponse`,
// `adoptFetchedSession`, `syncPreferencesFromState`, the
// workspace-files-changed React state that consumes the extracted
// buffering gate from app-live-state-workspace-events, the
// `forceAdoptNextStateEventRef` refresh flag, the session hydration
// fetch effect, and the `hydratedSessionIdsRef` /
// `hydratingSessionIdsRef` tracking refs.
//
// The EventSource lifecycle, reconnect fallback timers, live-session
// watchdog, visibility/focus recovery handlers, and per-mount
// state-resync bookkeeping refs are delegated to
// app-live-state-transport.ts. The `workspaceFilesChangedEvent`
// React state + setter still live here — consumers in App.tsx read
// them via the hook return value.
//
// Does not own: shell/UI-level state unrelated to transport
// (workspace/session list state not produced by state adoption,
// dialog state, drag/resize state), the request-error
// presentation state (`requestError`, the toast + inline markers)
// which `reportRequestError` still manages from App.tsx, the
// backend-connection indicator element itself, and the
// `handleRetryBackendConnection` / `handleBrowserOnline` /
// `handleBrowserOffline` helpers (App.tsx still owns those —
// they call through to this hook via the two invoker refs for
// the actual reconnect).
//
// `requestBackendReconnectRef` and `requestActionRecoveryResyncRef`
// are owned by App.tsx and passed in as params. The hook's
// transport useEffect populates them on mount and resets them
// to no-ops on cleanup. App.tsx owns the ref identity because
// `reportRequestError` and `handleRetryBackendConnection` are
// declared before this hook is invoked and need stable ref
// handles to call through to — the alternative (returning them
// from the hook) would force forward-declaration gymnastics in
// App.tsx.
//
// Bounded history request admission, fetch/merge dispatch, and demand
// completion are delegated to session-history-loading.ts. This hook retains
// hydration scheduling and publishes accepted session records.
//
// Split out of: ui/src/App.tsx (Slice 13A + 13B of the
// App-split plan, see docs/app-split-plan.md). Slice 13B moved
// the EventSource lifecycle, reconnect/watchdog timers, and
// visibility handlers here; the `transportCoordinationRef`
// bridge from 13A collapsed into direct closure references
// because the hook's handlers now live in the same useEffect
// as the coordination helpers they invoke.

import {
  startTransition,
  useCallback,
  useEffect,
  useRef,
  useState,
  type MutableRefObject,
} from "react";
import {
  noteSessionTailAdopted,
} from "./session-hydration-performance";
import {
  SESSION_HISTORY_PAGE_MESSAGE_COUNT,
  SESSION_TAIL_WINDOW_MESSAGE_COUNT,
} from "./session-tail-policy";
import { ApiRequestError } from "./api-request";
import { repairSessionTailFromHistoryPage } from "./session-history";
import { decideHttpSessionAdoption, applyHttpSessionEffects } from "./transcript-http-adoption";
import {
  fetchSessionHistory,
  fetchSessionTail,
  type CreateSessionResponse,
  type DelegationWaitRecord,
  type StateResponse,
} from "./api";
import {
  areRemoteConfigsEqual,
  areTelegramUiConfigsEqual,
  resolveAppPreferences,
} from "./session-model-utils";
import { resolveAdoptedStateSlices } from "./state-adoption";
import { applyTestRunDelta, reconcileTestRunSnapshot, type TestRunSummary } from "./test-runs";
import { applyTestRunWaitDelta, type TestRunWaitRecord } from "./test-run-waits";
import { ALL_PROJECTS_FILTER_ID } from "./project-filters";
import {
  classifyDeltaServerIdentity,
  isServerInstanceMismatch,
  shouldAdoptSnapshotRevision,
} from "./state-revision";
import { type PendingStateResyncOptions } from "./app-live-state-resync-options";
import {
  applyDelegationWaitConsumed,
  applyDelegationWaitCreated,
  areDelegationWaitRecordsEqual,
} from "./app-live-state-delegation-waits";
import {
  buildUnknownModelConfirmationKeySet,
  setContainsOnlyValuesFrom,
} from "./app-live-state-model-confirmations";
import {
  advancePartialTailAppendProof,
  getHydrationMessageCount,
  getHydrationMutationStamp,
  samePartialTailProjection,
  type AdoptFetchedSessionOutcome,
  type HydrationDeltaObservation,
  type PartialTailAppendProof,
  type SessionHydrationRequestContext,
} from "./session-hydration-adoption";
import {
  loadBoundedSessionHistoryWindow,
  loadOlderHistoryPageOnce,
  type OlderHistoryLoadRegistry,
  type SessionHistoryLoadingContext,
} from "./session-history-loading";
import {
  applyDelegationParentIdsFromSummaries,
  reconcileSingleSession,
} from "./session-reconcile";
import {
  openSessionInWorkspaceState,
  reconcileWorkspaceState,
} from "./workspace";
import type { ControlPanelSide } from "./workspace-storage";
import type {
  AgentReadiness,
  CodexState,
  DeltaEvent,
  OrchestratorInstance,
  Project,
  RemoteConfig,
  Session,
  StateSessionSummary,
  WorkspaceFilesChangedEvent,
} from "./types";

import {
  pruneSessionAttachmentValues,
  pruneSessionCommandValues,
  pruneSessionFlags,
  pruneSessionFlagsWithInvalidation,
  pruneSessionValues,
  type DraftImageAttachment,
  type SessionAgentCommandMap,
  type SessionFlagMap,
} from "./app-utils";
import type { WorkspaceLayoutSummary } from "./api";
import type { BackendConnectionState } from "./backend-connection";
import {
  type PendingSessionRename,
  type SessionErrorMap,
  type SessionNoticeMap,
} from "./app-shell-internals";
import type {
  AdoptCreatedSessionOutcome,
  AdoptSessionsOptions,
  AdoptStateOptions,
  SessionHydrationTarget,
  UseAppLiveStateParams,
  UseAppLiveStateReturn,
} from "./app-live-state-types";
import {
  resolveAdoptStateSessionOptions,
  shouldRequestSessionTailRead,
  releaseSessionHydrationFlight,
  SESSION_HYDRATION_MAX_RETRY_ATTEMPTS,
  SESSION_HYDRATION_RETRY_DELAYS_MS,
  type SessionHydrationOptions,
} from "./app-live-state-hydration";
import {
  addSessionHistoryPageDemandListener,
  resumeSessionHistoryDemandsAfterStateAdoption,
} from "./session-history-demand";
import {
  enqueueWorkspaceFilesChangedEvent as enqueueWorkspaceFilesChangedEventInGate,
  flushWorkspaceFilesChangedEventBuffer as flushWorkspaceFilesChangedEventGateBuffer,
  resetWorkspaceFilesChangedEventGate as resetWorkspaceFilesChangedEventGateRefs,
  type WorkspaceFilesChangedEventGateRefs,
} from "./app-live-state-workspace-events";
import { useAppLiveStateRenderSchedulers } from "./app-live-state-render-schedulers";
import { useAppLiveStateTransport } from "./app-live-state-transport";
import { reconcileAdoptedSessionsWorkspace } from "./app-live-state-workspace-reconciliation";
import { WaitDeltaWatermark } from "./wait-delta-watermark";

function rememberServerInstanceId(
  seenServerInstanceIdsRef: MutableRefObject<Set<string>>,
  serverInstanceId: string | null | undefined,
) {
  if (serverInstanceId) {
    seenServerInstanceIdsRef.current.add(serverInstanceId);
  }
}

export function useAppLiveState(
  params: UseAppLiveStateParams,
): UseAppLiveStateReturn {
  const {
    adoptionRefs,
    stateSetters,
    preferenceSetters,
    applyControlPanelLayout,
    clearRecoveredBackendRequestError,
    reportRequestError,
    requestBackendReconnectRef,
    requestActionRecoveryResyncRef,
    activeSession,
    activeTranscriptSessionId,
    visibleSessionHydrationTargets,
  } = params;
  const {
    isMountedRef,
    latestStateRevisionRef,
    lastSeenServerInstanceIdRef,
    seenServerInstanceIdsRef,
    sessionsRef,
    draftsBySessionIdRef,
    draftAttachmentsBySessionIdRef,
    codexStateRef,
    agentReadinessRef,
    projectsRef,
    orchestratorsRef,
    delegationWaitsRef,
    workspaceSummariesRef,
    refreshingAgentCommandSessionIdsRef,
    confirmedUnknownModelSendsRef,
    activePromptPollCancelRef,
    activePromptPollSessionIdRef,
  } = adoptionRefs;
  const {
    setWorkspace,
    setCodexState,
    setAgentReadiness,
    setProjects,
    setOrchestrators,
    setDelegationWaits,
    setDelegationChildSessionIds,
    setPendingEngramMcpRevocationSessionIds = () => undefined,
    setWorkspaceSummaries,
    setDraftsBySessionId,
    setDraftAttachmentsBySessionId,
    setSendingSessionIds,
    setStoppingSessionIds,
    setKillingSessionIds,
    setKillRevealSessionId,
    setPendingKillSessionId,
    setPendingSessionRename,
    setUpdatingSessionIds,
    setAgentCommandsBySessionId,
    setRefreshingAgentCommandSessionIds,
    setAgentCommandErrors,
    setSessionSettingNotices,
    setSelectedProjectId,
    setIsLoading,
    setHasAdoptedStateSnapshot,
    setBackendConnectionIssueDetail,
    setBackendConnectionState,
  } = stateSetters;
  const {
    setDefaultCodexModel,
    setDefaultCodexSandboxMode,
    setDefaultCodexApprovalPolicy,
    setDefaultClaudeModel,
    setDefaultCursorModel,
    setDefaultGeminiModel,
    setDefaultKimiModel,
    setDefaultKimiApprovalMode,
    setDefaultKimiEffort,
    setDefaultOpenCodeModel,
    setDefaultOpenCodeApprovalMode,
    setDefaultCodexReasoningEffort,
    setDefaultClaudeApprovalMode,
    setDefaultClaudeEffort,
    setRemoteConfigs,
    setTelegramConfig,
    setEngramHostSettings,
  } = preferenceSetters;

  const hydratingSessionIdsRef = useRef<Set<string>>(new Set());
  const [testRuns, setTestRuns] = useState<TestRunSummary[]>([]);
  const [testRunWaits, setTestRunWaits] = useState<readonly TestRunWaitRecord[]>([]);
  const [testRunWaitFailures, setTestRunWaitFailures] = useState<import("./test-run-waits").TestRunWaitFailures>({});
  const seenTestRunWaitFailuresRef = useRef(new Map<string, import("./test-run-waits").TestRunWaitFailure>());
  const testRunWaitWatermarkRef = useRef(new WaitDeltaWatermark());
  const delegationWaitWatermarkRef = useRef(new WaitDeltaWatermark());
  const dismissTestRunWaitFailure = useCallback((sessionId: string) => {
    setTestRunWaitFailures(current => {
      const next = { ...current };
      delete next[sessionId];
      return next;
    });
  }, []);
  const partialTailAppendProofsRef = useRef(
    new Map<string, PartialTailAppendProof>(),
  );
  const activeSessionIdRef = useRef(activeSession?.id ?? null);
  activeSessionIdRef.current = activeSession?.id ?? null;
  const activeTranscriptSessionIdRef = useRef(activeTranscriptSessionId);
  activeTranscriptSessionIdRef.current = activeTranscriptSessionId;
  const hydratedSessionIdsRef = useRef<Set<string>>(new Set());
  // Records sessions whose current browser state already includes an
  // authoritative recent tail. A partial transcript can remain
  // `messagesLoaded: false` indefinitely without triggering another automatic
  // page; explicit history demand still loads one older page at a time.
  const tailLoadedSessionIdsRef = useRef<Set<string>>(new Set());
  // SSE handlers live across renders; recovery must use current pane demand.
  const visibleHydrationSessionIdsRef = useRef<Set<string>>(new Set());
  visibleHydrationSessionIdsRef.current = new Set(
    visibleSessionHydrationTargets.map(target => target.id),
  );
  if (activeSession) visibleHydrationSessionIdsRef.current.add(activeSession.id);
  const hydrationMismatchSessionIdsRef = useRef<Set<string>>(new Set());
  const queuedHydrationSessionIdsRef = useRef<Set<string>>(new Set());
  const queuedTailRepairSessionIdsRef = useRef<Set<string>>(new Set());
  const queuedTextRepairHydrationSessionIdsRef = useRef<Set<string>>(new Set());
  const olderHistoryLoadsRef = useRef<OlderHistoryLoadRegistry>(new Map());
  const lastFullStateServerInstanceIdRef = useRef<string | null>(
    lastSeenServerInstanceIdRef.current,
  );
  const hydrationRestartResyncPendingRef = useRef(false);
  const hydrationAfterStateResyncSessionIdsRef = useRef(new Set<string>());
  const hydrationRetryTimersRef = useRef<Map<string, number>>(new Map());
  const hydrationRetryAttemptsRef = useRef<Map<string, number>>(new Map());
  const hydrationCappedRetryAttemptsRef = useRef<Map<string, number>>(
    new Map(),
  );
  const forceAdoptNextStateEventRef = useRef(false);
  const laggedRecoveryBaselineRevisionRef = useRef<number | null>(null);

  const [workspaceFilesChangedEvent, setWorkspaceFilesChangedEvent] =
    useState<WorkspaceFilesChangedEvent | null>(null);
  // Bumped from inside the SSE useEffect's `onerror` handler when the
  // browser has permanently closed the EventSource (`readyState === CLOSED`).
  // The browser only auto-reconnects after a 200-ending-normally response or
  // a network error — non-200 status codes (the dev-mode Vite proxy returns
  // 502 during the backend-restart gap, and some browsers also close on
  // unexpected stream ends) leave the EventSource dead. Bumping this epoch
  // re-runs the transport effect, which closes the dead EventSource via the
  // cleanup function and then constructs a fresh one in the effect body.
  // Without this, after a backend restart the user has to hard-refresh to
  // re-establish the live stream — see bugs.md "Browser auto-reconnect
  // gives up after a non-200 SSE response and the client gets stuck".
  const [sseEpoch, setSseEpoch] = useState(0);
  const sseRecoveryAttemptRef = useRef(0);
  const sseRecoveryTimerRef = useRef<ReturnType<
    typeof window.setTimeout
  > | null>(null);
  // Set by `forceSseReconnect` (e.g. from `handleSend` after detecting a
  // server-restart mid-request). Any later `adoptState` call that observes a
  // `fullStateServerInstanceChanged` flip consumes it and recreates SSE. The
  // returned request token only scopes same-instance false-alarm cleanup.
  // Setting `setSseEpoch` synchronously inside `forceSseReconnect` would
  // race with an in-flight `/api/state` probe scheduled by the same
  // caller — the effect cleanup sets `cancelled = true` and the probe's
  // await callback bails before the recovered state is applied.
  const nextSseReconnectRequestIdRef = useRef(1);
  const pendingSseRecreateOnInstanceChangeRef = useRef<{
    requestId: number;
  } | null>(null);
  const workspaceFilesChangedEventBufferRef =
    useRef<WorkspaceFilesChangedEvent | null>(null);
  const workspaceFilesChangedEventFlushTimeoutRef = useRef<number | null>(null);
  const lastWorkspaceFilesChangedRevisionRef = useRef<number | null>(null);
  const workspaceFilesChangedEventGateRefs: WorkspaceFilesChangedEventGateRefs =
    {
      bufferRef: workspaceFilesChangedEventBufferRef,
      flushTimeoutRef: workspaceFilesChangedEventFlushTimeoutRef,
      lastRevisionRef: lastWorkspaceFilesChangedRevisionRef,
    };
  // State-resync refs are kept at the hook body so the
  // transport useEffect can reset them on Strict Mode remount
  // without losing the per-mount cleanup identity.
  const stateResyncInFlightRef = useRef(false);
  const stateResyncPendingRef = useRef(false);
  const pendingStateResyncOptionsRef = useRef<PendingStateResyncOptions | null>(
    null,
  );
  const pendingRecoveryOpenSessionIdRef = useRef<string | undefined>(undefined);
  const pendingRecoveryPaneIdRef = useRef<string | null | undefined>(undefined);
  // Bridges `adoptState` (declared at the hook body so the
  // React render can call it) with the watchdog baseline
  // sync (defined inside the transport useEffect). Assigned on
  // mount, reset to a no-op on cleanup.
  const syncAdoptedLiveSessionResumeWatchdogBaselinesRef = useRef<
    (sessions: readonly Session[], now?: number) => void
  >(() => {});
  const {
    cancelPendingCodexStateRender,
    flushAndCancelPendingSessionRender,
    publishQueuedSessionSlices,
    queueSessionSliceForRender,
    scheduleCodexStateRender,
    scheduleSessionRender,
  } = useAppLiveStateRenderSchedulers({
    codexStateRef,
    draftAttachmentsBySessionIdRef,
    draftsBySessionIdRef,
    isMountedRef,
    sessionsRef,
    setCodexState,
    sessionAuthority: params.sessionAuthority,
  });

  function upsertSessionSlice(session: Session) {
    params.sessionAuthority.syncSlices([session.id]);
  }

  function clearHydrationRetry(sessionId: string) {
    const timerId = hydrationRetryTimersRef.current.get(sessionId);
    if (timerId !== undefined) {
      window.clearTimeout(timerId);
      hydrationRetryTimersRef.current.delete(sessionId);
    }
    hydrationRetryAttemptsRef.current.delete(sessionId);
    hydrationCappedRetryAttemptsRef.current.delete(sessionId);
  }

  function completeSessionHydration(sessionId: string) {
    clearHydrationRetry(sessionId);
    hydratedSessionIdsRef.current.add(sessionId);
  }

  function cancelHydrationRetries() {
    for (const timerId of hydrationRetryTimersRef.current.values()) {
      window.clearTimeout(timerId);
    }
    hydrationRetryTimersRef.current.clear();
    hydrationRetryAttemptsRef.current.clear();
    hydrationCappedRetryAttemptsRef.current.clear();
  }

  function invalidatePartialTailProof(sessionId?: string) {
    if (sessionId !== undefined) {
      const proof = partialTailAppendProofsRef.current.get(sessionId);
      if (proof) proof.valid = false;
      return;
    }
    for (const proof of partialTailAppendProofsRef.current.values()) {
      proof.valid = false;
    }
  }

  function hasPartialTailAppendProof(sessionId?: string) {
    // Read the live map synchronously, never a render-time snapshot. Transport
    // checks and observes in the same call stack; absent ids mean global fences.
    return sessionId === undefined
      ? partialTailAppendProofsRef.current.size > 0
      : partialTailAppendProofsRef.current.has(sessionId);
  }

  function observeHydrationDelta(observation: HydrationDeltaObservation) {
    const { delta } = observation;
    if (!delta || observation.revisionAction === "resync") {
      invalidatePartialTailProof();
    } else if ("sessionId" in delta) {
      const proof = partialTailAppendProofsRef.current.get(delta.sessionId);
      if (proof) advancePartialTailAppendProof(proof, observation);
    } else if (delta.type === "orchestratorsUpdated") {
      for (const session of delta.sessions ?? []) {
        invalidatePartialTailProof(session.id);
      }
    }
  }

  function clearHydrationMismatchSessionIds(sessionIds: Iterable<string>) {
    for (const sessionId of sessionIds) {
      hydrationMismatchSessionIdsRef.current.delete(sessionId);
    }
  }

  function sessionStillNeedsHydration(sessionId: string) {
    return sessionsRef.current.some(session =>
      session.id === sessionId && session.messagesLoaded === false);
  }

  function shouldStartTailLoad(
    sessionId: string,
    options?: SessionHydrationOptions,
  ) {
    if (options?.allowDivergentTextRepairAfterNewerRevision === true) {
      return false;
    }
    const session = sessionsRef.current.find((entry) => entry.id === sessionId);
    if (!session) {
      return false;
    }
    if (options?.forceTailRepair === true) {
      return true;
    }
    if (session.messagesLoaded !== false) {
      return false;
    }
    return (
      !tailLoadedSessionIdsRef.current.has(sessionId)
    );
  }

  function scheduleHydrationRetry(
    sessionId: string,
    options: { capAttempts?: boolean; ownerTailRead?: boolean } = {},
  ) {
    if (
      !isMountedRef.current ||
      hydrationRetryTimersRef.current.has(sessionId) ||
      !(options.ownerTailRead === true
        ? params.sessionAuthority.needsTailRead(sessionId, visibleHydrationSessionIdsRef.current.has(sessionId))
        : sessionStillNeedsHydration(sessionId))
    ) {
      return;
    }

    const attempt = hydrationRetryAttemptsRef.current.get(sessionId) ?? 0;
    if (options.capAttempts === true) {
      const cappedAttempt =
        hydrationCappedRetryAttemptsRef.current.get(sessionId) ?? 0;
      if (cappedAttempt >= SESSION_HYDRATION_MAX_RETRY_ATTEMPTS) {
        return;
      }
      hydrationCappedRetryAttemptsRef.current.set(sessionId, cappedAttempt + 1);
    }
    const delayMs =
      SESSION_HYDRATION_RETRY_DELAYS_MS[
        Math.min(attempt, SESSION_HYDRATION_MAX_RETRY_ATTEMPTS - 1)
      ];
    hydrationRetryAttemptsRef.current.set(sessionId, attempt + 1);
    const timerId = window.setTimeout(() => {
      hydrationRetryTimersRef.current.delete(sessionId);
      if (!isMountedRef.current) {
        return;
      }
      if (options.ownerTailRead !== true && sessionStillNeedsHydration(sessionId)) {
        startSessionHydration(sessionId);
      }
      requestSessionTailRead(sessionId);
    }, delayMs);
    hydrationRetryTimersRef.current.set(sessionId, timerId);
  }

  useEffect(() => {
    return () => {
      cancelHydrationRetries();
      invalidatePartialTailProof();
      partialTailAppendProofsRef.current.clear();
      olderHistoryLoadsRef.current.clear();
    };
  }, []);

  useEffect(
    () =>
      addSessionHistoryPageDemandListener((demand) => {
        const { sessionId } = demand;
        invalidatePartialTailProof(sessionId);
        if (
          demand.direction !== "older" ||
          demand.requestId !== undefined
        ) {
          void loadBoundedSessionHistoryWindow({
            context: createSessionHistoryLoadingContext(),
            demand,
          });
          return;
        }
        if (!isMountedRef.current || !sessionStillNeedsHydration(sessionId)) {
          return;
        }
        startSessionHydration(sessionId, { queueAfterCurrent: true });
      }),
    [],
  );

  function cancelStaleSendResponseRecoveryPollForSessions(
    sessionIds: Iterable<string>,
  ) {
    const polledSessionId = activePromptPollSessionIdRef.current;
    if (!polledSessionId) {
      return;
    }

    for (const sessionId of sessionIds) {
      if (sessionId !== polledSessionId) {
        continue;
      }
      activePromptPollCancelRef.current?.();
      activePromptPollCancelRef.current = null;
      activePromptPollSessionIdRef.current = null;
      return;
    }
  }

  function applyDelegationWaitDeltaLocally(delta: DeltaEvent) {
    if (delta.type !== "delegationWaitCreated" && delta.type !== "delegationWaitConsumed") return;
    if (!delegationWaitWatermarkRef.current.acceptDelta(
      delta.revision, delta.serverInstanceId, latestStateRevisionRef.current,
      delta.type === "delegationWaitCreated",
    )) return;
    const nextWaits = delta.type === "delegationWaitCreated"
      ? applyDelegationWaitCreated(delegationWaitsRef.current, delta.wait)
      : applyDelegationWaitConsumed(delegationWaitsRef.current, delta.waitId);
    if (areDelegationWaitRecordsEqual(delegationWaitsRef.current, nextWaits)) {
      return;
    }

    delegationWaitsRef.current = nextWaits;
    setDelegationWaits(nextWaits);
  }

  function adoptSessions(
    nextSessions: StateSessionSummary[],
    options?: AdoptSessionsOptions,
  ) {
    const previousSessions = sessionsRef.current;
    const mergedSessions = params.sessionAuthority.adoptSummaries(
      nextSessions,
      {
        disableMutationStampFastPath: options?.disableMutationStampFastPath,
        forceMessagesUnloaded: options?.forceMessagesUnloaded,
      },
    );
    queueMicrotask(startVisibleTranscriptRepairs);
    // Summary metadata cannot prove what happened to transcript content.
    // Only observed reducer applications may advance an append proof.
    for (const [sessionId, proof] of partialTailAppendProofsRef.current) {
      const merged = mergedSessions.find((entry) => entry.id === sessionId);
      if (!merged || !samePartialTailProjection(proof.latest, merged)) {
        proof.valid = false;
      }
    }
    const pendingOpenSessionId =
      options?.openSessionId ?? pendingRecoveryOpenSessionIdRef.current;
    const pendingPaneId =
      options?.openSessionId !== undefined
        ? (options.paneId ?? null)
        : (pendingRecoveryPaneIdRef.current ?? null);
    const shouldPruneDelegatedChildWorkspaceTabs =
      options?.pruneDelegatedChildWorkspaceTabs === true;

    // Transport loss invalidates the hydrated-tail proof even when summary
    // reconciliation keeps the same already-partial session object.
    if (options?.forceMessagesUnloaded === true) {
      for (const session of mergedSessions) {
        tailLoadedSessionIdsRef.current.delete(session.id);
      }
    }

    if (
      mergedSessions === previousSessions &&
      pendingOpenSessionId === undefined &&
      !shouldPruneDelegatedChildWorkspaceTabs
    ) {
      return;
    }

    const availableSessionIds = new Set(
      mergedSessions.map((session) => session.id),
    );
    const canOpenPendingSession =
      pendingOpenSessionId !== undefined &&
      availableSessionIds.has(pendingOpenSessionId);

    if (
      mergedSessions === previousSessions &&
      !canOpenPendingSession &&
      !shouldPruneDelegatedChildWorkspaceTabs
    ) {
      return;
    }

    const previousSessionsById = new Map(
      previousSessions.map((session) => [session.id, session]),
    );
    const changedSessions =
      mergedSessions === previousSessions
        ? []
        : mergedSessions.filter(
            (session) => previousSessionsById.get(session.id) !== session,
          );
    const removedSessionIds = new Set(
      mergedSessions === previousSessions
        ? []
        : previousSessions.flatMap((session) =>
            availableSessionIds.has(session.id) ? [] : [session.id],
          ),
    );
    const unhydratedSessionIds = new Set(
      mergedSessions === previousSessions
        ? []
        : mergedSessions.flatMap((session) =>
            session.messagesLoaded === false ? [session.id] : [],
          ),
    );
    const sessionsWithChangedWorkdir = new Set(
      mergedSessions === previousSessions
        ? []
        : mergedSessions.flatMap((session) => {
            const previousSession = previousSessionsById.get(session.id);
            return previousSession &&
              previousSession.workdir !== session.workdir
              ? [session.id]
              : [];
          }),
    );
    const hasRemovedSessions = removedSessionIds.size > 0;
    const hasWorkdirInvalidations = sessionsWithChangedWorkdir.size > 0;
    // Avoid rewriting workspace state when an adopted snapshot preserves the
    // same reconciled sessions. Workspace autosave is keyed off `workspace`
    // identity, so an identity-only rewrite here can create a loop:
    // workspace PUT -> SSE state snapshot -> adoptSessions -> workspace save.
    const shouldReconcileWorkspace =
      mergedSessions !== previousSessions ||
      canOpenPendingSession ||
      shouldPruneDelegatedChildWorkspaceTabs;

    if (mergedSessions !== previousSessions) {
      flushAndCancelPendingSessionRender(mergedSessions);
    }
    startTransition(() => {
      if (mergedSessions !== previousSessions) {
        params.sessionAuthority.publish();
      }
      if (shouldReconcileWorkspace) {
        setWorkspace((current) => {
          return reconcileAdoptedSessionsWorkspace({
            applyControlPanelLayout,
            canOpenPendingSession,
            current,
            mergedSessions,
            pendingOpenSessionId,
            pendingPaneId,
            pruneDelegatedChildWorkspaceTabs:
              shouldPruneDelegatedChildWorkspaceTabs,
            sessionsChanged: mergedSessions !== previousSessions,
          });
        });
      }
      if (hasRemovedSessions) {
        for (const id of removedSessionIds) seenTestRunWaitFailuresRef.current.delete(id);
        setTestRunWaitFailures(current => {
          const next = { ...current };
          for (const id of removedSessionIds) delete next[id];
          return next;
        });
        setDraftsBySessionId((current) =>
          pruneSessionValues(current, availableSessionIds),
        );
        setDraftAttachmentsBySessionId((current) =>
          pruneSessionAttachmentValues(current, availableSessionIds),
        );
        setSendingSessionIds((current) =>
          pruneSessionFlags(current, availableSessionIds),
        );
        setStoppingSessionIds((current) =>
          pruneSessionFlags(current, availableSessionIds),
        );
        setKillingSessionIds((current) =>
          pruneSessionFlags(current, availableSessionIds),
        );
        setKillRevealSessionId((current) =>
          current && availableSessionIds.has(current) ? current : null,
        );
        setPendingKillSessionId((current) =>
          current && availableSessionIds.has(current) ? current : null,
        );
        setPendingSessionRename((current) =>
          current && availableSessionIds.has(current.sessionId)
            ? current
            : null,
        );
        setUpdatingSessionIds((current) =>
          pruneSessionFlags(current, availableSessionIds),
        );
        setSessionSettingNotices((current) =>
          pruneSessionValues(current, availableSessionIds),
        );
      }
      if (hasRemovedSessions || hasWorkdirInvalidations) {
        setAgentCommandsBySessionId((current) =>
          pruneSessionCommandValues(
            current,
            availableSessionIds,
            sessionsWithChangedWorkdir,
          ),
        );
        setRefreshingAgentCommandSessionIds((current) =>
          pruneSessionFlagsWithInvalidation(
            current,
            availableSessionIds,
            sessionsWithChangedWorkdir,
          ),
        );
        setAgentCommandErrors((current) =>
          pruneSessionValues(
            current,
            availableSessionIds,
            sessionsWithChangedWorkdir,
          ),
        );
      }
    });
    if (canOpenPendingSession) {
      pendingRecoveryOpenSessionIdRef.current = undefined;
      pendingRecoveryPaneIdRef.current = undefined;
    }
    if (hasRemovedSessions) {
      hydratingSessionIdsRef.current = new Set(
        [...hydratingSessionIdsRef.current].filter((sessionId) =>
          availableSessionIds.has(sessionId),
        ),
      );
      queuedTextRepairHydrationSessionIdsRef.current = new Set(
        [...queuedTextRepairHydrationSessionIdsRef.current].filter(
          (sessionId) => availableSessionIds.has(sessionId),
        ),
      );
      queuedHydrationSessionIdsRef.current = new Set(
        [...queuedHydrationSessionIdsRef.current].filter((sessionId) =>
          availableSessionIds.has(sessionId),
        ),
      );
      queuedTailRepairSessionIdsRef.current = new Set(
        [...queuedTailRepairSessionIdsRef.current].filter((sessionId) =>
          availableSessionIds.has(sessionId),
        ),
      );
      tailLoadedSessionIdsRef.current = new Set(
        [...tailLoadedSessionIdsRef.current].filter((sessionId) =>
          availableSessionIds.has(sessionId),
        ),
      );
      for (const sessionId of hydrationRetryTimersRef.current.keys()) {
        if (!availableSessionIds.has(sessionId)) {
          // Removed sessions have no demand to wake when their retry clears.
          clearHydrationRetry(sessionId);
        }
      }
    }
    if (hasRemovedSessions || unhydratedSessionIds.size > 0) {
      hydratedSessionIdsRef.current = new Set(
        [...hydratedSessionIdsRef.current].filter(
          (sessionId) =>
            availableSessionIds.has(sessionId) &&
            !unhydratedSessionIds.has(sessionId),
        ),
      );
    }
    queueMicrotask(() => { for (const session of mergedSessions) requestSessionTailRead(session.id); });
    for (const session of mergedSessions) {
      const previousSession = previousSessionsById.get(session.id);
      const previousMessageCount =
        previousSession?.messageCount ?? previousSession?.messages.length;
      const nextMessageCount = session.messageCount ?? session.messages.length;
      const previousMutationStamp =
        previousSession?.sessionMutationStamp ?? null;
      const nextMutationStamp = session.sessionMutationStamp ?? null;
      const transcriptAuthorityChanged =
        previousSession !== undefined &&
        (previousMessageCount !== nextMessageCount ||
          (previousMutationStamp !== null &&
            nextMutationStamp !== null &&
            previousMutationStamp !== nextMutationStamp));
      if (
        session.messagesLoaded === false &&
        (options?.forceMessagesUnloaded === true ||
          previousSession?.messagesLoaded === true ||
          transcriptAuthorityChanged)
      ) {
        // A bounded tail is only authoritative for the summary metadata that
        // produced it. If a same-server snapshot advances the transcript count
        // or mutation stamp, re-arm targeted hydration even when both the old
        // and new projections are already `messagesLoaded: false`.
        tailLoadedSessionIdsRef.current.delete(session.id);
      }
    }
    if (hasRemovedSessions || unhydratedSessionIds.size > 0) {
      hydrationMismatchSessionIdsRef.current = new Set(
        [...hydrationMismatchSessionIdsRef.current].filter(
          (sessionId) =>
            availableSessionIds.has(sessionId) &&
            !unhydratedSessionIds.has(sessionId),
        ),
      );
    }
    if (hasRemovedSessions || hasWorkdirInvalidations) {
      refreshingAgentCommandSessionIdsRef.current =
        pruneSessionFlagsWithInvalidation(
          refreshingAgentCommandSessionIdsRef.current,
          availableSessionIds,
          sessionsWithChangedWorkdir,
        );
    }
    const availableUnknownModelKeys =
      buildUnknownModelConfirmationKeySet(mergedSessions);
    if (
      !setContainsOnlyValuesFrom(
        confirmedUnknownModelSendsRef.current,
        availableUnknownModelKeys,
      )
    ) {
      confirmedUnknownModelSendsRef.current = new Set(
        [...confirmedUnknownModelSendsRef.current].filter((key) =>
          availableUnknownModelKeys.has(key),
        ),
      );
    }
  }

  function adoptCreatedSessionResponse(
    created: CreateSessionResponse,
    options?: { openSessionId?: string; paneId?: string | null },
  ): AdoptCreatedSessionOutcome {
    if (created.session.id !== created.sessionId) {
      // Wire contract guarantees `session.id === sessionId`; a mismatch
      // means protocol drift. Trigger a recovery resync so the client
      // reconciles against authoritative state instead of opening a
      // workspace pane for a session that was never inserted into
      // `sessionsRef`. Mirrors the sibling path in `adoptFetchedSession`.
      requestActionRecoveryResyncRef.current({
        allowUnknownServerInstance: true,
      });
      return "recovering";
    }

    const isUnknownCrossInstanceCreateResponse =
      isServerInstanceMismatch(
        lastSeenServerInstanceIdRef.current,
        created.serverInstanceId,
      ) &&
      !!created.serverInstanceId &&
      !seenServerInstanceIdsRef.current.has(created.serverInstanceId);
    if (isUnknownCrossInstanceCreateResponse) {
      requestActionRecoveryResyncRef.current({
        openSessionId: options?.openSessionId ?? created.sessionId,
        paneId: options?.paneId ?? null,
        allowUnknownServerInstance: true,
      });
      return "recovering";
    }

    // Route the session write through the same revision-gate that governs
    // `adoptState`. Same-instance stale POST responses are rejected instead of
    // unconditionally overwriting `sessionsRef`; unknown cross-instance
    // responses above go through `/api/state` recovery before any UI adoption.
    if (
      !shouldAdoptSnapshotRevision(
        latestStateRevisionRef.current,
        created.revision,
        {
          lastSeenServerInstanceId: lastSeenServerInstanceIdRef.current,
          nextServerInstanceId: created.serverInstanceId,
          seenServerInstanceIds: seenServerInstanceIdsRef.current,
        },
      )
    ) {
      return "stale";
    }

    params.sessionAuthority.setServerInstance(created.serverInstanceId);
    const nextSessions = params.sessionAuthority.adoptCreatedSession(created.session);
    if (!nextSessions) return "stale";
    const adoptedSession =
      nextSessions.find((session) => session.id === created.sessionId) ??
      created.session;
    latestStateRevisionRef.current = created.revision;
    if (created.serverInstanceId) {
      rememberServerInstanceId(
        seenServerInstanceIdsRef,
        created.serverInstanceId,
      );
      lastSeenServerInstanceIdRef.current = created.serverInstanceId;
    }
    upsertSessionSlice(adoptedSession);
    flushAndCancelPendingSessionRender(nextSessions);
    params.sessionAuthority.publish();
    setWorkspace((current) =>
      applyControlPanelLayout(
        openSessionInWorkspaceState(
          reconcileWorkspaceState(current, nextSessions),
          options?.openSessionId ?? created.sessionId,
          options?.paneId ?? null,
        ),
      ),
    );
    return "adopted";
  }

  function captureHydrationRequestContext(
    sessionId: string,
    options?: { allowDivergentTextRepairAfterNewerRevision?: boolean },
  ): SessionHydrationRequestContext | null {
    const session = sessionsRef.current.find((entry) => entry.id === sessionId);
    if (!session) {
      return null;
    }

    return {
      kind:
        options?.allowDivergentTextRepairAfterNewerRevision === true
          ? "textRepair"
          : "sessionTail",
      messageCount: getHydrationMessageCount(session),
      revision: latestStateRevisionRef.current,
      serverInstanceId: lastSeenServerInstanceIdRef.current,
      sessionMutationStamp: getHydrationMutationStamp(session),
    };
  }

  // Returns a discriminated outcome because metadata mismatch needs different
  // recovery depending on direction: stale responses retry hydration, while a
  // bounded session response ahead of the current summary must first force
  // `/api/state` so the global revision/session metadata catches up.
  function adoptFetchedSession(
    session: Session, revision: number, serverInstanceId: string,
    requestContext: SessionHydrationRequestContext,
  ): AdoptFetchedSessionOutcome {
    const authority = params.sessionAuthority;
    const latestSessions = sessionsRef.current;
    const current = latestSessions.find(entry => entry.id === session.id) ?? null;
    const pairedLocalTail = authority.isEligibleLocalRead(session, serverInstanceId) &&
      current?.hasNewerHistory !== true;
    const decision = decideHttpSessionAdoption({
      responseSession: session, responseRevision: revision, responseServerInstanceId: serverInstanceId,
      requestContext, currentSession: current, currentRevision: latestStateRevisionRef.current,
      currentServerInstanceId: lastSeenServerInstanceIdRef.current,
      seenServerInstanceIds: seenServerInstanceIdsRef.current, pairedLocalTail,
    });
    if (decision.admission === "none") return decision.outcome;
    const next = authority.adoptTail(session, {
      outcome: decision.outcome as "adopted" | "partial" | "partialCoverage", requestContext,
    });
    if (!next) return "stale";
    applyHttpSessionEffects(decision, { latestRevision: latestStateRevisionRef,
      instance: lastSeenServerInstanceIdRef, seenInstances: seenServerInstanceIdsRef });
    const adopted = next.find(entry => entry.id === session.id)!;
    if (activeTranscriptSessionIdRef.current === session.id) noteSessionTailAdopted(adopted);
    upsertSessionSlice(adopted);
    flushAndCancelPendingSessionRender(next);
    startTransition(() => authority.publish());
    hydrationMismatchSessionIdsRef.current.delete(session.id);
    return decision.outcome;
  }

  function publishHistorySession(session: Session) {
    invalidatePartialTailProof(session.id);
    const latest = sessionsRef.current;
    if (!latest.some(entry => entry.id === session.id)) return false;
    const next = params.sessionAuthority.commit(latest.map(entry =>
      entry.id === session.id ? session : entry), "history");
    const adopted = next.find(entry => entry.id === session.id)!;
    if (adopted.hasNewerHistory === true) tailLoadedSessionIdsRef.current.delete(session.id);
    upsertSessionSlice(adopted);
    flushAndCancelPendingSessionRender(next);
    startTransition(() => params.sessionAuthority.publish());
    hydrationMismatchSessionIdsRef.current.delete(session.id);
    queueMicrotask(startVisibleTranscriptRepairs);
    return true;
  }

  function createSessionHistoryLoadingContext(): SessionHistoryLoadingContext {
    return {
      getLastSeenServerInstanceId: () => lastSeenServerInstanceIdRef.current,
      getSession: id => sessionsRef.current.find(session => session.id === id),
      inFlightOlderLoads: olderHistoryLoadsRef.current,
      isMounted: () => isMountedRef.current,
      publishSession: publishHistorySession,
      reportRequestError,
      requestActionRecoveryResync: options => requestActionRecoveryResyncRef.current(options),
    };
  }

  function startSessionHydration(
    sessionId: string,
    options?: SessionHydrationOptions,
  ) {
    if (
      options?.forceTailRepair || options?.allowDivergentTextRepairAfterNewerRevision
    ) {
      invalidatePartialTailProof(sessionId);
    }
    if (options?.forceTailRepair === true) {
      // Keep the invalidation across a failed request so the ordinary retry
      // path still performs a tail repair instead of falling into history
      // paging.
      tailLoadedSessionIdsRef.current.delete(sessionId);
    }
    if (hydratingSessionIdsRef.current.has(sessionId)) {
      if (
        options?.queueAfterCurrent === true &&
        options.forceTailRepair !== true
      ) {
        queuedHydrationSessionIdsRef.current.add(sessionId);
      }
      if (options?.forceTailRepair === true) {
        queuedTailRepairSessionIdsRef.current.add(sessionId);
      }
      if (options?.allowDivergentTextRepairAfterNewerRevision === true) {
        queuedTextRepairHydrationSessionIdsRef.current.add(sessionId);
      }
      return;
    }

    hydratingSessionIdsRef.current.add(sessionId);
    const requestContext = captureHydrationRequestContext(sessionId, options);
    if (!requestContext) {
      releaseSessionHydrationFlight(hydratingSessionIdsRef.current, sessionId);
      return;
    }
    void (async () => {
      let shouldRetryHydration = false;
      let retryHydrationWithCap = options?.ownerTailRead === true;
      let requestedRecovery = false;
      const requestRecovery = (recoveryOptions?: Parameters<typeof requestActionRecoveryResyncRef.current>[0]) => {
        requestedRecovery = true;
        requestActionRecoveryResyncRef.current(recoveryOptions);
      };
      try {
        let attemptedTailHydration = false;
        if (shouldStartTailLoad(sessionId, options)) {
          attemptedTailHydration = true;
          const baseline = sessionsRef.current.find((entry) => entry.id === sessionId);
          if (baseline) {
            const proof = { baseline, latest: baseline, valid: true };
            requestContext.partialTailAppendProof = proof;
            partialTailAppendProofsRef.current.set(sessionId, proof);
          }
          const tailResponse = await fetchSessionTail(
            sessionId,
            SESSION_TAIL_WINDOW_MESSAGE_COUNT,
          );
          if (!isMountedRef.current) {
            return;
          }
          if (tailResponse.session.id !== sessionId) {
            requestedRecovery = true;
            if (!hydrationMismatchSessionIdsRef.current.has(sessionId)) {
              hydrationMismatchSessionIdsRef.current.add(sessionId);
              requestRecovery();
            }
            return;
          }

          const tailSession = {
            ...tailResponse.session,
            messageStartIndex:
              tailResponse.session.messagesLoaded === true
                ? 0
                : Math.max(
                    0,
                    (tailResponse.session.messageCount ??
                      tailResponse.session.messages.length) -
                      tailResponse.session.messages.length,
                  ),
          };
          const tailAdoptOutcome = adoptFetchedSession(
            tailSession,
            tailResponse.revision,
            tailResponse.serverInstanceId,
            {
              ...requestContext,
              kind: "partialTail",
            },
          );
          switch (tailAdoptOutcome) {
            case "partialCoverage":
              clearHydrationRetry(sessionId);
              tailLoadedSessionIdsRef.current.add(sessionId);
              queuedHydrationSessionIdsRef.current.delete(sessionId);
              return;
            case "partial":
              clearHydrationRetry(sessionId);
              tailLoadedSessionIdsRef.current.add(sessionId);
              queuedHydrationSessionIdsRef.current.delete(sessionId);
              if (!sessionStillNeedsHydration(sessionId)) {
                completeSessionHydration(sessionId);
                return;
              }
              return;
            case "adopted":
              tailLoadedSessionIdsRef.current.add(sessionId);
              completeSessionHydration(sessionId);
              return;
            case "restartResync":
              hydrationAfterStateResyncSessionIdsRef.current.add(sessionId);
              hydrationRestartResyncPendingRef.current = true;
              requestRecovery();
              return;
            case "stateResync":
              requestRecovery();
              shouldRetryHydration = true;
              return;
            case "stale":
              shouldRetryHydration = true;
              return;
            default: {
              const _exhaustive: never = tailAdoptOutcome;
              void _exhaustive;
              break;
            }
          }
        }

        if (attemptedTailHydration && !sessionStillNeedsHydration(sessionId)) {
          completeSessionHydration(sessionId);
          return;
        }

        const retainedSession = sessionsRef.current.find(
          (entry) => entry.id === sessionId,
        );
        const isTextRepair =
          options?.allowDivergentTextRepairAfterNewerRevision === true;
        if (
          retainedSession &&
          (isTextRepair ||
            (retainedSession.messagesLoaded === false &&
              retainedSession.messages.length > 0))
        ) {
          const requestedBefore = isTextRepair
            ? null
            : (retainedSession.messages[0]?.id ?? null);
          if (!isTextRepair && !requestedBefore) {
            shouldRetryHydration = true;
            return;
          }
          if (!isTextRepair) {
            const result = await loadOlderHistoryPageOnce({
              context: { ...createSessionHistoryLoadingContext(),
                requestActionRecoveryResync: requestRecovery },
              requestedBefore: requestedBefore!,
              sessionId,
            });
            if (result.kind === "applied") {
              if (result.session.messagesLoaded === true) {
                completeSessionHydration(sessionId);
              } else {
                clearHydrationRetry(sessionId);
              }
            } else if (result.kind === "failed") {
              shouldRetryHydration = true;
              retryHydrationWithCap = true;
            }
            return;
          }

          const historyPage = await fetchSessionHistory(sessionId, {
            before: null,
            limit: SESSION_HISTORY_PAGE_MESSAGE_COUNT,
          });
          if (!isMountedRef.current) {
            return;
          }
          if (
            isServerInstanceMismatch(
              lastSeenServerInstanceIdRef.current,
              historyPage.serverInstanceId,
            )
          ) {
            requestRecovery({
              allowUnknownServerInstance: true,
            });
            return;
          }

          const currentSession = sessionsRef.current.find(
            (entry) => entry.id === sessionId,
          );
          if (!currentSession) {
            return;
          }
          const mergeOutcome = repairSessionTailFromHistoryPage({
            current: currentSession,
            page: historyPage,
          });
          switch (mergeOutcome.kind) {
            case "applied":
              if (!publishHistorySession(mergeOutcome.session)) {
                return;
              }
              if (mergeOutcome.session.messagesLoaded === true) {
                completeSessionHydration(sessionId);
              } else {
                clearHydrationRetry(sessionId);
              }
              return;
            case "cursorChanged":
            case "metadataChanged":
              requestRecovery();
              return;
            case "protocolError":
              throw new Error(mergeOutcome.message);
            default: {
              const _exhaustive: never = mergeOutcome;
              void _exhaustive;
              return;
            }
          }
        }

        // Every transcript load is either the recent tail or one bounded
        // history page. Reaching this point means local metadata no longer
        // describes either state; repair from the authoritative summary.
        requestRecovery();
        return;
      } catch (error) {
        if (!isMountedRef.current) {
          return;
        }
        // 404 is a benign race: the session was deleted, hidden,
        // or renumbered between a delta event that referenced it
        // and this hydration fetch. The action-recovery resync
        // will repair our local view on the next SSE tick without
        // dropping a toast on the user. Mirrors
        // `fetchWorkspaceLayout`'s "404 -> silent recovery" UX
        // posture; the transport throws `ApiRequestError` and we branch on
        // `instanceof` + status at the call site.
        if (
          error instanceof ApiRequestError &&
          (error.status === 404 || error.status === 409)
        ) {
          requestRecovery();
          return;
        }
        reportRequestError(error);
        shouldRetryHydration = true;
        retryHydrationWithCap = true;
      } finally {
        if (partialTailAppendProofsRef.current.get(sessionId) === requestContext.partialTailAppendProof) {
          partialTailAppendProofsRef.current.delete(sessionId);
        }
        releaseSessionHydrationFlight(hydratingSessionIdsRef.current, sessionId);
        if (
          queuedTextRepairHydrationSessionIdsRef.current.delete(sessionId) &&
          isMountedRef.current
        ) {
          startSessionHydration(sessionId, {
            allowDivergentTextRepairAfterNewerRevision: true,
          });
        } else if (
          queuedTailRepairSessionIdsRef.current.delete(sessionId) &&
          isMountedRef.current
        ) {
          startSessionHydration(sessionId, {
            forceTailRepair: true,
            queueAfterCurrent: true,
          });
        } else if (
          queuedHydrationSessionIdsRef.current.delete(sessionId) &&
          isMountedRef.current
        ) {
          startSessionHydration(sessionId, { queueAfterCurrent: true });
        } else if (shouldRetryHydration) {
          scheduleHydrationRetry(sessionId, {
            capAttempts: retryHydrationWithCap,
            ownerTailRead: options?.ownerTailRead === true,
          });
        }
        // Slot release can reopen the gate even without another stream event.
        // Retry/recovery endings are served by their timer or summary instead.
        if (!shouldRetryHydration && !requestedRecovery) requestSessionTailRead(sessionId);
      }
    })();
  }

  function requestSessionTailRead(sessionId: string) {
    if (!shouldRequestSessionTailRead({
      mounted: isMountedRef.current,
      inFlight: hydratingSessionIdsRef.current.has(sessionId),
      retryPending: hydrationRetryTimersRef.current.has(sessionId),
      needsTailRead: () => params.sessionAuthority.needsTailRead(sessionId, visibleHydrationSessionIdsRef.current.has(sessionId)),
    })) return;
    startSessionHydration(sessionId, { forceTailRepair: true, ownerTailRead: true });
  }

  useEffect(() => {
    const hydrationTargetIds = new Set<string>();
    if (activeSession) {
      hydrationTargetIds.add(activeSession.id);
    }
    for (const target of visibleSessionHydrationTargets) {
      hydrationTargetIds.add(target.id);
    }
    queueMicrotask(startVisibleTranscriptRepairs);

    const sessionIdsToHydrate = [...hydrationTargetIds].filter((sessionId) => {
      const session = sessionsRef.current.find(
        (candidate) => candidate.id === sessionId,
      );
      return (
        session?.messagesLoaded === false &&
        !tailLoadedSessionIdsRef.current.has(sessionId) &&
        // A declined response already owns a retry deadline. Passive renders
        // must not race it; explicit demand and forced repair call directly.
        !hydrationRetryTimersRef.current.has(sessionId)
      );
    });
    if (sessionIdsToHydrate.length === 0) {
      return;
    }

    for (const sessionId of sessionIdsToHydrate) {
      startSessionHydration(sessionId);
    }
    // This effect owns the one automatic request: loading the recent tail for
    // a summary-only session. Once any messages are present, older history is
    // fetched only by explicit scroll/search/marker demand. Do not turn
    // `messagesLoaded: false` into an automatic page loop.
  }, [
    activeSession?.id,
    activeSession?.messages.length,
    activeSession?.messagesLoaded,
    visibleSessionHydrationTargets,
  ]);

  function syncPreferencesFromState(nextState: StateResponse) {
    const preferences = resolveAppPreferences(nextState.preferences);
    setDefaultCodexModel(preferences.defaultCodexModel);
    setDefaultCodexSandboxMode(preferences.defaultCodexSandboxMode);
    setDefaultCodexApprovalPolicy(preferences.defaultCodexApprovalPolicy);
    setDefaultClaudeModel(preferences.defaultClaudeModel);
    setDefaultCursorModel(preferences.defaultCursorModel);
    setDefaultGeminiModel(preferences.defaultGeminiModel);
    setDefaultKimiModel(preferences.defaultKimiModel);
    setDefaultKimiApprovalMode(preferences.defaultKimiApprovalMode);
    setDefaultKimiEffort(preferences.defaultKimiEffort);
    setDefaultOpenCodeModel(preferences.defaultOpenCodeModel);
    setDefaultOpenCodeApprovalMode(preferences.defaultOpenCodeApprovalMode);
    setDefaultCodexReasoningEffort(preferences.defaultCodexReasoningEffort);
    setDefaultClaudeApprovalMode(preferences.defaultClaudeApprovalMode);
    setDefaultClaudeEffort(preferences.defaultClaudeEffort);
    setRemoteConfigs((current) =>
      areRemoteConfigsEqual(current, preferences.remotes)
        ? current
        : preferences.remotes,
    );
    setTelegramConfig((current) =>
      areTelegramUiConfigsEqual(current, preferences.telegram)
        ? current
        : preferences.telegram,
    );
    setEngramHostSettings(preferences.engram);
  }

  function startVisibleTranscriptRepairs() {
    if (!isMountedRef.current) return;
    for (const id of visibleHydrationSessionIdsRef.current) requestSessionTailRead(id);
  }

  function markTranscriptsDirty() {
    params.sessionAuthority.declareLoss("lagged");
    invalidatePartialTailProof();
    queueMicrotask(startVisibleTranscriptRepairs);
  }

  function repairTranscriptsAfterTransportLoss() {
    invalidatePartialTailProof();
    queueMicrotask(startVisibleTranscriptRepairs);
  }

  function adoptState(nextState: StateResponse, options?: AdoptStateOptions) {
    if (!isMountedRef.current) {
      return false;
    }

    const fullStateServerInstanceChanged =
      !!nextState.serverInstanceId &&
      nextState.serverInstanceId !== lastFullStateServerInstanceIdRef.current;
    const hadFullStateServerInstance = lastFullStateServerInstanceIdRef.current !== null;
    const allowUnknownServerInstance =
      options?.allowUnknownServerInstance === true;
    const allowServerInstanceChange =
      fullStateServerInstanceChanged && allowUnknownServerInstance;
    if (
      !shouldAdoptSnapshotRevision(
        latestStateRevisionRef.current,
        nextState.revision,
        {
          ...options,
          force: options?.force === true || allowServerInstanceChange,
          allowRevisionDowngrade:
            options?.allowRevisionDowngrade === true ||
            allowServerInstanceChange,
          lastSeenServerInstanceId: lastSeenServerInstanceIdRef.current,
          nextServerInstanceId: nextState.serverInstanceId,
          seenServerInstanceIds: seenServerInstanceIdsRef.current,
          allowUnknownServerInstance,
        },
      )
    ) {
      return false;
    }

    params.sessionAuthority.setServerInstance(nextState.serverInstanceId);
    latestStateRevisionRef.current = nextState.revision;
    if (fullStateServerInstanceChanged) {
      // Keep a failure this server sent before its snapshot was adopted; drop
      // failures from an older server and untagged ones from older backends.
      const fromThisServer = (failure: import("./test-run-waits").TestRunWaitFailure) =>
        failure.serverInstanceId === nextState.serverInstanceId;
      for (const [sessionId, failure] of seenTestRunWaitFailuresRef.current) {
        if (!fromThisServer(failure)) seenTestRunWaitFailuresRef.current.delete(sessionId);
      }
      setTestRunWaitFailures(current => {
        const kept = Object.fromEntries(Object.entries(current).filter(([, failure]) => fromThisServer(failure)));
        return Object.keys(kept).length === Object.keys(current).length ? current : kept;
      });
    }
    setTestRuns(current => reconcileTestRunSnapshot(current, nextState.testRuns ?? []));
    // A partial wait delta can be ahead of this otherwise adoptable snapshot.
    // Keep its projection until a snapshot covering that delta arrives.
    if (testRunWaitWatermarkRef.current.snapshotCovers(nextState.revision, nextState.serverInstanceId)) {
      setTestRunWaits(current => JSON.stringify(current) === JSON.stringify(nextState.testRunWaits ?? []) ? current : nextState.testRunWaits ?? []);
    }
    setHasAdoptedStateSnapshot(true);
    if (nextState.serverInstanceId) {
      rememberServerInstanceId(
        seenServerInstanceIdsRef,
        nextState.serverInstanceId,
      );
      lastSeenServerInstanceIdRef.current = nextState.serverInstanceId;
      lastFullStateServerInstanceIdRef.current = nextState.serverInstanceId;
    }
    const pendingSseRecreateOnInstanceChange =
      pendingSseRecreateOnInstanceChangeRef.current;
    const shouldClearPendingSseRecreateFalseAlarm =
      pendingSseRecreateOnInstanceChange !== null &&
      options?.sseReconnectRequestId ===
        pendingSseRecreateOnInstanceChange.requestId;

    if (fullStateServerInstanceChanged) {
      // Retry deadlines belong to the old authority, not restart recovery.
      if (hadFullStateServerInstance) params.sessionAuthority.declareLoss("serverInstanceChanged");
      cancelHydrationRetries();
      invalidatePartialTailProof();
      partialTailAppendProofsRef.current.clear();
      hydratingSessionIdsRef.current.clear();
      hydratedSessionIdsRef.current.clear();
      queuedHydrationSessionIdsRef.current.clear();
      queuedTailRepairSessionIdsRef.current.clear();
      queuedTextRepairHydrationSessionIdsRef.current.clear();
      tailLoadedSessionIdsRef.current.clear();
      olderHistoryLoadsRef.current.clear();
      // Caller-requested EventSource recreation on instance change. See
      // `forceSseReconnect` for the full context. The flag is set
      // synchronously by `handleSend` BEFORE the recovery probe is in
      // flight; consuming it here — strictly AFTER `adoptState` has
      // committed the recovered state — avoids the race where a
      // synchronous `setSseEpoch` would tear down the effect mid-probe
      // and drop the recovered response. The `setSseEpoch` will queue a
      // re-render whose effect cleanup runs ONLY after the in-progress
      // adoption is fully reflected in React state.
      if (pendingSseRecreateOnInstanceChange !== null) {
        pendingSseRecreateOnInstanceChangeRef.current = null;
        setSseEpoch((current) => current + 1);
      }
    } else if (shouldClearPendingSseRecreateFalseAlarm) {
      // A successful same-instance recovery means the caller's restart
      // suspicion was a false alarm. Clear the pending marker so a later,
      // unrelated instance change does not recreate the EventSource for a
      // stale request.
      pendingSseRecreateOnInstanceChangeRef.current = null;
    }
    hydrationMismatchSessionIdsRef.current.clear();
    const currentCodexState = codexStateRef.current;
    const currentAgentReadiness = agentReadinessRef.current;
    const currentProjects = projectsRef.current;
    const currentOrchestrators = orchestratorsRef.current;
    const currentWorkspaceSummaries = workspaceSummariesRef.current;
    const adoptedStateSlices = resolveAdoptedStateSlices(
      {
        codex: currentCodexState,
        agentReadiness: currentAgentReadiness,
        projects: currentProjects,
        orchestrators: currentOrchestrators,
        workspaces: currentWorkspaceSummaries,
      },
      nextState,
    );
    if (adoptedStateSlices.codex !== currentCodexState) {
      codexStateRef.current = adoptedStateSlices.codex;
      cancelPendingCodexStateRender();
      setCodexState(adoptedStateSlices.codex);
    }
    if (adoptedStateSlices.agentReadiness !== currentAgentReadiness) {
      agentReadinessRef.current = adoptedStateSlices.agentReadiness;
      setAgentReadiness(adoptedStateSlices.agentReadiness);
    }
    syncPreferencesFromState(nextState);
    if (adoptedStateSlices.projects !== currentProjects) {
      projectsRef.current = adoptedStateSlices.projects;
      setProjects(adoptedStateSlices.projects);
    }
    if (adoptedStateSlices.orchestrators !== currentOrchestrators) {
      orchestratorsRef.current = adoptedStateSlices.orchestrators;
      setOrchestrators(adoptedStateSlices.orchestrators);
    }
    const nextDelegationWaits = nextState.delegationWaits ?? [];
    // Wait deltas can be ahead of this snapshot's delegation membership.
    // Keep their projection until the requested covering snapshot arrives.
    if (
      delegationWaitWatermarkRef.current.snapshotCovers(nextState.revision, nextState.serverInstanceId) &&
      !areDelegationWaitRecordsEqual(
        delegationWaitsRef.current,
        nextDelegationWaits,
      )
    ) {
      delegationWaitsRef.current = nextDelegationWaits;
      setDelegationWaits(nextDelegationWaits);
    }
    const nextDelegationChildSessionIds = new Set(
      (nextState.delegations ?? []).map(
        (delegation) => delegation.childSessionId,
      ),
    );
    setDelegationChildSessionIds((current) =>
      current.size === nextDelegationChildSessionIds.size &&
      setContainsOnlyValuesFrom(current, nextDelegationChildSessionIds)
        ? current
        : nextDelegationChildSessionIds,
    );
    const nextPendingEngramMcpRevocationSessionIds = new Set(
      nextState.pendingEngramMcpRevocationSessionIds ?? [],
    );
    setPendingEngramMcpRevocationSessionIds((current) =>
      current.size === nextPendingEngramMcpRevocationSessionIds.size &&
      setContainsOnlyValuesFrom(
        current,
        nextPendingEngramMcpRevocationSessionIds,
      )
        ? current
        : nextPendingEngramMcpRevocationSessionIds,
    );
    if (adoptedStateSlices.workspaces !== currentWorkspaceSummaries) {
      workspaceSummariesRef.current = adoptedStateSlices.workspaces;
      setWorkspaceSummaries(adoptedStateSlices.workspaces);
    }
    const requestedOpenSessionId =
      options?.openSessionId ?? pendingRecoveryOpenSessionIdRef.current;
    adoptSessions(
      applyDelegationParentIdsFromSummaries(
        nextState.sessions,
        nextState.delegations ?? [],
      ),
      resolveAdoptStateSessionOptions(options, fullStateServerInstanceChanged),
    );
    if (options?.forceMessagesUnloaded === true) {
      // A same-metadata recovery may preserve session identity, so no React
      // effect runs. Repair visible transcripts explicitly, without fetching
      // every inactive session affected by the global transport loss.
      startVisibleTranscriptRepairs();
    }
    // Local state adoptions can resume or create active sessions before any SSE arrives.
    syncAdoptedLiveSessionResumeWatchdogBaselinesRef.current(
      sessionsRef.current,
    );
    for (const sessionId of hydrationAfterStateResyncSessionIdsRef.current) {
      hydrationAfterStateResyncSessionIdsRef.current.delete(sessionId);
      if (sessionStillNeedsHydration(sessionId)) {
        startSessionHydration(sessionId, { queueAfterCurrent: true });
      }
    }
    if (requestedOpenSessionId) {
      const openedSession = nextState.sessions.find(
        (session) => session.id === requestedOpenSessionId,
      );
      if (openedSession) {
        setSelectedProjectId(openedSession.projectId ?? ALL_PROJECTS_FILTER_ID);
      }
    }
    resumeSessionHistoryDemandsAfterStateAdoption();
    return true;
  }

  function flushWorkspaceFilesChangedEventBuffer() {
    flushWorkspaceFilesChangedEventGateBuffer({
      gateRefs: workspaceFilesChangedEventGateRefs,
      isMountedRef,
      setWorkspaceFilesChangedEvent,
    });
  }

  function resetWorkspaceFilesChangedEventGate() {
    resetWorkspaceFilesChangedEventGateRefs(workspaceFilesChangedEventGateRefs);
  }

  function enqueueWorkspaceFilesChangedEvent(
    filesChanged: WorkspaceFilesChangedEvent,
  ) {
    enqueueWorkspaceFilesChangedEventInGate(
      workspaceFilesChangedEventGateRefs,
      filesChanged,
      flushWorkspaceFilesChangedEventBuffer,
    );
  }

  useAppLiveStateTransport({
    sessionAuthority: params.sessionAuthority,
    requestSessionTailRead,
    visibleHydrationSessionIdsRef,
    applyTestRunDeltaLocally: delta => {
      if (delta.type === "testRunChanged" || delta.type === "testRunRemoved") {
        setTestRuns(current => applyTestRunDelta(current, delta));
      } else if (delta.type === "testRunWaitResumeDispatchFailed") {
        if (
          classifyDeltaServerIdentity(
            delta.serverInstanceId, lastSeenServerInstanceIdRef.current, seenServerInstanceIdsRef.current,
          ) === "retired"
        ) return;
        const seen = seenTestRunWaitFailuresRef.current.get(delta.sessionId);
        // Revisions are comparable only within one server instance: a restart
        // can roll the revision back. An unseen server may report a failure
        // before its first snapshot; a retired server was rejected above.
        const sameServer = seen?.serverInstanceId === delta.serverInstanceId;
        if (seen && sameServer && (seen.revision > delta.revision || (seen.revision === delta.revision && seen.error === delta.error))) return;
        seenTestRunWaitFailuresRef.current.set(delta.sessionId, delta);
        setTestRunWaitFailures(current => ({ ...current, [delta.sessionId]: delta }));
      } else if (delta.type === "testRunWaitCreated" || delta.type === "testRunWaitConsumed") {
        if (!testRunWaitWatermarkRef.current.acceptDelta(
          delta.revision, delta.serverInstanceId, latestStateRevisionRef.current,
          delta.type === "testRunWaitCreated",
        )) return;
        setTestRunWaits(current => applyTestRunWaitDelta(current, delta));
      }
    },
    observeHydrationDelta,
    hasPartialTailAppendProof,
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
    markTranscriptsDirty,
    repairTranscriptsAfterTransportLoss,
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
  });

  /**
   * Marks an EventSource recreation as pending and returns a request token.
   * The marker fires when any later `adoptState` call observes a
   * `fullStateServerInstanceChanged` flip. The returned token scopes only the
   * same-instance false-alarm cleanup path, so older in-flight `/api/state`
   * probes can still consume the armed recreate when they are first to adopt
   * the replacement instance.
   * Used by `useAppSessionActions::handleSend` after detecting the
   * server-restarted-mid-request case: the immediate
   * `requestActionRecoveryResync` probe will adopt the new state in
   * the same closure that handleSend kicked off, and the recreate fires
   * AFTER that adoption — not synchronously alongside it. Synchronous
   * `setSseEpoch` would tear down the transport effect mid-probe (the
   * cleanup sets `cancelled = true` and the probe's await callback
   * bails before the recovered state is applied).
   *
   * The flag-on-adopt design keeps the existing "after replacement-
   * instance fallback adoption, polling MUST continue" tests green
   * because they don't go through `forceSseReconnect`. Round 8's
   * blanket EventSource recreation in `adoptState` was reverted for
   * exactly that reason; this version re-introduces the recreate but
   * gated on an explicit caller request.
   */
  function forceSseReconnect() {
    const requestId = nextSseReconnectRequestIdRef.current;
    nextSseReconnectRequestIdRef.current += 1;
    pendingSseRecreateOnInstanceChangeRef.current = { requestId };
    return requestId;
  }

  return {
    adoptState,
    testRuns,
    testRunWaits,
    testRunWaitFailures,
    dismissTestRunWaitFailure,
    adoptCreatedSessionResponse,
    syncPreferencesFromState,
    clearHydrationMismatchSessionIds,
    hydratedSessionIdsRef,
    hydratingSessionIdsRef,
    forceAdoptNextStateEventRef,
    forceSseReconnect,
    workspaceFilesChangedEvent,
    workspaceFilesChangedEventBufferRef,
    workspaceFilesChangedEventFlushTimeoutRef,
    resetWorkspaceFilesChangedEventGate,
  };
}
