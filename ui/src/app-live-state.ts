// app-live-state.ts
//
// Owns: the delta-adoption + hydration half of the live-state
// plumbing that used to live inline in App.tsx. That includes the
// SSE `state` / `delta` / `workspaceFilesChanged` handler bodies,
// the adoption helpers (`adoptState`, `adoptSessions`,
// `adoptCreatedSessionResponse`, `adoptFetchedSession`,
// `syncPreferencesFromState`), the workspace-files-changed
// buffering gate (`flushWorkspaceFilesChangedEventBuffer`,
// `resetWorkspaceFilesChangedEventGate`,
// `enqueueWorkspaceFilesChangedEvent` plus the buffer / flush
// timeout / revision-gate refs), the `forceAdoptNextStateEventRef`
// refresh flag, the session hydration fetch effect, and the
// `hydratedSessionIdsRef` / `hydratingSessionIdsRef` tracking
// refs. The `workspaceFilesChangedEvent` React state + setter
// also live here — consumers in App.tsx read them via the hook
// return value.
//
// Does not own: bootstrap fetch sequencing, reconnect timer
// orchestration, watchdog timer orchestration, visibility /
// focus / pagehide / pageshow handlers, the EventSource
// open/close lifecycle, or `requestActionRecoveryResyncRef`
// (kept in App.tsx so reconnect/bootstrap paths keep
// registering it). The reconnect/watchdog coordination helpers
// that delta/state adoption still invokes (e.g.
// `confirmReconnectRecoveryFromLiveEvent`, `requestStateResync`,
// `markLiveTransportActivity`) are reached through a
// `transportCoordinationRef` that App.tsx populates inside its
// EventSource effect; those helpers will move in Slice 13B.
//
// Split out of: ui/src/App.tsx (Slice 13A of the App-split plan,
// see docs/app-split-plan.md).

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
  ApiRequestError,
  fetchSession,
  type CreateSessionResponse,
  type StateResponse,
} from "./api";
import {
  applyDeltaToSessions,
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
import type {
  PendingSessionRename,
  SessionErrorMap,
  SessionNoticeMap,
  StateEventPayload,
} from "./app-shell-internals";

// Outcome of `adoptCreatedSessionResponse`:
//   - "adopted":    session inserted into `sessionsRef` and the
//                   workspace pane opened. Nothing for the caller
//                   to do.
//   - "stale":      revision gate rejected the write because a
//                   newer snapshot already landed. The session
//                   was NOT inserted here but an earlier delta
//                   already saw it (that's how the revision got
//                   ahead), so `sessionsRef` does contain
//                   `created.sessionId`. The caller may still
//                   open the workspace pane as a safe fallback.
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
};

export type RequestStateResyncOptions = {
  allowAuthoritativeRollback?: boolean;
  preserveReconnectFallback?: boolean;
  preserveWatchdogCooldown?: boolean;
  rearmOnSuccess?: boolean;
  rearmOnFailure?: boolean;
};

// Bag of reconnect/watchdog helpers that still live inside
// App.tsx's EventSource effect during Slice 13A. The hook's
// delta/state/workspace-files handlers reach these through a
// ref so App.tsx can swap them when the transport effect
// remounts. All of these move out in Slice 13B.
export type LiveTransportCoordination = {
  isCancelled(): boolean;
  sawReconnectOpenSinceLastError(): boolean;
  hasReconnectStateResyncTimeout(): boolean;
  confirmReconnectRecoveryFromLiveEvent(): void;
  clearInitialStateResyncRetryTimeout(): void;
  clearReconnectStateResyncTimeoutAfterConfirmedReopen(): void;
  scheduleReconnectStateResync(): void;
  requestStateResync(options?: RequestStateResyncOptions): void;
  markLiveTransportActivity(
    sessionIds: Iterable<string>,
    now?: number,
    options?: { clearWatchdogCooldown?: boolean },
  ): void;
  markLiveSessionResumeWatchdogBaseline(
    sessionIds: Iterable<string>,
    now?: number,
  ): void;
  syncLiveTransportActivityFromState(
    sessions: Session[],
    now?: number,
    options?: { clearWatchdogCooldown?: boolean },
  ): void;
  pruneLiveTransportActivitySessions(sessions: Session[]): void;
  syncLiveSessionResumeWatchdogBaselines(
    sessions: Session[],
    now?: number,
  ): void;
};

export type UseAppLiveStateAdoptionRefs = {
  isMountedRef: MutableRefObject<boolean>;
  latestStateRevisionRef: MutableRefObject<number | null>;
  lastSeenServerInstanceIdRef: MutableRefObject<string | null>;
  sessionsRef: MutableRefObject<Session[]>;
  codexStateRef: MutableRefObject<CodexState>;
  agentReadinessRef: MutableRefObject<AgentReadiness[]>;
  projectsRef: MutableRefObject<Project[]>;
  orchestratorsRef: MutableRefObject<OrchestratorInstance[]>;
  workspaceSummariesRef: MutableRefObject<WorkspaceLayoutSummary[]>;
  refreshingAgentCommandSessionIdsRef: MutableRefObject<SessionFlagMap>;
  confirmedUnknownModelSendsRef: MutableRefObject<Set<string>>;
};

export type UseAppLiveStateStateSetters = {
  setSessions: Dispatch<SetStateAction<Session[]>>;
  setWorkspace: Dispatch<SetStateAction<WorkspaceState>>;
  setCodexState: Dispatch<SetStateAction<CodexState>>;
  setAgentReadiness: Dispatch<SetStateAction<AgentReadiness[]>>;
  setProjects: Dispatch<SetStateAction<Project[]>>;
  setOrchestrators: Dispatch<SetStateAction<OrchestratorInstance[]>>;
  setWorkspaceSummaries: Dispatch<SetStateAction<WorkspaceLayoutSummary[]>>;
  setDraftsBySessionId: Dispatch<
    SetStateAction<Record<string, string>>
  >;
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
  setAgentCommandsBySessionId: Dispatch<
    SetStateAction<SessionAgentCommandMap>
  >;
  setRefreshingAgentCommandSessionIds: Dispatch<
    SetStateAction<SessionFlagMap>
  >;
  setAgentCommandErrors: Dispatch<SetStateAction<SessionErrorMap>>;
  setSessionSettingNotices: Dispatch<SetStateAction<SessionNoticeMap>>;
  setSelectedProjectId: Dispatch<SetStateAction<string>>;
  setIsLoading: Dispatch<SetStateAction<boolean>>;
  setBackendConnectionIssueDetail: Dispatch<SetStateAction<string | null>>;
  setBackendConnectionState: (next: BackendConnectionState) => void;
};

export type UseAppLiveStatePreferenceSetters = {
  setDefaultCodexReasoningEffort: Dispatch<SetStateAction<CodexReasoningEffort>>;
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
  requestActionRecoveryResyncRef: MutableRefObject<() => void>;
  syncAdoptedLiveSessionResumeWatchdogBaselinesRef: MutableRefObject<
    (sessions: Session[], now?: number) => void
  >;
  transportCoordinationRef: MutableRefObject<LiveTransportCoordination>;
  clearRecoveredBackendRequestError: () => void;
  reportRequestError: (
    error: unknown,
    options?: { message?: string },
  ) => void;
  activeSession: Session | null;
};

export type UseAppLiveStateReturn = {
  handleDeltaEvent: (event: MessageEvent<string>) => void;
  handleStateEvent: (event: MessageEvent<string>) => void;
  handleWorkspaceFilesChangedEvent: (event: MessageEvent<string>) => void;
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
  workspaceFilesChangedEventBufferRef: MutableRefObject<
    WorkspaceFilesChangedEvent | null
  >;
  workspaceFilesChangedEventFlushTimeoutRef: MutableRefObject<number | null>;
  resetWorkspaceFilesChangedEventGate: () => void;
};

export function useAppLiveState(
  params: UseAppLiveStateParams,
): UseAppLiveStateReturn {
  const {
    adoptionRefs,
    stateSetters,
    preferenceSetters,
    applyControlPanelLayout,
    requestActionRecoveryResyncRef,
    syncAdoptedLiveSessionResumeWatchdogBaselinesRef,
    transportCoordinationRef,
    clearRecoveredBackendRequestError,
    reportRequestError,
    activeSession,
  } = params;
  const {
    isMountedRef,
    latestStateRevisionRef,
    lastSeenServerInstanceIdRef,
    sessionsRef,
    codexStateRef,
    agentReadinessRef,
    projectsRef,
    orchestratorsRef,
    workspaceSummariesRef,
    refreshingAgentCommandSessionIdsRef,
    confirmedUnknownModelSendsRef,
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

  function adoptSessions(
    nextSessions: Session[],
    options?: AdoptSessionsOptions,
  ) {
    const previousSessions = sessionsRef.current;
    const previousSessionsById = new Map(
      previousSessions.map((session) => [session.id, session]),
    );
    const mergedSessions = reconcileSessions(previousSessions, nextSessions);
    const availableSessionIds = new Set(
      mergedSessions.map((session) => session.id),
    );
    const sessionsWithChangedWorkdir = new Set(
      mergedSessions.flatMap((session) => {
        const previousSession = previousSessionsById.get(session.id);
        return previousSession && previousSession.workdir !== session.workdir
          ? [session.id]
          : [];
      }),
    );
    // Avoid rewriting workspace state when an adopted snapshot preserves the
    // same reconciled sessions. Workspace autosave is keyed off `workspace`
    // identity, so an identity-only rewrite here can create a loop:
    // workspace PUT -> SSE state snapshot -> adoptSessions -> workspace save.
    const shouldReconcileWorkspace =
      mergedSessions !== previousSessions || Boolean(options?.openSessionId);

    sessionsRef.current = mergedSessions;
    setSessions(mergedSessions);
    if (shouldReconcileWorkspace) {
      setWorkspace((current) => {
        const reconciled =
          mergedSessions !== previousSessions
            ? applyControlPanelLayout(
                reconcileWorkspaceState(current, mergedSessions),
              )
            : current;
        if (!options?.openSessionId) {
          return reconciled;
        }

        return applyControlPanelLayout(
          openSessionInWorkspaceState(
            reconciled,
            options.openSessionId,
            options.paneId ?? null,
          ),
        );
      });
    }
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
      current && availableSessionIds.has(current.sessionId) ? current : null,
    );
    setUpdatingSessionIds((current) =>
      pruneSessionFlags(current, availableSessionIds),
    );
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
    refreshingAgentCommandSessionIdsRef.current =
      pruneSessionFlagsWithInvalidation(
        refreshingAgentCommandSessionIdsRef.current,
        availableSessionIds,
        sessionsWithChangedWorkdir,
      );
    setAgentCommandErrors((current) =>
      pruneSessionValues(
        current,
        availableSessionIds,
        sessionsWithChangedWorkdir,
      ),
    );
    setSessionSettingNotices((current) =>
      pruneSessionValues(current, availableSessionIds),
    );
    const availableUnknownModelKeys = new Set(
      mergedSessions
        .filter((session) => describeUnknownSessionModelWarning(session))
        .map((session) =>
          unknownSessionModelConfirmationKey(session.id, session.model),
        ),
    );
    confirmedUnknownModelSendsRef.current = new Set(
      [...confirmedUnknownModelSendsRef.current].filter((key) =>
        availableUnknownModelKeys.has(key),
      ),
    );
    hydratingSessionIdsRef.current = new Set(
      [...hydratingSessionIdsRef.current].filter((sessionId) =>
        availableSessionIds.has(sessionId),
      ),
    );
    hydratedSessionIdsRef.current = new Set(
      [...hydratedSessionIdsRef.current].filter((sessionId) =>
        availableSessionIds.has(sessionId),
      ),
    );
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
        allowRevisionDowngrade: currentSession.messages.length === 0,
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
    setSessions(nextSessions);
    return true;
  }

  useEffect(() => {
    if (
      !activeSession ||
      activeSession.messagesLoaded !== false
    ) {
      return;
    }

    const sessionId = activeSession.id;
    if (
      hydratedSessionIdsRef.current.has(sessionId) ||
      hydratingSessionIdsRef.current.has(sessionId)
    ) {
      return;
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
        // `fetchWorkspaceLayout`'s "404 → silent recovery" UX
        // posture; the transport shape differs (that one returns
        // `null` at the API boundary so callers treat it as "no
        // layout yet"; here `fetchSession` throws
        // `ApiRequestError` and we branch on `instanceof` + status
        // at the call site).
        if (
          error instanceof ApiRequestError &&
          error.status === 404
        ) {
          requestActionRecoveryResyncRef.current();
          return;
        }
        reportRequestError(error);
      } finally {
        hydratingSessionIdsRef.current.delete(sessionId);
      }
    })();
    // Deps intentionally do NOT include `activeSession?.messages.length`:
    // the body only reads `activeSession?.id` and
    // `activeSession?.messagesLoaded`, so re-triggering on every
    // streamed token (which bumps `messages.length`) just to hit
    // the "already hydrated / already hydrating" early-returns is
    // wasted work. Session swap (id change) and the one-shot
    // `messagesLoaded: false → true` transition are the only
    // signals this effect cares about.
  }, [activeSession?.id, activeSession?.messagesLoaded]);

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

  function adoptState(
    nextState: StateResponse,
    options?: AdoptStateOptions,
  ) {
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

    latestStateRevisionRef.current = nextState.revision;
    if (nextState.serverInstanceId) {
      lastSeenServerInstanceIdRef.current = nextState.serverInstanceId;
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
    adoptSessions(nextState.sessions, options);
    // Local state adoptions can resume or create active sessions before any SSE arrives.
    syncAdoptedLiveSessionResumeWatchdogBaselinesRef.current(
      nextState.sessions,
    );
    if (options?.openSessionId) {
      const openedSession = nextState.sessions.find(
        (session) => session.id === options.openSessionId,
      );
      setSelectedProjectId(openedSession?.projectId ?? ALL_PROJECTS_FILTER_ID);
    }
    return true;
  }

  function flushWorkspaceFilesChangedEventBuffer() {
    workspaceFilesChangedEventFlushTimeoutRef.current = null;
    const bufferedEvent = workspaceFilesChangedEventBufferRef.current;
    workspaceFilesChangedEventBufferRef.current = null;
    if (!bufferedEvent || transportCoordinationRef.current.isCancelled()) {
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

  function handleStateEvent(event: MessageEvent<string>) {
    const coordination = transportCoordinationRef.current;
    if (coordination.isCancelled()) {
      return;
    }

    try {
      const state = JSON.parse(event.data) as StateEventPayload;
      if (state._sseFallback) {
        // Marked fallback payloads only signal that the client should refetch
        // the authoritative snapshot from /api/state.
        forceAdoptNextStateEventRef.current = false;
        coordination.requestStateResync({
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
      forceAdoptNextStateEventRef.current = false;
      // Confirm recovery only after adoption succeeds. If adoptState throws
      // (bad payload, reducer error), the catch block must keep the client in
      // the reconnecting state with fallback polling armed rather than
      // prematurely marking the connection as healthy.
      coordination.confirmReconnectRecoveryFromLiveEvent();
      if (adopted) {
        coordination.clearInitialStateResyncRetryTimeout();
        const adoptedAt = Date.now();
        coordination.clearReconnectStateResyncTimeoutAfterConfirmedReopen();
        // A live SSE state payload proves the stream is healthy again, so it also
        // clears any residual watchdog retry cooldown from an earlier fallback probe.
        coordination.syncLiveTransportActivityFromState(
          state.sessions,
          adoptedAt,
        );
        coordination.pruneLiveTransportActivitySessions(state.sessions);
        coordination.syncLiveSessionResumeWatchdogBaselines(
          state.sessions,
          adoptedAt,
        );
      }
      setBackendConnectionIssueDetail(null);
      clearRecoveredBackendRequestError();
    } catch (error) {
      if (!coordination.isCancelled()) {
        setBackendConnectionIssueDetail(
          describeBackendConnectionIssueDetail(error),
        );
        // A bad reconnect state payload must not leave the client marked as
        // connected without a usable snapshot. Restore "reconnecting" so the
        // retry affordance stays available (onopen already set "connected"),
        // and re-arm fallback polling so recovery continues via /api/state.
        if (coordination.sawReconnectOpenSinceLastError()) {
          setBackendConnectionState("reconnecting");
          if (!coordination.hasReconnectStateResyncTimeout()) {
            coordination.scheduleReconnectStateResync();
          }
        }
      }
    } finally {
      if (!coordination.isCancelled()) {
        setIsLoading(false);
      }
    }
  }

  function handleDeltaEvent(event: MessageEvent<string>) {
    const coordination = transportCoordinationRef.current;
    if (coordination.isCancelled()) {
      return;
    }

    try {
      const delta = JSON.parse(event.data) as DeltaEvent;
      const revisionAction = decideDeltaRevisionAction(
        latestStateRevisionRef.current,
        delta.revision,
      );
      if (revisionAction === "ignore") {
        // An ignored delta proves the client already has data at this
        // revision or newer — the snapshot that advanced the revision was
        // authoritative. Transport is healthy and the client is caught up.
        coordination.confirmReconnectRecoveryFromLiveEvent();
        coordination.clearReconnectStateResyncTimeoutAfterConfirmedReopen();
        setBackendConnectionIssueDetail(null);
        clearRecoveredBackendRequestError();
        return;
      }
      if (revisionAction === "resync") {
        // A revision gap means we missed events but the stream IS working.
        // Do NOT confirm recovery yet — if the follow-up /api/state fetch
        // fails, the client must stay in the reconnecting state. Use
        // rearmOnFailure so a failed resync re-arms polling instead of
        // stalling recovery.
        coordination.requestStateResync({ rearmOnFailure: true });
        return;
      }

      if (delta.type === "orchestratorsUpdated") {
        // Global orchestrator updates prove the SSE stream is healthy enough to
        // clear reconnect fallback state. When the delta also carries session
        // snapshots, treat those specific ids as live data for watchdog baselines.
        coordination.confirmReconnectRecoveryFromLiveEvent();
        coordination.clearReconnectStateResyncTimeoutAfterConfirmedReopen();
        const appliedAt = Date.now();
        if (delta.sessions?.length) {
          const deltaSessionIds = delta.sessions.map((session) => session.id);
          coordination.markLiveTransportActivity(deltaSessionIds, appliedAt);
          coordination.markLiveSessionResumeWatchdogBaseline(
            deltaSessionIds,
            appliedAt,
          );
        }
        latestStateRevisionRef.current = delta.revision;
        const nextSessions = mergeOrchestratorDeltaSessions(
          sessionsRef.current,
          delta.sessions,
        );
        sessionsRef.current = nextSessions;
        orchestratorsRef.current = delta.orchestrators;
        startTransition(() => {
          setOrchestrators(delta.orchestrators);
          setSessions(nextSessions);
        });
        setBackendConnectionIssueDetail(null);
        clearRecoveredBackendRequestError();
        return;
      }

      // Non-session deltas such as orchestratorsUpdated are handled above; the
      // session reducer only accepts deltas that carry a concrete sessionId.
      const result = applyDeltaToSessions(sessionsRef.current, delta);
      if (result.kind === "applied") {
        coordination.confirmReconnectRecoveryFromLiveEvent();
        const appliedAt = Date.now();
        coordination.clearReconnectStateResyncTimeoutAfterConfirmedReopen();
        // Every session-scoped delta proves liveness for that session, including
        // any future delta shape that revives it back to "active".
        coordination.markLiveTransportActivity([delta.sessionId], appliedAt);
        coordination.markLiveSessionResumeWatchdogBaseline(
          [delta.sessionId],
          appliedAt,
        );
        latestStateRevisionRef.current = delta.revision;
        sessionsRef.current = result.sessions;
        startTransition(() => {
          setSessions(sessionsRef.current);
        });
        setBackendConnectionIssueDetail(null);
        clearRecoveredBackendRequestError();
        return;
      }

      // Unrecognized delta type — resync to get an authoritative snapshot.
      coordination.requestStateResync({ rearmOnFailure: true });
    } catch {
      // Parse or reducer failure — restore reconnecting state so the retry
      // affordance stays available, and re-arm polling.
      if (coordination.sawReconnectOpenSinceLastError()) {
        setBackendConnectionState("reconnecting");
        if (!coordination.hasReconnectStateResyncTimeout()) {
          coordination.scheduleReconnectStateResync();
        }
      } else {
        coordination.requestStateResync({ rearmOnFailure: true });
      }
    }
  }

  function handleWorkspaceFilesChangedEvent(event: MessageEvent<string>) {
    const coordination = transportCoordinationRef.current;
    if (coordination.isCancelled()) {
      return;
    }

    try {
      const filesChanged = JSON.parse(
        event.data,
      ) as WorkspaceFilesChangedEvent;
      coordination.confirmReconnectRecoveryFromLiveEvent();
      coordination.clearReconnectStateResyncTimeoutAfterConfirmedReopen();
      setBackendConnectionIssueDetail(null);
      clearRecoveredBackendRequestError();
      enqueueWorkspaceFilesChangedEvent(filesChanged);
    } catch {
      // File-change events are non-authoritative hints. If one is malformed,
      // keep the main state stream alive and wait for the next event/snapshot.
    }
  }

  return {
    handleDeltaEvent,
    handleStateEvent,
    handleWorkspaceFilesChangedEvent,
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
