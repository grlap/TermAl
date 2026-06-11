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
import { ALL_PROJECTS_FILTER_ID } from "./project-filters";
import {
  isServerInstanceMismatch,
  shouldAdoptSnapshotRevision,
} from "./state-revision";
import {
  type PendingStateResyncOptions,
} from "./app-live-state-resync-options";
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
import {
  applyDelegationParentIdsFromSummaries,
  reconcileSessions,
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
  fullFetchAdoptFetchedSessionOutcome,
  resolveAdoptStateSessionOptions,
  SESSION_HYDRATION_MAX_RETRY_ATTEMPTS,
  SESSION_HYDRATION_RETRY_DELAYS_MS,
  SESSION_TAIL_FULL_HYDRATION_COMPOSER_BUSY_RETRY_MS,
} from "./app-live-state-hydration";
import {
  cancelDeferredFullHydrationTimers,
  clearDeferredFullHydrationTimer,
  scheduleDeferredFullHydration as scheduleDeferredFullHydrationTimer,
  shouldDelayFullHydrationStartForComposer as shouldDelayFullHydrationStartForComposerFromScheduler,
  shouldPromoteDeferredFullHydration,
  type DeferredFullHydrationHandle,
  type SessionHydrationOptions,
} from "./app-live-state-deferred-hydration";
import {
  addSessionFullHydrationDemandListener,
} from "./session-hydration-demand";
import {
  enqueueWorkspaceFilesChangedEvent as enqueueWorkspaceFilesChangedEventInGate,
  flushWorkspaceFilesChangedEventBuffer as flushWorkspaceFilesChangedEventGateBuffer,
  resetWorkspaceFilesChangedEventGate as resetWorkspaceFilesChangedEventGateRefs,
  type WorkspaceFilesChangedEventGateRefs,
} from "./app-live-state-workspace-events";
import { useAppLiveStateRenderSchedulers } from "./app-live-state-render-schedulers";
import { useAppLiveStateTransport } from "./app-live-state-transport";
import { reconcileAdoptedSessionsWorkspace } from "./app-live-state-workspace-reconciliation";

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
  SESSION_TAIL_FULL_HYDRATION_COMPOSER_BUSY_HARD_TIMEOUT_MS,
  SESSION_TAIL_FULL_HYDRATION_COMPOSER_BUSY_RETRY_MS,
  SESSION_TAIL_FULL_HYDRATION_DEFER_MS,
} from "./app-live-state-hydration";

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
    setHasAdoptedStateSnapshot,
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
    setTelegramConfig,
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
  const deferredFullHydrationTimersRef = useRef<
    Map<string, DeferredFullHydrationHandle>
  >(new Map());
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
    setSessions,
  });

  function upsertSessionSlice(session: Session) {
    upsertSessionStoreSession({
      session,
      committedDraft: draftsBySessionIdRef.current[session.id] ?? "",
      draftAttachments:
        draftAttachmentsBySessionIdRef.current[session.id] ?? [],
    });
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
    clearDeferredFullHydration(sessionId);
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

  function clearDeferredFullHydration(sessionId: string) {
    clearDeferredFullHydrationTimer(deferredFullHydrationTimersRef, sessionId);
  }

  function cancelDeferredFullHydrations() {
    cancelDeferredFullHydrationTimers(deferredFullHydrationTimersRef);
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

  function shouldDelayFullHydrationStartForComposer(
    sessionId: string,
    options?: SessionHydrationOptions,
  ) {
    return shouldDelayFullHydrationStartForComposerFromScheduler({
      sessionId,
      options,
      sessionStillNeedsHydration,
      shouldStartTailFirstHydration,
    });
  }

  function scheduleDeferredFullHydration(
    sessionId: string,
    options: {
      autoStart?: boolean;
      delayMs?: number;
      firstScheduledAtMs?: number;
    } = {},
  ) {
    scheduleDeferredFullHydrationTimer({
      timersRef: deferredFullHydrationTimersRef,
      isMountedRef,
      sessionId,
      sessionStillNeedsHydration,
      startSessionHydration,
      options,
    });
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

  useEffect(() => {
    return () => {
      cancelHydrationRetries();
      cancelDeferredFullHydrations();
    };
  }, []);

  useEffect(
    () =>
      addSessionFullHydrationDemandListener(({ sessionId }) => {
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
    const mergedSessions = reconcileSessions(previousSessions, nextSessions, {
      disableMutationStampFastPath: options?.disableMutationStampFastPath,
      forceMessagesUnloaded: options?.forceMessagesUnloaded,
    });
    const pendingOpenSessionId =
      options?.openSessionId ?? pendingRecoveryOpenSessionIdRef.current;
    const pendingPaneId =
      options?.openSessionId !== undefined
        ? (options.paneId ?? null)
        : (pendingRecoveryPaneIdRef.current ?? null);
    const shouldPruneDelegatedChildWorkspaceTabs =
      options?.pruneDelegatedChildWorkspaceTabs === true;

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
            return previousSession && previousSession.workdir !== session.workdir
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
      for (const sessionId of deferredFullHydrationTimersRef.current.keys()) {
        if (!availableSessionIds.has(sessionId)) {
          clearDeferredFullHydration(sessionId);
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
    const currentSession = latestSessions[latestExistingIndex];

    const hydratedSession = {
      ...session,
      messagesLoaded: adoptOutcome === "adopted",
    };
    const reconciledHydratedSession = reconcileSingleSession(
      currentSession,
      hydratedSession,
      { disableMutationStampFastPath: true },
    );
    const nextSessions = latestSessions.map((entry, index) =>
      index === latestExistingIndex ? reconciledHydratedSession : entry,
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
    upsertSessionSlice(reconciledHydratedSession);
    flushAndCancelPendingSessionRender(nextSessions);
    setSessions(nextSessions);
    hydrationMismatchSessionIdsRef.current.delete(session.id);
    return adoptOutcome;
  }

  function startSessionHydration(
    sessionId: string,
    options?: SessionHydrationOptions,
  ) {
    if (
      options?.fromDeferredFullHydration !== true &&
      deferredFullHydrationTimersRef.current.has(sessionId)
    ) {
      if (shouldPromoteDeferredFullHydration(options)) {
        clearDeferredFullHydration(sessionId);
      } else {
        return;
      }
    }
    if (options?.fromDeferredFullHydration === true) {
      clearDeferredFullHydration(sessionId);
    }
    if (hydratingSessionIdsRef.current.has(sessionId)) {
      if (options?.queueAfterCurrent === true) {
        queuedHydrationSessionIdsRef.current.add(sessionId);
      }
      if (options?.allowDivergentTextRepairAfterNewerRevision === true) {
        queuedTextRepairHydrationSessionIdsRef.current.add(sessionId);
      }
      return;
    }
    if (shouldDelayFullHydrationStartForComposer(sessionId, options)) {
      scheduleDeferredFullHydration(sessionId, {
        delayMs: SESSION_TAIL_FULL_HYDRATION_COMPOSER_BUSY_RETRY_MS,
      });
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
              queuedHydrationSessionIdsRef.current.delete(sessionId);
              if (!sessionStillNeedsHydration(sessionId)) {
                completeSessionHydration(sessionId);
                return;
              }
              scheduleDeferredFullHydration(sessionId, { autoStart: false });
              return;
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
          startSessionHydration(sessionId, { queueAfterCurrent: true });
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
    setTelegramConfig((current) =>
      areTelegramUiConfigsEqual(current, preferences.telegram)
        ? current
        : preferences.telegram,
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
      hydratingSessionIdsRef.current.clear();
      hydratedSessionIdsRef.current.clear();
      queuedHydrationSessionIdsRef.current.clear();
      queuedTextRepairHydrationSessionIdsRef.current.clear();
      cancelDeferredFullHydrations();
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
      applyDelegationParentIdsFromSummaries(
        nextState.sessions,
        nextState.delegations ?? [],
      ),
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

  useAppLiveStateTransport({
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
