// app-live-state.ts
//
// Owns: the full live-state transport plumbing that used to live
// inline in App.tsx. That includes the SSE `state` / `delta` /
// `workspaceFilesChanged` handler bodies, the adoption helpers
// (`adoptState`, `adoptSessions`, `adoptCreatedSessionResponse`,
// `adoptFetchedSession`, `syncPreferencesFromState`), the
// workspace-files-changed buffering gate
// (`flushWorkspaceFilesChangedEventBuffer`,
// `resetWorkspaceFilesChangedEventGate`,
// `enqueueWorkspaceFilesChangedEvent` plus the buffer / flush
// timeout / revision-gate refs), the `forceAdoptNextStateEventRef`
// refresh flag, the session hydration fetch effect, the
// `hydratedSessionIdsRef` / `hydratingSessionIdsRef` tracking
// refs, AND (as of Slice 13B) the EventSource open/close
// lifecycle, reconnect fallback timer orchestration, watchdog
// timer orchestration, visibility / focus / pagehide / pageshow
// recovery handlers, the reconnect/watchdog coordination helpers
// (`confirmReconnectRecoveryFromLiveEvent`, `requestStateResync`,
// `markLiveTransportActivity`, etc.), and the per-mount
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
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import {
  syncComposerSessionsStoreIncremental,
  upsertSessionStoreSession,
} from "./session-store";
import {
  ApiRequestError,
  fetchSession,
  fetchState,
  isBackendUnavailableError,
  type CreateSessionResponse,
  type StateResponse,
} from "./api";
import {
  applyDeltaToSessions,
  LIVE_SESSION_RESUME_WATCHDOG_DRIFT_MS,
  LIVE_SESSION_WATCHDOG_RESYNC_RETRY_COOLDOWN_MS,
  pruneLiveTransportActivitySessions,
  sessionHasPotentiallyStaleTransport,
} from "./live-updates";
import {
  areRemoteConfigsEqual,
  describeUnknownSessionModelWarning,
  resolveAppPreferences,
  unknownSessionModelConfirmationKey,
} from "./session-model-utils";
import { resolveAdoptedStateSlices } from "./state-adoption";
import { ALL_PROJECTS_FILTER_ID } from "./project-filters";
import {
  decideDeltaRevisionAction,
  shouldAdoptSnapshotRevision,
} from "./state-revision";
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
  ClaudeApprovalMode,
  ClaudeEffortLevel,
  CodexReasoningEffort,
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
import { mergeWorkspaceFilesChangedEvents } from "./workspace-file-events";
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

// Outcome of `adoptCreatedSessionResponse`:
//   - "adopted":    session inserted into `sessionsRef` and the
//                   workspace pane opened. Nothing for the caller
//                   to do.
//   - "stale":      revision gate rejected the write because a
//                   newer snapshot already landed. The session
//                   was NOT inserted here. Callers must verify
//                   `sessionsRef` already contains
//                   `created.sessionId` before opening a pane;
//                   unrelated state probes can advance the
//                   revision without ever inserting the created
//                   session locally.
//   - "recovering": wire-contract violation
//                   (`session.id !== sessionId`) — a resync was
//                   scheduled and the caller MUST NOT open a
//                   workspace pane for the mismatched id. That
//                   id was never inserted into `sessionsRef`,
//                   so opening it would leave a phantom pane
//                   that persists until the resync reconciles.
//
// A plain boolean could not carry the "stale vs recovering"
// distinction, so the call-site fallback (`if (!adopted) { open
// workspace pane }`) opened a phantom on protocol mismatch.
export type AdoptCreatedSessionOutcome = "adopted" | "stale" | "recovering";

export type AdoptStateOptions = {
  force?: boolean;
  /** Allow adopting a snapshot with a lower revision than the current one.
   *  Only used for backend restart rollbacks where the revision counter resets. */
  allowRevisionDowngrade?: boolean;
  openSessionId?: string;
  paneId?: string | null;
};

export type AdoptSessionsOptions = {
  openSessionId?: string;
  paneId?: string | null;
  disableMutationStampFastPath?: boolean;
};

export type RequestStateResyncOptions = {
  allowAuthoritativeRollback?: boolean;
  preserveReconnectFallback?: boolean;
  preserveWatchdogCooldown?: boolean;
  rearmOnSuccess?: boolean;
  rearmOnFailure?: boolean;
  openSessionId?: string;
  paneId?: string | null;
};

export type SessionHydrationTarget = {
  id: string;
  messagesLoaded?: boolean | null;
};

const SLOW_STATE_EVENT_WARNING_MS = 50;
const STATE_EVENT_METADATA_PEEK_CHARS = 4096;

function createStateEventProfiler() {
  if (
    !import.meta.env.DEV ||
    typeof performance === "undefined" ||
    typeof console === "undefined"
  ) {
    return null;
  }

  const startedAt = performance.now();
  let lastMarkAt = startedAt;
  const steps: string[] = [];

  return {
    mark(label: string) {
      const now = performance.now();
      steps.push(`${label}=${(now - lastMarkAt).toFixed(1)}ms`);
      lastMarkAt = now;
    },
    finish(details: {
      adopted?: boolean;
      revision?: number;
      sessionCount?: number;
    }) {
      const now = performance.now();
      const totalMs = now - startedAt;
      if (totalMs < SLOW_STATE_EVENT_WARNING_MS) {
        return;
      }

      const suffix = [
        `total=${totalMs.toFixed(1)}ms`,
        details.revision !== undefined ? `revision=${details.revision}` : null,
        details.adopted !== undefined ? `adopted=${details.adopted}` : null,
        details.sessionCount !== undefined
          ? `sessions=${details.sessionCount}`
          : null,
        ...steps,
      ]
        .filter(Boolean)
        .join(" ");
      console.warn(`[TermAl perf] slow state event ${suffix}`);
    },
  };
}

function extractTopLevelJsonNumber(payload: string, key: string) {
  const match = new RegExp(`"${key}"\\s*:\\s*(-?\\d+)`).exec(
    payload.slice(0, STATE_EVENT_METADATA_PEEK_CHARS),
  );
  if (!match?.[1]) {
    return null;
  }

  const value = Number(match[1]);
  return Number.isFinite(value) ? value : null;
}

function extractTopLevelJsonString(payload: string, key: string) {
  const match = new RegExp(
    `"${key}"\\s*:\\s*"([^"\\\\]*(?:\\\\.[^"\\\\]*)*)"`,
  ).exec(payload.slice(0, STATE_EVENT_METADATA_PEEK_CHARS));
  if (!match?.[1]) {
    return null;
  }

  try {
    return JSON.parse(`"${match[1]}"`) as string;
  } catch {
    return null;
  }
}

function payloadHasTopLevelTrueBoolean(payload: string, key: string) {
  return new RegExp(`"${key}"\\s*:\\s*true(?:\\s*[,}])`).test(
    payload.slice(0, STATE_EVENT_METADATA_PEEK_CHARS),
  );
}

export type UseAppLiveStateAdoptionRefs = {
  isMountedRef: MutableRefObject<boolean>;
  latestStateRevisionRef: MutableRefObject<number | null>;
  lastSeenServerInstanceIdRef: MutableRefObject<string | null>;
  sessionsRef: MutableRefObject<Session[]>;
  draftsBySessionIdRef: MutableRefObject<Record<string, string>>;
  draftAttachmentsBySessionIdRef: MutableRefObject<
    Record<string, DraftImageAttachment[]>
  >;
  codexStateRef: MutableRefObject<CodexState>;
  agentReadinessRef: MutableRefObject<AgentReadiness[]>;
  projectsRef: MutableRefObject<Project[]>;
  orchestratorsRef: MutableRefObject<OrchestratorInstance[]>;
  workspaceSummariesRef: MutableRefObject<WorkspaceLayoutSummary[]>;
  refreshingAgentCommandSessionIdsRef: MutableRefObject<SessionFlagMap>;
  confirmedUnknownModelSendsRef: MutableRefObject<Set<string>>;
  activePromptPollCancelRef: MutableRefObject<(() => void) | null>;
  activePromptPollSessionIdRef: MutableRefObject<string | null>;
};

export type UseAppLiveStateStateSetters = {
  setSessions: Dispatch<SetStateAction<Session[]>>;
  setWorkspace: Dispatch<SetStateAction<WorkspaceState>>;
  setCodexState: Dispatch<SetStateAction<CodexState>>;
  setAgentReadiness: Dispatch<SetStateAction<AgentReadiness[]>>;
  setProjects: Dispatch<SetStateAction<Project[]>>;
  setOrchestrators: Dispatch<SetStateAction<OrchestratorInstance[]>>;
  setWorkspaceSummaries: Dispatch<SetStateAction<WorkspaceLayoutSummary[]>>;
  setDraftsBySessionId: Dispatch<SetStateAction<Record<string, string>>>;
  setDraftAttachmentsBySessionId: Dispatch<
    SetStateAction<Record<string, DraftImageAttachment[]>>
  >;
  setSendingSessionIds: Dispatch<SetStateAction<SessionFlagMap>>;
  setStoppingSessionIds: Dispatch<SetStateAction<SessionFlagMap>>;
  setKillingSessionIds: Dispatch<SetStateAction<SessionFlagMap>>;
  setKillRevealSessionId: Dispatch<SetStateAction<string | null>>;
  setPendingKillSessionId: Dispatch<SetStateAction<string | null>>;
  setPendingSessionRename: Dispatch<
    SetStateAction<PendingSessionRename | null>
  >;
  setUpdatingSessionIds: Dispatch<SetStateAction<SessionFlagMap>>;
  setAgentCommandsBySessionId: Dispatch<SetStateAction<SessionAgentCommandMap>>;
  setRefreshingAgentCommandSessionIds: Dispatch<SetStateAction<SessionFlagMap>>;
  setAgentCommandErrors: Dispatch<SetStateAction<SessionErrorMap>>;
  setSessionSettingNotices: Dispatch<SetStateAction<SessionNoticeMap>>;
  setSelectedProjectId: Dispatch<SetStateAction<string>>;
  setIsLoading: Dispatch<SetStateAction<boolean>>;
  setBackendConnectionIssueDetail: Dispatch<SetStateAction<string | null>>;
  setBackendConnectionState: (next: BackendConnectionState) => void;
};

export type UseAppLiveStatePreferenceSetters = {
  setDefaultCodexReasoningEffort: Dispatch<
    SetStateAction<CodexReasoningEffort>
  >;
  setDefaultClaudeApprovalMode: Dispatch<SetStateAction<ClaudeApprovalMode>>;
  setDefaultClaudeEffort: Dispatch<SetStateAction<ClaudeEffortLevel>>;
  setRemoteConfigs: Dispatch<SetStateAction<RemoteConfig[]>>;
};

export type UseAppLiveStateParams = {
  adoptionRefs: UseAppLiveStateAdoptionRefs;
  stateSetters: UseAppLiveStateStateSetters;
  preferenceSetters: UseAppLiveStatePreferenceSetters;
  applyControlPanelLayout: (
    nextWorkspace: WorkspaceState,
    side?: ControlPanelSide,
  ) => WorkspaceState;
  clearRecoveredBackendRequestError: () => void;
  reportRequestError: (error: unknown, options?: { message?: string }) => void;
  /**
   * Populated by the hook's transport useEffect on mount so the
   * App.tsx manual-retry button and browser-online handler (which
   * run outside the hook) can trigger an immediate /api/state
   * probe. The hook resets this to a no-op during cleanup so
   * stale calls after unmount do nothing. App.tsx owns the ref
   * identity because it is called from helpers declared before
   * the hook is invoked.
   */
  requestBackendReconnectRef: MutableRefObject<() => void>;
  /**
   * Populated by the hook's transport useEffect on mount so the
   * App.tsx `reportRequestError` path can probe /api/state for a
   * one-off backend-unavailable error without touching SSE-level
   * reconnect state. Reset to a no-op on cleanup.
   */
  requestActionRecoveryResyncRef: MutableRefObject<
    (options?: { openSessionId?: string; paneId?: string | null }) => void
  >;
  activeSession: Session | null;
  visibleSessionHydrationTargets: readonly SessionHydrationTarget[];
};

export type UseAppLiveStateReturn = {
  adoptState: (
    nextState: StateResponse,
    options?: AdoptStateOptions,
  ) => boolean;
  adoptCreatedSessionResponse: (
    created: CreateSessionResponse,
    options?: { openSessionId?: string; paneId?: string | null },
  ) => AdoptCreatedSessionOutcome;
  syncPreferencesFromState: (nextState: StateResponse) => void;
  hydratedSessionIdsRef: MutableRefObject<Set<string>>;
  hydratingSessionIdsRef: MutableRefObject<Set<string>>;
  forceAdoptNextStateEventRef: MutableRefObject<boolean>;
  workspaceFilesChangedEvent: WorkspaceFilesChangedEvent | null;
  workspaceFilesChangedEventBufferRef: MutableRefObject<WorkspaceFilesChangedEvent | null>;
  workspaceFilesChangedEventFlushTimeoutRef: MutableRefObject<number | null>;
  resetWorkspaceFilesChangedEventGate: () => void;
};

function buildUnknownModelConfirmationKeySet(sessions: Session[]) {
  return new Set(
    sessions
      .filter((session) => describeUnknownSessionModelWarning(session))
      .map((session) =>
        unknownSessionModelConfirmationKey(session.id, session.model),
      ),
  );
}

function setContainsOnlyValuesFrom<T>(current: Set<T>, allowed: Set<T>) {
  for (const value of current) {
    if (!allowed.has(value)) {
      return false;
    }
  }

  return true;
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
    sessionsRef,
    draftsBySessionIdRef,
    draftAttachmentsBySessionIdRef,
    codexStateRef,
    agentReadinessRef,
    projectsRef,
    orchestratorsRef,
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
    setDefaultCodexReasoningEffort,
    setDefaultClaudeApprovalMode,
    setDefaultClaudeEffort,
    setRemoteConfigs,
  } = preferenceSetters;

  const hydratingSessionIdsRef = useRef<Set<string>>(new Set());
  const hydratedSessionIdsRef = useRef<Set<string>>(new Set());
  const forceAdoptNextStateEventRef = useRef(false);

  const [workspaceFilesChangedEvent, setWorkspaceFilesChangedEvent] =
    useState<WorkspaceFilesChangedEvent | null>(null);
  const workspaceFilesChangedEventBufferRef =
    useRef<WorkspaceFilesChangedEvent | null>(null);
  const workspaceFilesChangedEventFlushTimeoutRef = useRef<number | null>(null);
  const lastWorkspaceFilesChangedRevisionRef = useRef<number | null>(null);
  // State-resync refs are kept at the hook body so the
  // transport useEffect can reset them on Strict Mode remount
  // without losing the per-mount cleanup identity.
  const stateResyncInFlightRef = useRef(false);
  const stateResyncPendingRef = useRef(false);
  const stateResyncAllowAuthoritativeRollbackRef = useRef(false);
  const stateResyncPreserveReconnectFallbackRef = useRef(false);
  const stateResyncPreserveWatchdogCooldownRef = useRef(false);
  const stateResyncRearmOnSuccessRef = useRef(false);
  const stateResyncRearmOnFailureRef = useRef(false);
  const stateResyncOpenSessionIdRef = useRef<string | undefined>(undefined);
  const stateResyncPaneIdRef = useRef<string | null | undefined>(undefined);
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
  }

  function flushPendingSessionStoreSync(sessionSnapshot = sessionsRef.current) {
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
    pendingSessionIds.clear();
    if (changedSessions.length === 0) {
      return;
    }

    syncComposerSessionsStoreIncremental({
      changedSessions,
      draftsBySessionId: draftsBySessionIdRef.current,
      draftAttachmentsBySessionId: draftAttachmentsBySessionIdRef.current,
    });
  }

  function cancelPendingSessionRender() {
    if (pendingSessionRenderFrameRef.current !== null) {
      window.cancelAnimationFrame(pendingSessionRenderFrameRef.current);
      pendingSessionRenderFrameRef.current = null;
    }
    hasPendingSessionRenderRef.current = false;
    pendingSessionStoreSyncIdsRef.current.clear();
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

    pendingCodexStateRenderFrameRef.current =
      window.requestAnimationFrame(flushPendingCodexStateRender);
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

    pendingSessionRenderFrameRef.current =
      window.requestAnimationFrame(flushPendingSessionRender);
  }

  useEffect(() => {
    return () => {
      cancelPendingSessionRender();
      cancelPendingCodexStateRender();
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
    }
    if (hasRemovedSessions || unhydratedSessionIds.size > 0) {
      hydratedSessionIdsRef.current = new Set(
        [...hydratedSessionIdsRef.current].filter((sessionId) =>
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
      requestActionRecoveryResyncRef.current();
      return "recovering";
    }

    // Route the session write through the same revision-gate that
    // governs `adoptState`, BUT pass the response's `serverInstanceId`
    // so a server-restart-driven revision rewind is accepted. This is
    // the unified fix for:
    //   - "Prompt sent during SSE reconnect window is invisible until
    //     safety-net poll" — after a restart, the POST response's
    //     instance id differs from `lastSeenServerInstanceIdRef` and
    //     the gate accepts the lower revision, so the user's prompt
    //     shows up immediately.
    //   - "adoptCreatedSessionResponse session write is not
    //     revision-gated" — a stale POST response (race between SSE
    //     delta and POST resolution on the same server instance) is
    //     now rejected instead of unconditionally overwriting
    //     `sessionsRef`.
    if (
      !shouldAdoptSnapshotRevision(
        latestStateRevisionRef.current,
        created.revision,
        {
          lastSeenServerInstanceId: lastSeenServerInstanceIdRef.current,
          nextServerInstanceId: created.serverInstanceId,
        },
      )
    ) {
      return "stale";
    }

    const previousSessions = sessionsRef.current;
    const existingIndex = previousSessions.findIndex(
      (session) => session.id === created.sessionId,
    );
    const nextSessions =
      existingIndex === -1
        ? [...previousSessions, created.session]
        : previousSessions.map((session, index) =>
            index === existingIndex ? created.session : session,
          );
    latestStateRevisionRef.current = created.revision;
    if (created.serverInstanceId) {
      lastSeenServerInstanceIdRef.current = created.serverInstanceId;
    }
    sessionsRef.current = nextSessions;
    upsertSessionSlice(created.session);
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

  // Returns `boolean` rather than the sibling
  // `AdoptCreatedSessionOutcome` discriminated union because the
  // wire-contract-mismatch branch for this path is hoisted to
  // the call site in the hydration effect below (see
  // `fetchSession` catch branch: it checks
  // `response.session.id !== sessionId` and triggers recovery
  // BEFORE reaching this function). The two `false` outcomes
  // here — unknown session id (deleted during fetch) and
  // revision gate rejection — both collapse to the same caller
  // behaviour: don't mark as hydrated, let the next SSE delta
  // or action-recovery resync reconcile. A three-way
  // discrimination would be useful only if those two cases
  // ever needed divergent callback treatment.
  function adoptFetchedSession(
    session: Session,
    revision: number,
    serverInstanceId: string,
  ) {
    const previousRevision = latestStateRevisionRef.current;
    const previousSessions = sessionsRef.current;
    const existingIndex = previousSessions.findIndex(
      (entry) => entry.id === session.id,
    );
    if (existingIndex === -1) {
      return false;
    }

    const currentSession = previousSessions[existingIndex];
    // Routing through `shouldAdoptSnapshotRevision` gives us the
    // `serverInstanceId` restart-detection branch while preserving the
    // pre-existing nuance: on a genuine first hydration (no local
    // messages yet) we accept even a lower revision, but once the
    // session has hydrated messages we refuse to clobber them with an
    // older snapshot. `force + allowRevisionDowngrade: <messages empty>`
    // encodes exactly that — force=true enters the downgrade branch,
    // and `allowRevisionDowngrade` decides whether same-instance
    // downgrades are permitted. Instance mismatch wins over both, so
    // a restart mid-hydration is always adopted regardless of revision.
    if (
      !shouldAdoptSnapshotRevision(previousRevision, revision, {
        lastSeenServerInstanceId: lastSeenServerInstanceIdRef.current,
        nextServerInstanceId: serverInstanceId,
        force: true,
        allowRevisionDowngrade: currentSession.messagesLoaded !== true,
      })
    ) {
      return false;
    }

    const hydratedSession = { ...session, messagesLoaded: true };
    const nextSessions = previousSessions.map((entry, index) =>
      index === existingIndex ? hydratedSession : entry,
    );
    if (previousRevision === null || revision > previousRevision) {
      latestStateRevisionRef.current = revision;
    }
    if (serverInstanceId) {
      lastSeenServerInstanceIdRef.current = serverInstanceId;
    }
    sessionsRef.current = nextSessions;
    upsertSessionSlice(hydratedSession);
    flushAndCancelPendingSessionRender(nextSessions);
    setSessions(nextSessions);
    return true;
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
      if (hydratingSessionIdsRef.current.has(sessionId)) {
        continue;
      }

      hydratingSessionIdsRef.current.add(sessionId);
      void (async () => {
        try {
          const response = await fetchSession(sessionId);
          if (!isMountedRef.current) {
            return;
          }
          if (response.session.id !== sessionId) {
            requestActionRecoveryResyncRef.current();
            return;
          }
          if (
            adoptFetchedSession(
              response.session,
              response.revision,
              response.serverInstanceId,
            )
          ) {
            hydratedSessionIdsRef.current.add(sessionId);
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
        } finally {
          hydratingSessionIdsRef.current.delete(sessionId);
        }
      })();
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

    if (
      !shouldAdoptSnapshotRevision(
        latestStateRevisionRef.current,
        nextState.revision,
        {
          ...options,
          lastSeenServerInstanceId: lastSeenServerInstanceIdRef.current,
          nextServerInstanceId: nextState.serverInstanceId,
        },
      )
    ) {
      return false;
    }

    const serverInstanceChanged =
      !!nextState.serverInstanceId &&
      nextState.serverInstanceId !== lastSeenServerInstanceIdRef.current;
    latestStateRevisionRef.current = nextState.revision;
    if (nextState.serverInstanceId) {
      lastSeenServerInstanceIdRef.current = nextState.serverInstanceId;
    }
    if (serverInstanceChanged) {
      hydratingSessionIdsRef.current.clear();
      hydratedSessionIdsRef.current.clear();
    }
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
    if (adoptedStateSlices.workspaces !== currentWorkspaceSummaries) {
      workspaceSummariesRef.current = adoptedStateSlices.workspaces;
      setWorkspaceSummaries(adoptedStateSlices.workspaces);
    }
    const requestedOpenSessionId =
      options?.openSessionId ?? pendingRecoveryOpenSessionIdRef.current;
    adoptSessions(nextState.sessions, {
      ...options,
      disableMutationStampFastPath: serverInstanceChanged,
    });
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
    workspaceFilesChangedEventFlushTimeoutRef.current = null;
    const bufferedEvent = workspaceFilesChangedEventBufferRef.current;
    workspaceFilesChangedEventBufferRef.current = null;
    if (!bufferedEvent || !isMountedRef.current) {
      return;
    }

    startTransition(() => {
      setWorkspaceFilesChangedEvent(bufferedEvent);
    });
  }

  function resetWorkspaceFilesChangedEventGate() {
    lastWorkspaceFilesChangedRevisionRef.current = null;
    workspaceFilesChangedEventBufferRef.current = null;
    if (workspaceFilesChangedEventFlushTimeoutRef.current !== null) {
      window.clearTimeout(workspaceFilesChangedEventFlushTimeoutRef.current);
      workspaceFilesChangedEventFlushTimeoutRef.current = null;
    }
  }

  function enqueueWorkspaceFilesChangedEvent(
    filesChanged: WorkspaceFilesChangedEvent,
  ) {
    const lastRevision = lastWorkspaceFilesChangedRevisionRef.current;
    if (lastRevision !== null && filesChanged.revision < lastRevision) {
      return;
    }

    lastWorkspaceFilesChangedRevisionRef.current = filesChanged.revision;
    workspaceFilesChangedEventBufferRef.current =
      mergeWorkspaceFilesChangedEvents(
        workspaceFilesChangedEventBufferRef.current,
        filesChanged,
      );

    if (workspaceFilesChangedEventFlushTimeoutRef.current !== null) {
      return;
    }

    workspaceFilesChangedEventFlushTimeoutRef.current = window.setTimeout(
      flushWorkspaceFilesChangedEventBuffer,
      0,
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
    let nextReconnectStateResyncDelayMs = RECONNECT_STATE_RESYNC_DELAY_MS;
    let liveSessionResumeWatchdogIntervalId: ReturnType<
      typeof window.setInterval
    > | null = null;
    let shouldResyncOnResume = false;
    // These refs are component-scoped, so Strict Mode effect remounts must reset
    // any stale in-flight resync bookkeeping from the previous mount.
    stateResyncInFlightRef.current = false;
    stateResyncPendingRef.current = false;
    stateResyncAllowAuthoritativeRollbackRef.current = false;
    stateResyncPreserveReconnectFallbackRef.current = false;
    stateResyncPreserveWatchdogCooldownRef.current = false;
    stateResyncRearmOnSuccessRef.current = false;
    stateResyncRearmOnFailureRef.current = false;
    stateResyncOpenSessionIdRef.current = undefined;
    stateResyncPaneIdRef.current = undefined;
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

    function clearReconnectStateResyncTimeoutAfterConfirmedReopen() {
      if (!sawReconnectOpenSinceLastError) {
        return;
      }

      clearReconnectStateResyncTimeout();
      resetReconnectStateResyncBackoff();
    }

    function confirmReconnectRecoveryFromLiveEvent() {
      if (!sawReconnectOpenSinceLastError) {
        return;
      }

      clearReconnectStateResyncTimeoutAfterConfirmedReopen();
      setBackendConnectionState("connected");
    }

    function scheduleReconnectStateResync() {
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
          rearmOnFailure: true,
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
      for (const sessionId of sessionIds) {
        lastLiveTransportActivityAtBySessionId.set(sessionId, now);
      }
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
      // Snapshot adoption seeds the baseline for every listed session immediately.
      // Idle entries are harmless because stale-transport checks still gate on
      // session.status === "active".
      markLiveTransportActivity(
        sessions.map((session) => session.id),
        now,
        { clearWatchdogCooldown },
      );
    }

    function markLiveSessionResumeWatchdogBaseline(
      sessionIds: Iterable<string>,
      now = Date.now(),
    ) {
      for (const sessionId of sessionIds) {
        lastLiveSessionResumeWatchdogTickAtBySessionId.set(sessionId, now);
      }
    }

    function pruneLiveSessionResumeWatchdogBaselineSessions(
      sessions: Session[],
    ) {
      const liveSessionIds = new Set(sessions.map((session) => session.id));
      for (const sessionId of lastLiveSessionResumeWatchdogTickAtBySessionId.keys()) {
        if (!liveSessionIds.has(sessionId)) {
          lastLiveSessionResumeWatchdogTickAtBySessionId.delete(sessionId);
        }
      }
    }

    function syncLiveSessionResumeWatchdogBaselines(
      sessions: Session[],
      now = Date.now(),
    ) {
      // Advance every currently known session so idle-to-active transitions do not
      // inherit a false wake gap from time spent without live streaming.
      markLiveSessionResumeWatchdogBaseline(
        sessions.map((session) => session.id),
        now,
      );
      pruneLiveSessionResumeWatchdogBaselineSessions(sessions);
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
            const allowAuthoritativeRollback =
              stateResyncAllowAuthoritativeRollbackRef.current;
            stateResyncAllowAuthoritativeRollbackRef.current = false;
            const preserveReconnectFallback =
              stateResyncPreserveReconnectFallbackRef.current;
            stateResyncPreserveReconnectFallbackRef.current = false;
            const preserveWatchdogCooldown =
              stateResyncPreserveWatchdogCooldownRef.current;
            stateResyncPreserveWatchdogCooldownRef.current = false;
            const rearmOnSuccess = stateResyncRearmOnSuccessRef.current;
            stateResyncRearmOnSuccessRef.current = false;
            const rearmOnFailure = stateResyncRearmOnFailureRef.current;
            stateResyncRearmOnFailureRef.current = false;
            const openSessionId = stateResyncOpenSessionIdRef.current;
            stateResyncOpenSessionIdRef.current = undefined;
            const paneId = stateResyncPaneIdRef.current;
            stateResyncPaneIdRef.current = undefined;
            const requestedRevision = latestStateRevisionRef.current;

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
              const shouldForceAuthoritativeSnapshot =
                allowAuthoritativeRollback &&
                shouldPreferAuthoritativeSnapshot &&
                requestedRevision !== null &&
                latestStateRevisionRef.current === requestedRevision &&
                state.revision <= requestedRevision;
              const shouldForceRollback =
                shouldForceAuthoritativeSnapshot &&
                !!state.serverInstanceId &&
                state.serverInstanceId !== lastSeenServerInstanceIdRef.current;

              const adopted = adoptState(state, {
                // A reconnect fallback snapshot is authoritative if no newer SSE state landed
                // while it was in flight, even when a crashed backend restarted below the last
                // streamed client revision. Keep this broader than restart-only rollback:
                // reconnect/watchdog/manual-retry probes are explicitly asking `/api/state`
                // to replace any buffered or non-session SSE view once the fetch resolves.
                force: shouldForceAuthoritativeSnapshot,
                allowRevisionDowngrade: shouldForceAuthoritativeSnapshot,
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
              if (adopted) {
                clearInitialStateResyncRetryTimeout();
                if (
                  reconnectStateResyncTimeoutId !== null &&
                  sawReconnectOpenSinceLastError
                ) {
                  // Once the stream itself has reopened, an adopted snapshot can
                  // disarm any leftover reconnect fallback timer and reset the
                  // next reconnect cycle back to the fast initial delay.
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
              } else if (shouldRetryStaleSameInstanceSnapshot) {
                // A same-instance snapshot that still lags behind the client's
                // locally adopted revision can arrive during create/fork wake-gap
                // recovery. Do not roll back to it, but keep probing until the
                // authoritative snapshot catches up.
                scheduleReconnectStateResync();
              }
              setBackendConnectionIssueDetail(null);
              clearRecoveredBackendRequestError();
              if (
                rearmOnSuccess &&
                requestedRevision !== null &&
                latestStateRevisionRef.current === requestedRevision &&
                !sawReconnectOpenSinceLastError &&
                reconnectStateResyncTimeoutId === null
              ) {
                // Reconnect fallback probes must keep polling authoritative
                // state until the live SSE transport proves it reopened.
                // Generic watchdog or wake-gap resyncs stay one-shot.
                scheduleReconnectStateResync();
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
                  scheduleReconnectStateResync();
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
        sawReconnectOpenSinceLastError &&
        !options?.preserveReconnectFallback
      ) {
        clearReconnectStateResyncTimeout();
      }
      // Otherwise preserve the armed reconnect fallback; startStateResyncLoop()
      // will only disarm it once a /api/state snapshot is actually adopted.
      // Coalesced resync requests retain the strongest semantics until the next
      // loop iteration consumes them.
      if (options?.allowAuthoritativeRollback) {
        stateResyncAllowAuthoritativeRollbackRef.current = true;
      }
      if (options?.preserveReconnectFallback) {
        stateResyncPreserveReconnectFallbackRef.current = true;
      }
      if (options?.preserveWatchdogCooldown) {
        stateResyncPreserveWatchdogCooldownRef.current = true;
      }
      if (options?.rearmOnSuccess) {
        stateResyncRearmOnSuccessRef.current = true;
      }
      if (options?.rearmOnFailure) {
        stateResyncRearmOnFailureRef.current = true;
      }
      if (options?.openSessionId !== undefined) {
        stateResyncOpenSessionIdRef.current = options.openSessionId;
        stateResyncPaneIdRef.current = options.paneId ?? null;
      }
      stateResyncPendingRef.current = true;
      startStateResyncLoop();
    }
    requestBackendReconnectRef.current = () => {
      if (cancelled || !readNavigatorOnline()) {
        return;
      }

      sawReconnectOpenSinceLastError = false;
      clearInitialStateResyncRetryTimeout();
      clearReconnectStateResyncTimeout();
      resetReconnectStateResyncBackoff();
      // Manual retry is intentionally an immediate `/api/state` probe. It does
      // not preserve any currently armed reconnect timer, but success or
      // failure both hand control back to the normal reconnect cycle.
      requestStateResync({
        allowAuthoritativeRollback: latestStateRevisionRef.current !== null,
        rearmOnSuccess: true,
        rearmOnFailure: true,
      });
    };
    requestActionRecoveryResyncRef.current = (options) => {
      if (cancelled || !readNavigatorOnline()) {
        return;
      }

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
        openSessionId: options?.openSessionId,
        paneId: options?.paneId,
      });
    };

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
      requestStateResync({ allowAuthoritativeRollback: true });
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
        preserveWatchdogCooldown: true,
      });
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
        profiler?.mark("peek");
        if (
          rawRevision !== null &&
          !rawIsFallback &&
          !shouldAdoptSnapshotRevision(
            latestStateRevisionRef.current,
            rawRevision,
            {
              force: forceAdoptNextStateEventRef.current,
              allowRevisionDowngrade: forceAdoptNextStateEventRef.current,
              lastSeenServerInstanceId: lastSeenServerInstanceIdRef.current,
              nextServerInstanceId: rawServerInstanceId,
            },
          )
        ) {
          profiledRevision = rawRevision;
          profiledAdopted = false;
          forceAdoptNextStateEventRef.current = false;
          confirmReconnectRecoveryFromLiveEvent();
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
          forceAdoptNextStateEventRef.current = false;
          profiler?.mark("fallback");
          requestStateResync({
            allowAuthoritativeRollback: true,
            preserveReconnectFallback: true,
          });
          return;
        }

        const force = forceAdoptNextStateEventRef.current;
        // SSE state events are always the first event on a new connection
        // (before any deltas), so there is no risk of a delta racing ahead
        // and being overwritten. Allow revision downgrade so a restarted
        // server (whose persisted revision may be lower) is adopted.
        const adopted = adoptState(state, {
          force,
          allowRevisionDowngrade: force,
        });
        profiler?.mark("adoptState");
        profiledAdopted = adopted;
        forceAdoptNextStateEventRef.current = false;
        // Confirm recovery only after adoption succeeds. If adoptState throws
        // (bad payload, reducer error), the catch block must keep the client in
        // the reconnecting state with fallback polling armed rather than
        // prematurely marking the connection as healthy.
        confirmReconnectRecoveryFromLiveEvent();
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
        if (!cancelled) {
          setBackendConnectionIssueDetail(
            describeBackendConnectionIssueDetail(error),
          );
          // A bad reconnect state payload must not leave the client marked as
          // connected without a usable snapshot. Restore "reconnecting" so the
          // retry affordance stays available (onopen already set "connected"),
          // and re-arm fallback polling so recovery continues via /api/state.
          if (sawReconnectOpenSinceLastError) {
            setBackendConnectionState("reconnecting");
            if (reconnectStateResyncTimeoutId === null) {
              scheduleReconnectStateResync();
            }
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
        const revisionAction = decideDeltaRevisionAction(
          latestStateRevisionRef.current,
          delta.revision,
        );
        if (revisionAction === "ignore") {
          if ("sessionId" in delta && typeof delta.sessionId === "string") {
            cancelStaleSendResponseRecoveryPollForSessions([delta.sessionId]);
          } else if (
            delta.type === "orchestratorsUpdated" &&
            delta.sessions?.length
          ) {
            cancelStaleSendResponseRecoveryPollForSessions(
              delta.sessions.map((session) => session.id),
            );
          }
          // An ignored delta proves the client already has data at this
          // revision or newer — the snapshot that advanced the revision was
          // authoritative. Transport is healthy and the client is caught up.
          confirmReconnectRecoveryFromLiveEvent();
          clearReconnectStateResyncTimeoutAfterConfirmedReopen();
          setBackendConnectionIssueDetail(null);
          clearRecoveredBackendRequestError();
          return;
        }
        if (revisionAction === "resync") {
          if ("sessionId" in delta && typeof delta.sessionId === "string") {
            cancelStaleSendResponseRecoveryPollForSessions([delta.sessionId]);
          } else if (
            delta.type === "orchestratorsUpdated" &&
            delta.sessions?.length
          ) {
            cancelStaleSendResponseRecoveryPollForSessions(
              delta.sessions.map((session) => session.id),
            );
          }
          // A revision gap means we missed events but the stream IS working.
          // Do NOT confirm recovery yet — if the follow-up /api/state fetch
          // fails, the client must stay in the reconnecting state. Use
          // rearmOnFailure so a failed resync re-arms polling instead of
          // stalling recovery.
          requestStateResync({ rearmOnFailure: true });
          return;
        }

        if (delta.type === "codexUpdated") {
          confirmReconnectRecoveryFromLiveEvent();
          clearReconnectStateResyncTimeoutAfterConfirmedReopen();
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
          confirmReconnectRecoveryFromLiveEvent();
          clearReconnectStateResyncTimeoutAfterConfirmedReopen();
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
        if (result.kind === "applied") {
          confirmReconnectRecoveryFromLiveEvent();
          const appliedAt = Date.now();
          clearReconnectStateResyncTimeoutAfterConfirmedReopen();
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
          }
          scheduleSessionRender();
          setBackendConnectionIssueDetail(null);
          clearRecoveredBackendRequestError();
          return;
        }

        // Unrecognized delta type — resync to get an authoritative snapshot.
        requestStateResync({ rearmOnFailure: true });
      } catch {
        // Parse or reducer failure — restore reconnecting state so the retry
        // affordance stays available, and re-arm polling.
        if (sawReconnectOpenSinceLastError) {
          setBackendConnectionState("reconnecting");
          if (reconnectStateResyncTimeoutId === null) {
            scheduleReconnectStateResync();
          }
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
        confirmReconnectRecoveryFromLiveEvent();
        clearReconnectStateResyncTimeoutAfterConfirmedReopen();
        setBackendConnectionIssueDetail(null);
        clearRecoveredBackendRequestError();
        enqueueWorkspaceFilesChangedEvent(filesChanged);
      } catch {
        // File-change events are non-authoritative hints. If one is malformed,
        // keep the main state stream alive and wait for the next event/snapshot.
      }
    }

    eventSource.addEventListener("state", handleStateEvent as EventListener);
    eventSource.addEventListener("delta", handleDeltaEvent as EventListener);
    eventSource.addEventListener(
      "workspaceFilesChanged",
      handleWorkspaceFilesChangedEvent as EventListener,
    );
    eventSource.onopen = () => {
      if (!cancelled) {
        resetWorkspaceFilesChangedEventGate();
        sawReconnectOpenSinceLastError = true;
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
      const isOnline = readNavigatorOnline();
      const hasHydratedState = latestStateRevisionRef.current !== null;
      setBackendConnectionState(
        isOnline
          ? hasHydratedState
            ? "reconnecting"
            : "connecting"
          : "offline",
      );
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
      if (workspaceFilesChangedEventFlushTimeoutRef.current !== null) {
        window.clearTimeout(workspaceFilesChangedEventFlushTimeoutRef.current);
        workspaceFilesChangedEventFlushTimeoutRef.current = null;
      }
      workspaceFilesChangedEventBufferRef.current = null;
      if (liveSessionResumeWatchdogIntervalId !== null) {
        window.clearInterval(liveSessionResumeWatchdogIntervalId);
      }
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
        "workspaceFilesChanged",
        handleWorkspaceFilesChangedEvent as EventListener,
      );
      eventSource.close();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return {
    adoptState,
    adoptCreatedSessionResponse,
    syncPreferencesFromState,
    hydratedSessionIdsRef,
    hydratingSessionIdsRef,
    forceAdoptNextStateEventRef,
    workspaceFilesChangedEvent,
    workspaceFilesChangedEventBufferRef,
    workspaceFilesChangedEventFlushTimeoutRef,
    resetWorkspaceFilesChangedEventGate,
  };
}
