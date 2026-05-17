// app-live-state.ts
//
// Owns: the full live-state transport plumbing that used to live
// inline in App.tsx. That includes the SSE `state` / `delta` /
// `workspaceFilesChanged` handler bodies, the adoption helpers
// (`adoptState`, `adoptSessions`, `adoptCreatedSessionResponse`,
// `adoptFetchedSession`, `syncPreferencesFromState`), the
// workspace-files-changed React state that consumes the extracted
// buffering gate from app-live-state-workspace-events, the
// `forceAdoptNextStateEventRef` refresh flag, the session hydration fetch effect, the
// `hydratedSessionIdsRef` / `hydratingSessionIdsRef` tracking
// refs, AND (as of Slice 13B) the EventSource open/close
// lifecycle, reconnect fallback timer orchestration, watchdog
// timer orchestration, visibility / focus / pagehide / pageshow
// recovery handlers, the reconnect/watchdog coordination helpers
// (`confirmReconnectRecoveryFromLiveEvent`, `requestStateResync`,
// etc.; pure activity-map updates live in app-live-state-activity),
// and the per-mount
// state-resync bookkeeping refs (`stateResyncInFlightRef` et al).
// The `workspaceFilesChangedEvent` React state + setter also
// live here — consumers in App.tsx read them via the hook return
// value.
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
// Split out of: ui/src/App.tsx (Slice 13A + 13B of the
// App-split plan, see docs/app-split-plan.md). Slice 13B moved
// the EventSource lifecycle, reconnect/watchdog timers, and
// visibility handlers here; the `transportCoordinationRef`
// bridge from 13A collapsed into direct closure references
// because the hook's handlers now live in the same useEffect
// as the coordination helpers they invoke.

import {
  startTransition,
  useEffect,
  useRef,
  useState,
  type MutableRefObject,
} from "react";
import {
  removeSessionFromStore,
  syncComposerSessionsStoreIncremental,
  upsertSessionStoreSession,
} from "./session-store";
import {
  SESSION_TAIL_FIRST_HYDRATION_MIN_MESSAGES,
  SESSION_TAIL_WINDOW_MESSAGE_COUNT,
} from "./session-tail-policy";
import {
  ApiRequestError,
  fetchSession,
  fetchSessionTail,
  fetchState,
  isBackendUnavailableError,
  type CreateSessionResponse,
  type DelegationWaitRecord,
  type StateResponse,
} from "./api";
import {
  applyDeltaToSessions,
  LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS,
  LIVE_SESSION_WATCHDOG_RESYNC_RETRY_COOLDOWN_MS,
  pruneLiveTransportActivitySessions,
  sessionDeltaAdvancesCurrentMutationStamp,
  sessionHasPotentiallyStaleTransport,
} from "./live-updates";
import {
  areRemoteConfigsEqual,
  resolveAppPreferences,
} from "./session-model-utils";
import { resolveAdoptedStateSlices } from "./state-adoption";
import { ALL_PROJECTS_FILTER_ID } from "./project-filters";
import {
  decideDeltaRevisionAction,
  isServerInstanceMismatch,
  shouldAdoptSnapshotRevision,
} from "./state-revision";
import {
  coalescePendingStateResyncOptions,
  consumePendingStateResyncOptions,
  type PendingStateResyncOptions,
  type RequestStateResyncOptions,
} from "./app-live-state-resync-options";
import {
  markLiveSessionResumeWatchdogBaseline as markLiveSessionResumeWatchdogBaselineActivity,
  markLiveTransportActivity as markLiveTransportSessionActivity,
  syncLiveSessionResumeWatchdogBaselines as syncLiveSessionResumeWatchdogBaselineActivity,
  syncLiveTransportActivityFromState as syncLiveTransportSessionActivityFromState,
} from "./app-live-state-activity";
import {
  createStateEventProfiler,
  extractTopLevelJsonNumber,
  extractTopLevelJsonString,
  payloadHasTopLevelTrueBoolean,
} from "./app-live-state-event-utils";
import {
  isDelegationDeltaEvent,
  isSameRevisionReplayableSessionDelta,
  isSessionDeltaEvent,
  staleSendRecoveryPollSessionIdsForDelta,
} from "./app-live-state-delta-events";
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
  classifyFetchedSessionAdoption,
  getHydrationMessageCount,
  getHydrationMutationStamp,
  type AdoptFetchedSessionOutcome,
  type SessionHydrationRequestContext,
} from "./session-hydration-adoption";
import { mergeOrchestratorDeltaSessions } from "./control-surface-state";
import { reconcileSessions } from "./session-reconcile";
import {
  openSessionInWorkspaceState,
  reconcileWorkspaceState,
  type WorkspaceState,
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
  WorkspaceFilesChangedEvent,
} from "./types";
import {
  pruneSessionAttachmentValues,
  pruneSessionCommandValues,
  pruneSessionFlags,
  pruneSessionFlagsWithInvalidation,
  pruneSessionValues,
  readNavigatorOnline,
  type DraftImageAttachment,
  type SessionAgentCommandMap,
  type SessionFlagMap,
} from "./app-utils";
import type { WorkspaceLayoutSummary } from "./api";
import {
  describeBackendConnectionIssueDetail,
  type BackendConnectionState,
} from "./backend-connection";
import {
  LIVE_SESSION_RESUME_WATCHDOG_INTERVAL_MS,
  RECONNECT_STATE_RESYNC_DELAY_MS,
  RECONNECT_STATE_RESYNC_MAX_DELAY_MS,
  type PendingSessionRename,
  type SessionErrorMap,
  type SessionNoticeMap,
  type StateEventPayload,
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
  fullFetchAdoptFetchedSessionOutcome,
  resolveAdoptStateSessionOptions,
  SESSION_HYDRATION_MAX_RETRY_ATTEMPTS,
  SESSION_HYDRATION_RETRY_DELAYS_MS,
} from "./app-live-state-hydration";
import {
  clearWorkspaceFilesChangedEventBuffer,
  enqueueWorkspaceFilesChangedEvent as enqueueWorkspaceFilesChangedEventInGate,
  flushWorkspaceFilesChangedEventBuffer as flushWorkspaceFilesChangedEventGateBuffer,
  resetWorkspaceFilesChangedEventGate as resetWorkspaceFilesChangedEventGateRefs,
  type WorkspaceFilesChangedEventGateRefs,
} from "./app-live-state-workspace-events";

export type {
  AdoptCreatedSessionOutcome,
  AdoptSessionsOptions,
  AdoptStateOptions,
  SessionHydrationTarget,
  UseAppLiveStateAdoptionRefs,
  UseAppLiveStateParams,
  UseAppLiveStatePreferenceSetters,
  UseAppLiveStateReturn,
  UseAppLiveStateStateSetters,
} from "./app-live-state-types";
export {
  resolveAdoptStateSessionOptions,
  SESSION_HYDRATION_FIRST_RETRY_DELAY_MS,
  SESSION_HYDRATION_MAX_RETRY_ATTEMPTS,
} from "./app-live-state-hydration";

type SessionHydrationOptions = {
  allowDivergentTextRepairAfterNewerRevision?: boolean;
  queueAfterCurrent?: boolean;
};

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
    setSessions,
    setWorkspace,
    setCodexState,
    setAgentReadiness,
    setProjects,
    setOrchestrators,
    setDelegationWaits,
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
    setBackendConnectionIssueDetail,
    setBackendConnectionState,
  } = stateSetters;
  const {
    setDefaultCodexModel,
    setDefaultClaudeModel,
    setDefaultCursorModel,
    setDefaultGeminiModel,
    setDefaultCodexReasoningEffort,
    setDefaultClaudeApprovalMode,
    setDefaultClaudeEffort,
    setRemoteConfigs,
  } = preferenceSetters;

  const hydratingSessionIdsRef = useRef<Set<string>>(new Set());
  const hydratedSessionIdsRef = useRef<Set<string>>(new Set());
  const hydrationMismatchSessionIdsRef = useRef<Set<string>>(new Set());
  const queuedHydrationSessionIdsRef = useRef<Set<string>>(new Set());
  const queuedTextRepairHydrationSessionIdsRef = useRef<Set<string>>(
    new Set(),
  );
  const lastFullStateServerInstanceIdRef = useRef<string | null>(
    lastSeenServerInstanceIdRef.current,
  );
  const hydrationRestartResyncPendingRef = useRef(false);
  const hydrationRetryTimersRef = useRef<Map<string, number>>(new Map());
  const hydrationRetryAttemptsRef = useRef<Map<string, number>>(new Map());
  const hydrationCappedRetryAttemptsRef = useRef<Map<string, number>>(new Map());
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
  const sseRecoveryTimerRef = useRef<ReturnType<typeof window.setTimeout> | null>(
    null,
  );
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
  const pendingStateResyncOptionsRef =
    useRef<PendingStateResyncOptions | null>(null);
  const pendingRecoveryOpenSessionIdRef = useRef<string | undefined>(undefined);
  const pendingRecoveryPaneIdRef = useRef<string | null | undefined>(undefined);
  // Bridges `adoptState` (declared at the hook body so the
  // React render can call it) with the watchdog baseline
  // sync (defined inside the transport useEffect). Assigned on
  // mount, reset to a no-op on cleanup.
  const syncAdoptedLiveSessionResumeWatchdogBaselinesRef = useRef<
    (sessions: Session[], now?: number) => void
  >(() => {});
  const pendingSessionRenderFrameRef = useRef<number | null>(null);
  const hasPendingSessionRenderRef = useRef(false);
  const pendingSessionStoreSyncIdsRef = useRef<Set<string>>(new Set());
  // Session panes subscribe through session-store. Session deltas publish their
  // changed slices immediately, while the pending-id queue stays armed so the
  // coalesced frame can still prune ids that disappear before the broad
  // `sessions` render flushes.
  const eagerlyPublishedSessionStoreIdsRef = useRef<Set<string>>(new Set());
  const pendingCodexStateRenderFrameRef = useRef<number | null>(null);
  const hasPendingCodexStateRenderRef = useRef(false);

  function upsertSessionSlice(session: Session) {
    upsertSessionStoreSession({
      session,
      committedDraft: draftsBySessionIdRef.current[session.id] ?? "",
      draftAttachments:
        draftAttachmentsBySessionIdRef.current[session.id] ?? [],
    });
  }

  function queueSessionSliceForRender(sessionId: string) {
    pendingSessionStoreSyncIdsRef.current.add(sessionId);
    eagerlyPublishedSessionStoreIdsRef.current.delete(sessionId);
  }

  function publishQueuedSessionSlices(sessionSnapshot = sessionsRef.current) {
    const pendingSessionIds = pendingSessionStoreSyncIdsRef.current;
    if (pendingSessionIds.size === 0) {
      return;
    }

    const sessionsById = new Map(
      sessionSnapshot.map((session) => [session.id, session]),
    );
    const changedSessions = [...pendingSessionIds].flatMap((sessionId) => {
      const session = sessionsById.get(sessionId);
      return session ? [session] : [];
    });
    if (changedSessions.length === 0) {
      return;
    }
    changedSessions.forEach((session) => {
      eagerlyPublishedSessionStoreIdsRef.current.add(session.id);
    });

    syncComposerSessionsStoreIncremental({
      changedSessions,
      draftsBySessionId: draftsBySessionIdRef.current,
      draftAttachmentsBySessionId: draftAttachmentsBySessionIdRef.current,
      removedSessionIds: [],
    });
  }

  function flushPendingSessionStoreSync(sessionSnapshot = sessionsRef.current) {
    const pendingSessionIds = pendingSessionStoreSyncIdsRef.current;
    if (pendingSessionIds.size === 0) {
      return;
    }

    const sessionsById = new Map(
      sessionSnapshot.map((session) => [session.id, session]),
    );
    const eagerlyPublishedSessionIds = eagerlyPublishedSessionStoreIdsRef.current;
    const changedSessions = [...pendingSessionIds].flatMap((sessionId) => {
      const session = sessionsById.get(sessionId);
      if (!session || eagerlyPublishedSessionIds.has(sessionId)) {
        return [];
      }
      return [session];
    });
    const removedSessionIds = [...pendingSessionIds].filter(
      (sessionId) => !sessionsById.has(sessionId),
    );
    pendingSessionIds.clear();
    eagerlyPublishedSessionIds.clear();
    if (changedSessions.length === 0 && removedSessionIds.length === 0) {
      return;
    }
    if (changedSessions.length === 0) {
      removedSessionIds.forEach((sessionId) => {
        removeSessionFromStore({ sessionId });
      });
      return;
    }

    syncComposerSessionsStoreIncremental({
      changedSessions,
      draftsBySessionId: draftsBySessionIdRef.current,
      draftAttachmentsBySessionId: draftAttachmentsBySessionIdRef.current,
      removedSessionIds,
    });
  }

  function cancelPendingSessionRender() {
    if (pendingSessionRenderFrameRef.current !== null) {
      window.cancelAnimationFrame(pendingSessionRenderFrameRef.current);
      pendingSessionRenderFrameRef.current = null;
    }
    hasPendingSessionRenderRef.current = false;
    pendingSessionStoreSyncIdsRef.current.clear();
    eagerlyPublishedSessionStoreIdsRef.current.clear();
  }

  function cancelPendingCodexStateRender() {
    if (pendingCodexStateRenderFrameRef.current !== null) {
      window.cancelAnimationFrame(pendingCodexStateRenderFrameRef.current);
      pendingCodexStateRenderFrameRef.current = null;
    }
    hasPendingCodexStateRenderRef.current = false;
  }

  function flushPendingCodexStateRender() {
    pendingCodexStateRenderFrameRef.current = null;
    if (!hasPendingCodexStateRenderRef.current || !isMountedRef.current) {
      return;
    }

    hasPendingCodexStateRenderRef.current = false;
    const nextCodexState = codexStateRef.current;
    startTransition(() => {
      setCodexState(nextCodexState);
    });
  }

  function scheduleCodexStateRender() {
    hasPendingCodexStateRenderRef.current = true;
    if (pendingCodexStateRenderFrameRef.current !== null) {
      return;
    }
    if (
      typeof window === "undefined" ||
      typeof window.requestAnimationFrame !== "function"
    ) {
      flushPendingCodexStateRender();
      return;
    }

    pendingCodexStateRenderFrameRef.current = window.requestAnimationFrame(
      flushPendingCodexStateRender,
    );
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

  function clearHydrationMismatchSessionIds(sessionIds: Iterable<string>) {
    for (const sessionId of sessionIds) {
      hydrationMismatchSessionIdsRef.current.delete(sessionId);
    }
  }

  function sessionStillNeedsHydration(sessionId: string) {
    return sessionsRef.current.some(
      (session) => session.id === sessionId && session.messagesLoaded === false,
    );
  }

  function shouldStartTailFirstHydration(
    sessionId: string,
    options?: { allowDivergentTextRepairAfterNewerRevision?: boolean },
  ) {
    if (options?.allowDivergentTextRepairAfterNewerRevision === true) {
      return false;
    }
    const session = sessionsRef.current.find((entry) => entry.id === sessionId);
    if (!session || session.messagesLoaded !== false || session.messages.length > 0) {
      return false;
    }
    const messageCount =
      typeof session.messageCount === "number"
        ? session.messageCount
        : session.messages.length;
    return messageCount >= SESSION_TAIL_FIRST_HYDRATION_MIN_MESSAGES;
  }

  function scheduleHydrationRetry(
    sessionId: string,
    options: { capAttempts?: boolean } = {},
  ) {
    if (
      !isMountedRef.current ||
      hydrationRetryTimersRef.current.has(sessionId) ||
      !sessionStillNeedsHydration(sessionId)
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
      if (!isMountedRef.current || !sessionStillNeedsHydration(sessionId)) {
        return;
      }
      startSessionHydration(sessionId);
    }, delayMs);
    hydrationRetryTimersRef.current.set(sessionId, timerId);
  }

  function flushAndCancelPendingSessionRender(
    sessionSnapshot = sessionsRef.current,
  ) {
    if (pendingSessionRenderFrameRef.current !== null) {
      window.cancelAnimationFrame(pendingSessionRenderFrameRef.current);
      pendingSessionRenderFrameRef.current = null;
    }
    flushPendingSessionStoreSync(sessionSnapshot);
    hasPendingSessionRenderRef.current = false;
  }

  function flushPendingSessionRender() {
    pendingSessionRenderFrameRef.current = null;
    if (!hasPendingSessionRenderRef.current || !isMountedRef.current) {
      return;
    }

    hasPendingSessionRenderRef.current = false;
    const nextSessions = sessionsRef.current;
    flushPendingSessionStoreSync(nextSessions);
    startTransition(() => {
      setSessions(nextSessions);
    });
  }

  function scheduleSessionRender() {
    hasPendingSessionRenderRef.current = true;
    if (pendingSessionRenderFrameRef.current !== null) {
      return;
    }
    if (
      typeof window === "undefined" ||
      typeof window.requestAnimationFrame !== "function"
    ) {
      flushPendingSessionRender();
      return;
    }

    pendingSessionRenderFrameRef.current = window.requestAnimationFrame(
      flushPendingSessionRender,
    );
  }

  useEffect(() => {
    return () => {
      cancelPendingSessionRender();
      cancelPendingCodexStateRender();
      cancelHydrationRetries();
    };
  }, []);

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
    let nextWaits: DelegationWaitRecord[] | null = null;
    if (delta.type === "delegationWaitCreated") {
      const currentRevision = latestStateRevisionRef.current;
      if (currentRevision !== null && delta.revision < currentRevision) {
        return;
      }
      nextWaits = applyDelegationWaitCreated(
        delegationWaitsRef.current,
        delta.wait,
      );
    } else if (delta.type === "delegationWaitConsumed") {
      nextWaits = applyDelegationWaitConsumed(
        delegationWaitsRef.current,
        delta.waitId,
      );
    }

    if (
      nextWaits === null ||
      areDelegationWaitRecordsEqual(delegationWaitsRef.current, nextWaits)
    ) {
      return;
    }

    delegationWaitsRef.current = nextWaits;
    setDelegationWaits(nextWaits);
  }

  function adoptSessions(
    nextSessions: Session[],
    options?: AdoptSessionsOptions,
  ) {
    const previousSessions = sessionsRef.current;
    const previousSessionsById = new Map(
      previousSessions.map((session) => [session.id, session]),
    );
    const mergedSessions = reconcileSessions(previousSessions, nextSessions, {
      disableMutationStampFastPath: options?.disableMutationStampFastPath,
      forceMessagesUnloaded: options?.forceMessagesUnloaded,
    });
    const changedSessions = mergedSessions.filter(
      (session) => previousSessionsById.get(session.id) !== session,
    );
    const availableSessionIds = new Set(
      mergedSessions.map((session) => session.id),
    );
    const removedSessionIds = new Set(
      previousSessions.flatMap((session) =>
        availableSessionIds.has(session.id) ? [] : [session.id],
      ),
    );
    const unhydratedSessionIds = new Set(
      mergedSessions.flatMap((session) =>
        session.messagesLoaded === false ? [session.id] : [],
      ),
    );
    const sessionsWithChangedWorkdir = new Set(
      mergedSessions.flatMap((session) => {
        const previousSession = previousSessionsById.get(session.id);
        return previousSession && previousSession.workdir !== session.workdir
          ? [session.id]
          : [];
      }),
    );
    const hasRemovedSessions = removedSessionIds.size > 0;
    const hasWorkdirInvalidations = sessionsWithChangedWorkdir.size > 0;
    const pendingOpenSessionId =
      options?.openSessionId ?? pendingRecoveryOpenSessionIdRef.current;
    const pendingPaneId =
      options?.openSessionId !== undefined
        ? (options.paneId ?? null)
        : (pendingRecoveryPaneIdRef.current ?? null);
    const canOpenPendingSession =
      pendingOpenSessionId !== undefined &&
      availableSessionIds.has(pendingOpenSessionId);
    // Avoid rewriting workspace state when an adopted snapshot preserves the
    // same reconciled sessions. Workspace autosave is keyed off `workspace`
    // identity, so an identity-only rewrite here can create a loop:
    // workspace PUT -> SSE state snapshot -> adoptSessions -> workspace save.
    const shouldReconcileWorkspace =
      mergedSessions !== previousSessions || canOpenPendingSession;

    sessionsRef.current = mergedSessions;
    if (changedSessions.length > 0 || hasRemovedSessions) {
      syncComposerSessionsStoreIncremental({
        changedSessions,
        draftsBySessionId: draftsBySessionIdRef.current,
        draftAttachmentsBySessionId: draftAttachmentsBySessionIdRef.current,
        removedSessionIds: [...removedSessionIds],
      });
    }
    if (mergedSessions !== previousSessions) {
      flushAndCancelPendingSessionRender(mergedSessions);
    }
    startTransition(() => {
      if (mergedSessions !== previousSessions) {
        setSessions(mergedSessions);
      }
      if (shouldReconcileWorkspace) {
        setWorkspace((current) => {
          const reconciled =
            mergedSessions !== previousSessions
              ? applyControlPanelLayout(
                  reconcileWorkspaceState(current, mergedSessions),
                )
              : current;
          if (!canOpenPendingSession || !pendingOpenSessionId) {
            return reconciled;
          }

          return applyControlPanelLayout(
            openSessionInWorkspaceState(
              reconciled,
              pendingOpenSessionId,
              pendingPaneId,
            ),
          );
        });
      }
      if (hasRemovedSessions) {
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
      for (const sessionId of hydrationRetryTimersRef.current.keys()) {
        if (!availableSessionIds.has(sessionId)) {
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

    const previousSessions = sessionsRef.current;
    const existingIndex = previousSessions.findIndex(
      (session) => session.id === created.sessionId,
    );
    const nextSessionCandidates =
      existingIndex === -1
        ? [...previousSessions, created.session]
        : previousSessions.map((session, index) =>
            index === existingIndex ? created.session : session,
          );
    const nextSessions = reconcileSessions(
      previousSessions,
      nextSessionCandidates,
    );
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
    sessionsRef.current = nextSessions;
    upsertSessionSlice(adoptedSession);
    flushAndCancelPendingSessionRender(nextSessions);
    setSessions(nextSessions);
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
          : "fullSession",
      messageCount: getHydrationMessageCount(session),
      revision: latestStateRevisionRef.current,
      serverInstanceId: lastSeenServerInstanceIdRef.current,
      sessionMutationStamp: getHydrationMutationStamp(session),
    };
  }

  // Returns a discriminated outcome because metadata mismatch needs different
  // recovery depending on direction: stale responses retry hydration, while a
  // full-session response ahead of the current summary must first force
  // `/api/state` so the global revision/session metadata catches up.
  function adoptFetchedSession(
    session: Session,
    revision: number,
    serverInstanceId: string,
    requestContext: SessionHydrationRequestContext,
  ): AdoptFetchedSessionOutcome {
    const previousRevision = latestStateRevisionRef.current;
    const latestSessions = sessionsRef.current;
    const latestExistingIndex = latestSessions.findIndex(
      (entry) => entry.id === session.id,
    );
    const latestCurrentSession =
      latestExistingIndex === -1 ? null : latestSessions[latestExistingIndex];
    const adoptOutcome = classifyFetchedSessionAdoption({
      responseSession: session,
      responseRevision: revision,
      responseServerInstanceId: serverInstanceId,
      requestContext,
      currentSession: latestCurrentSession,
      currentRevision: previousRevision,
      currentServerInstanceId: lastSeenServerInstanceIdRef.current,
      seenServerInstanceIds: seenServerInstanceIdsRef.current,
    });
    if (
      (adoptOutcome !== "adopted" && adoptOutcome !== "partial") ||
      latestExistingIndex === -1
    ) {
      return adoptOutcome;
    }

    const hydratedSession = {
      ...session,
      messagesLoaded: adoptOutcome === "adopted",
    };
    const nextSessions = latestSessions.map((entry, index) =>
      index === latestExistingIndex ? hydratedSession : entry,
    );
    if (previousRevision === null || revision > previousRevision) {
      // A fresh server instance starts a new revision counter, so adopting its
      // targeted hydration response may legitimately step this ref backward.
      // Cross-instance ordering is handled by the server-instance gate above.
      latestStateRevisionRef.current = revision;
    }
    if (serverInstanceId) {
      rememberServerInstanceId(seenServerInstanceIdsRef, serverInstanceId);
      lastSeenServerInstanceIdRef.current = serverInstanceId;
    }
    sessionsRef.current = nextSessions;
    upsertSessionSlice(hydratedSession);
    flushAndCancelPendingSessionRender(nextSessions);
    setSessions(nextSessions);
    hydrationMismatchSessionIdsRef.current.delete(session.id);
    return adoptOutcome;
  }

  function startSessionHydration(
    sessionId: string,
    options?: SessionHydrationOptions,
  ) {
    if (hydratingSessionIdsRef.current.has(sessionId)) {
      if (options?.queueAfterCurrent === true) {
        queuedHydrationSessionIdsRef.current.add(sessionId);
      }
      if (options?.allowDivergentTextRepairAfterNewerRevision === true) {
        queuedTextRepairHydrationSessionIdsRef.current.add(sessionId);
      }
      return;
    }

    hydratingSessionIdsRef.current.add(sessionId);
    const requestContext = captureHydrationRequestContext(sessionId, options);
    if (!requestContext) {
      hydratingSessionIdsRef.current.delete(sessionId);
      return;
    }
    void (async () => {
      let shouldRetryHydration = false;
      let retryHydrationWithCap = false;
      try {
        let attemptedTailHydration = false;
        if (shouldStartTailFirstHydration(sessionId, options)) {
          attemptedTailHydration = true;
          const tailResponse = await fetchSessionTail(
            sessionId,
            SESSION_TAIL_WINDOW_MESSAGE_COUNT,
          );
          if (!isMountedRef.current) {
            return;
          }
          if (tailResponse.session.id !== sessionId) {
            if (!hydrationMismatchSessionIdsRef.current.has(sessionId)) {
              hydrationMismatchSessionIdsRef.current.add(sessionId);
              requestActionRecoveryResyncRef.current();
            }
            return;
          }

          const tailAdoptOutcome = adoptFetchedSession(
            tailResponse.session,
            tailResponse.revision,
            tailResponse.serverInstanceId,
            {
              ...requestContext,
              kind: "partialTail",
            },
          );
          switch (tailAdoptOutcome) {
            case "partial":
              break;
            case "adopted":
              completeSessionHydration(sessionId);
              return;
            case "restartResync":
              hydrationRestartResyncPendingRef.current = true;
              requestActionRecoveryResyncRef.current();
              return;
            case "stateResync":
              requestActionRecoveryResyncRef.current();
              shouldRetryHydration = true;
              return;
            case "stale":
              break;
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
        // Recapture so the full-fetch classifier sees metadata mutated by
        // partial tail adoption above.
        const fullRequestContext =
          captureHydrationRequestContext(sessionId, options) ?? requestContext;
        const response = await fetchSession(sessionId);
        if (!isMountedRef.current) {
          return;
        }
        if (response.session.id !== sessionId) {
          // Suppressed until the next authoritative state adoption clears the
          // set. A timer reset would re-open the mismatch -> resync loop.
          if (!hydrationMismatchSessionIdsRef.current.has(sessionId)) {
            hydrationMismatchSessionIdsRef.current.add(sessionId);
            requestActionRecoveryResyncRef.current();
          }
          return;
        }
        const adoptOutcome = fullFetchAdoptFetchedSessionOutcome(
          adoptFetchedSession(
            response.session,
            response.revision,
            response.serverInstanceId,
            fullRequestContext,
          ),
        );
        switch (adoptOutcome) {
          case "adopted":
            completeSessionHydration(sessionId);
            break;
          case "restartResync":
            hydrationRestartResyncPendingRef.current = true;
            requestActionRecoveryResyncRef.current();
            // The recovery state probe is the authoritative path after a
            // backend restart. Do not stack a session retry on top of it.
            break;
          case "stateResync":
            requestActionRecoveryResyncRef.current();
            shouldRetryHydration = true;
            break;
          case "stale": {
            // The classifier returns `"stale"` for several distinct
            // reasons (see `classifyFetchedSessionAdoption`):
            //   (a) the response is metadata-only
            //       (`responseSession.messagesLoaded !== true`) —
            //       the backend has not hydrated the full transcript
            //       yet (typical for unloaded remote-proxy sessions
            //       awaiting upstream fetch). Retrying is useful:
            //       the backend will eventually load and the next
            //       fetch will adopt.
            //   (b) the response is fully loaded but local is also
            //       fully loaded AND has advanced past the response
            //       (revision / message_count / mutation_stamp
            //       skew). SSE deltas arrived during the
            //       `/api/sessions/{id}` round-trip and bumped local
            //       state past what the response captured. Retrying
            //       is FUTILE: the SSE stream is faster than the
            //       REST round-trip, so the next fetch will race the
            //       same way and lose again, producing an infinite
            //       refetch loop during any active streaming turn
            //       whose delta cadence is faster than the round-
            //       trip (observed in practice: hundreds of MB of
            //       `/api/sessions/{id}` traffic during a single
            //       Codex table-printing turn). Local state is at
            //       least as recent as the response anyway —
            //       nothing to gain.
            //   (c) local is summary-only (`messagesLoaded === false`)
            //       and the response IS fully loaded but the
            //       classifier rejected adoption (e.g., concurrent
            //       delta bumped local metadata past the response's
            //       snapshot, so `requestStillMatches` /
            //       `responseMatches` failed). Retrying IS useful:
            //       the next fetch should land the canonical
            //       transcript that the metadata-only delta could
            //       not provide.
            //
            // Skip retry only for case (b): both local AND response
            // hydrated. Cases (a) and (c) keep the existing retry
            // behaviour. See bugs.md "Hydration retry loop can spam
            // persistent failures".
            const localSession = sessionsRef.current.find(
              (entry) => entry.id === sessionId,
            );
            const localFullyHydrated = localSession?.messagesLoaded === true;
            const responseFullyHydrated =
              response.session.messagesLoaded === true;
            if (localFullyHydrated && responseFullyHydrated) {
              clearHydrationRetry(sessionId);
            } else {
              shouldRetryHydration = true;
            }
            break;
          }
          default: {
            const _exhaustive: never = adoptOutcome;
            void _exhaustive;
            shouldRetryHydration = true;
            break;
          }
        }
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
        // posture; the transport shape differs (that one returns
        // `null` at the API boundary so callers treat it as "no
        // layout yet"; here `fetchSession` throws
        // `ApiRequestError` and we branch on `instanceof` + status
        // at the call site).
        if (error instanceof ApiRequestError && error.status === 404) {
          requestActionRecoveryResyncRef.current();
          return;
        }
        reportRequestError(error);
        shouldRetryHydration = true;
        retryHydrationWithCap = true;
      } finally {
        hydratingSessionIdsRef.current.delete(sessionId);
        if (
          queuedTextRepairHydrationSessionIdsRef.current.delete(sessionId) &&
          isMountedRef.current
        ) {
          startSessionHydration(sessionId, {
            allowDivergentTextRepairAfterNewerRevision: true,
          });
          return;
        }
        if (
          queuedHydrationSessionIdsRef.current.delete(sessionId) &&
          isMountedRef.current
        ) {
          startSessionHydration(sessionId);
          return;
        }
        if (shouldRetryHydration) {
          scheduleHydrationRetry(sessionId, {
            capAttempts: retryHydrationWithCap,
          });
        }
      }
    })();
  }

  useEffect(() => {
    const targetMessagesLoadedBySessionId = new Map<
      string,
      boolean | null | undefined
    >();
    if (activeSession) {
      targetMessagesLoadedBySessionId.set(
        activeSession.id,
        activeSession.messagesLoaded,
      );
    }
    for (const target of visibleSessionHydrationTargets) {
      if (!targetMessagesLoadedBySessionId.has(target.id)) {
        targetMessagesLoadedBySessionId.set(target.id, target.messagesLoaded);
      }
    }

    const sessionIdsToHydrate = [...targetMessagesLoadedBySessionId.entries()]
      .filter(([, messagesLoaded]) => messagesLoaded === false)
      .map(([sessionId]) => sessionId);
    if (sessionIdsToHydrate.length === 0) {
      return;
    }

    for (const sessionId of sessionIdsToHydrate) {
      startSessionHydration(sessionId);
    }
    // Deps intentionally do NOT include message counts:
    // the body only reads target ids and `messagesLoaded` flags.
    // Visible session pane changes and the one-shot
    // `messagesLoaded: false -> true` transition are the only
    // signals this effect cares about.
  }, [
    activeSession?.id,
    activeSession?.messagesLoaded,
    visibleSessionHydrationTargets,
  ]);

  function syncPreferencesFromState(nextState: StateResponse) {
    const preferences = resolveAppPreferences(nextState.preferences);
    setDefaultCodexModel(preferences.defaultCodexModel);
    setDefaultClaudeModel(preferences.defaultClaudeModel);
    setDefaultCursorModel(preferences.defaultCursorModel);
    setDefaultGeminiModel(preferences.defaultGeminiModel);
    setDefaultCodexReasoningEffort(preferences.defaultCodexReasoningEffort);
    setDefaultClaudeApprovalMode(preferences.defaultClaudeApprovalMode);
    setDefaultClaudeEffort(preferences.defaultClaudeEffort);
    setRemoteConfigs((current) =>
      areRemoteConfigsEqual(current, preferences.remotes)
        ? current
        : preferences.remotes,
    );
  }

  function adoptState(nextState: StateResponse, options?: AdoptStateOptions) {
    if (!isMountedRef.current) {
      return false;
    }

    const fullStateServerInstanceChanged =
      !!nextState.serverInstanceId &&
      nextState.serverInstanceId !== lastFullStateServerInstanceIdRef.current;
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

    latestStateRevisionRef.current = nextState.revision;
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
      hydratingSessionIdsRef.current.clear();
      hydratedSessionIdsRef.current.clear();
      queuedHydrationSessionIdsRef.current.clear();
      queuedTextRepairHydrationSessionIdsRef.current.clear();
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
    if (!areDelegationWaitRecordsEqual(delegationWaitsRef.current, nextDelegationWaits)) {
      delegationWaitsRef.current = nextDelegationWaits;
      setDelegationWaits(nextDelegationWaits);
    }
    if (adoptedStateSlices.workspaces !== currentWorkspaceSummaries) {
      workspaceSummariesRef.current = adoptedStateSlices.workspaces;
      setWorkspaceSummaries(adoptedStateSlices.workspaces);
    }
    const requestedOpenSessionId =
      options?.openSessionId ?? pendingRecoveryOpenSessionIdRef.current;
    adoptSessions(
      nextState.sessions,
      resolveAdoptStateSessionOptions(options, fullStateServerInstanceChanged),
    );
    // Local state adoptions can resume or create active sessions before any SSE arrives.
    syncAdoptedLiveSessionResumeWatchdogBaselinesRef.current(
      nextState.sessions,
    );
    if (requestedOpenSessionId) {
      const openedSession = nextState.sessions.find(
        (session) => session.id === requestedOpenSessionId,
      );
      if (openedSession) {
        setSelectedProjectId(openedSession.projectId ?? ALL_PROJECTS_FILTER_ID);
      }
    }
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

  useEffect(() => {
    let cancelled = false;
    let initialStateResyncRetryTimeoutId: ReturnType<
      typeof window.setTimeout
    > | null = null;
    let reconnectStateResyncTimeoutId: ReturnType<
      typeof window.setTimeout
    > | null = null;
    let sawReconnectOpenSinceLastError = false;
    let reconnectRecoveryConfirmedSinceLastError = false;
    // Some browsers/proxies can resume delivering authoritative `state` frames
    // after an error without an observable `onopen` in our handler order. Delta
    // frames can be buffered from before the error, so only state events or
    // cause-specific recovery contracts may use this as no-open live proof.
    let reconnectErrorPendingLiveProof = false;
    // True only between a bad reopened SSE payload and the next confirmed live
    // event or outage reset. This separates "bad SSE needs proof" from
    // ordinary reconnect wake-gap polling after `/api/state` progress.
    // Invariant: producers set this only when sawReconnectOpenSinceLastError
    // is true, and onerror clears both flags together for the next outage.
    let pendingBadLiveEventRecovery = false;
    let allowReconnectRecoveryWithoutExplicitOpen = false;
    let delegationRepairAdoptedSinceLastReconnectError = false;
    let lastDelegationRepairRequestedRevision: number | null = null;
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
      if (!sawReconnectOpenSinceLastError && !allowWithoutConfirmedOpen) {
        return;
      }

      reconnectRecoveryConfirmedSinceLastError = true;
      clearReconnectStateResyncTimeout();
      resetReconnectStateResyncBackoff();
    }

    function clearDelegationRepairReconnectProof() {
      delegationRepairAdoptedSinceLastReconnectError = false;
      lastDelegationRepairRequestedRevision = null;
    }

    function confirmReconnectRecoveryFromLiveEvent({
      allowWithoutConfirmedOpen = false,
    }: { allowWithoutConfirmedOpen?: boolean } = {}): boolean {
      const canConfirmWithoutOpen =
        allowWithoutConfirmedOpen &&
        (allowReconnectRecoveryWithoutExplicitOpen ||
          reconnectErrorPendingLiveProof);
      if (!sawReconnectOpenSinceLastError && !canConfirmWithoutOpen) {
        return false;
      }

      clearReconnectStateResyncTimeoutAfterConfirmedReopen({
        allowWithoutConfirmedOpen: canConfirmWithoutOpen,
      });
      pendingBadLiveEventRecovery = false;
      allowReconnectRecoveryWithoutExplicitOpen = false;
      reconnectErrorPendingLiveProof = false;
      clearDelegationRepairReconnectProof();
      setBackendConnectionState("connected");
      return true;
    }

    function confirmReconnectRecoveryFromDeltaEvent() {
      return confirmReconnectRecoveryFromLiveEvent({
        allowWithoutConfirmedOpen:
          allowReconnectRecoveryWithoutExplicitOpen,
      });
    }

    function confirmReconnectRecoveryFromStateEvent() {
      const allowWithoutConfirmedOpen =
        allowReconnectRecoveryWithoutExplicitOpen ||
        reconnectErrorPendingLiveProof;
      return confirmReconnectRecoveryFromLiveEvent({
        allowWithoutConfirmedOpen,
      });
    }

    function confirmReconnectRecoveryFromAuthoritativeSnapshot() {
      reconnectRecoveryConfirmedSinceLastError = true;
      pendingBadLiveEventRecovery = false;
      allowReconnectRecoveryWithoutExplicitOpen = false;
      reconnectErrorPendingLiveProof = false;
      clearReconnectStateResyncTimeout();
      resetReconnectStateResyncBackoff();
      clearDelegationRepairReconnectProof();
      setBackendConnectionState("connected");
    }

    function beginBadLiveEventRecovery() {
      pendingBadLiveEventRecovery = true;
      reconnectRecoveryConfirmedSinceLastError = false;
      allowReconnectRecoveryWithoutExplicitOpen = false;
      clearDelegationRepairReconnectProof();
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
            } = consumePendingStateResyncOptions(
              pendingStateResyncOptionsRef,
            );
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
                  reconnectRecoveryConfirmedSinceLastError
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
                  !reconnectRecoveryConfirmedSinceLastError &&
                  lastDelegationRepairRequestedRevision !== null &&
                  forceAdoptEqualOrNewerRevision !== null &&
                  state.revision >= lastDelegationRepairRequestedRevision
                ) {
                  delegationRepairAdoptedSinceLastReconnectError = true;
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
                !reconnectRecoveryConfirmedSinceLastError &&
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
                  allowReconnectRecoveryWithoutExplicitOpen = true;
                }
                // Timer-driven reconnect fallbacks keep probing until a live
                // EventSource data frame proves the SSE stream is healthy
                // again. A same-instance /api/state snapshot can refresh the
                // visible UI, but polling success alone does not prove later
                // assistant deltas will arrive.
                const shouldRearmUntilLiveEvent =
                  rearmUntilLiveEventOnSuccess &&
                  !reconnectRecoveryConfirmedSinceLastError;
                const carryManualRetryContractForward =
                  rearmAfterSameInstanceProgressUntilLiveEvent &&
                  shouldRearmUntilLiveEvent;
                // Keep polling after success only for one of these contracts:
                // explicit live-SSE proof is still required, a bad reopened SSE
                // event needs recovery, no newer snapshot progress was made, or
                // a replacement instance was adopted and should be confirmed.
                const shouldRearmAfterSuccess =
                  shouldRearmUntilLiveEvent ||
                  (pendingBadLiveEventRecovery &&
                    !reconnectRecoveryConfirmedSinceLastError) ||
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
        reconnectRecoveryConfirmedSinceLastError &&
        !options?.preserveReconnectFallback
      ) {
        clearReconnectStateResyncTimeout();
      }
      // Otherwise preserve the armed reconnect fallback; startStateResyncLoop()
      // will only disarm it once a /api/state snapshot is actually adopted.
      // Coalesced resync requests retain the strongest semantics until the next
      // loop iteration consumes them.
      pendingStateResyncOptionsRef.current =
        coalescePendingStateResyncOptions(
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

      sawReconnectOpenSinceLastError = false;
      reconnectRecoveryConfirmedSinceLastError = false;
      pendingBadLiveEventRecovery = false;
      allowReconnectRecoveryWithoutExplicitOpen = false;
      reconnectErrorPendingLiveProof = false;
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

    function handleStateEvent(event: MessageEvent<string>) {
      if (cancelled) {
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
          if (!pendingBadLiveEventRecovery) {
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
        if (adopted || !pendingBadLiveEventRecovery) {
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
        if (!cancelled) {
          setBackendConnectionIssueDetail(
            describeBackendConnectionIssueDetail(error),
          );
          // A bad reconnect state payload must not leave the client marked as
          // connected without a usable snapshot. Restore "reconnecting" so the
          // retry affordance stays available (onopen already set "connected"),
          // and re-arm fallback polling so recovery continues via /api/state.
          if (sawReconnectOpenSinceLastError) {
            beginBadLiveEventRecovery();
          }
        }
      } finally {
        if (!cancelled) {
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
      if (cancelled) {
        return;
      }

      try {
        const delta = JSON.parse(event.data) as DeltaEvent;
        const currentRevision = latestStateRevisionRef.current;
        if (isDelegationDeltaEvent(delta)) {
          applyDelegationWaitDeltaLocally(delta);
          if (currentRevision === null || delta.revision >= currentRevision) {
            if (delegationRepairAdoptedSinceLastReconnectError) {
              confirmReconnectRecoveryFromDeltaEvent();
            }
            lastDelegationRepairRequestedRevision = delta.revision;
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
          } else if (!pendingBadLiveEventRecovery) {
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
            pendingBadLiveEventRecovery &&
            latestStateRevisionRef.current !== null &&
            delta.revision === latestStateRevisionRef.current;
          if (
            !pendingBadLiveEventRecovery ||
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
        if (sawReconnectOpenSinceLastError) {
          beginBadLiveEventRecovery();
        } else {
          requestStateResync({ rearmOnFailure: true });
        }
      }
    }

    function handleWorkspaceFilesChangedEvent(event: MessageEvent<string>) {
      if (cancelled) {
        return;
      }

      try {
        const filesChanged = JSON.parse(
          event.data,
        ) as WorkspaceFilesChangedEvent;
        if (!pendingBadLiveEventRecovery) {
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
      if (cancelled) {
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

    eventSource.addEventListener("state", handleStateEvent as EventListener);
    eventSource.addEventListener("delta", handleDeltaEvent as EventListener);
    eventSource.addEventListener("lagged", handleLaggedEvent as EventListener);
    eventSource.addEventListener(
      "workspaceFilesChanged",
      handleWorkspaceFilesChangedEvent as EventListener,
    );
    eventSource.onopen = () => {
      if (!cancelled) {
        resetWorkspaceFilesChangedEventGate();
        sawReconnectOpenSinceLastError = true;
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
    };
    eventSource.onerror = () => {
      if (cancelled) {
        return;
      }

      const hadReconnectOpenSinceLastError = sawReconnectOpenSinceLastError;
      sawReconnectOpenSinceLastError = false;
      reconnectRecoveryConfirmedSinceLastError = false;
      pendingBadLiveEventRecovery = false;
      allowReconnectRecoveryWithoutExplicitOpen = false;
      reconnectErrorPendingLiveProof = true;
      clearDelegationRepairReconnectProof();
      clearForceAdoptNextStateEvent();
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
      const readyState = (eventSource as { readyState?: unknown }).readyState;
      const eventSourceClosed =
        typeof readyState === "number" && readyState === 2;
      if (eventSourceClosed && isOnline) {
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

      const readyState = (eventSource as { readyState?: unknown }).readyState;
      const isOpen = typeof readyState === "number" && readyState === 1;
      if (isOpen) {
        sseStaleSinceMs = null;
        return;
      }
      if (typeof readyState !== "number") {
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
