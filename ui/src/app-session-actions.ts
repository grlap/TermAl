// app-session-actions.ts
//
// Owns: session and project action orchestration that used to live inline in
// App.tsx. That includes prompt send/draft-attachment lifecycle, session
// creation/cloning, project creation + folder picking, approval/user-input/MCP
// submissions, queued-prompt cancellation, stop/kill/rename, session settings
// updates, session-model refresh, Codex thread actions, and agent-command
// refresh.
//
// Does not own: dialog/popup open-close state, popover positioning/focus
// management, render-only props/JSX, or top-level request-error presentation.
//
// Split out of: ui/src/App.tsx (Slice 14 of docs/app-split-plan.md).

import type { Dispatch, MutableRefObject, SetStateAction } from "react";
import {
  archiveCodexThread,
  cancelQueuedPrompt,
  compactCodexThread,
  createConversationMarker,
  createProject,
  deleteConversationMarker,
  createSession,
  fetchAgentCommands,
  fetchState,
  forkCodexThread,
  killSession,
  pickProjectRoot,
  refreshSessionModelOptions,
  renameSession,
  rollbackCodexThread,
  sendMessage,
  stopSession,
  submitApproval,
  submitCodexAppRequest,
  submitMcpElicitation,
  submitUserInput,
  unarchiveCodexThread,
  updateConversationMarker,
  updateSessionSettings,
  type StateResponse,
  type UpdateConversationMarkerRequest,
} from "./api";
import type { AdoptStateOptions, UseAppLiveStateReturn } from "./app-live-state";
import { startActivePromptPoll } from "./active-prompt-poll";
import { isServerInstanceMismatch } from "./state-revision";
import {
  getErrorMessage,
  releaseDraftAttachments,
  removeQueuedPromptFromSessions,
  setSessionFlag,
  type DraftImageAttachment,
  type SessionAgentCommandMap,
  type SessionFlagMap,
} from "./app-utils";
import type { SessionErrorMap, SessionNoticeMap } from "./app-shell-internals";
import { CREATE_SESSION_WORKSPACE_ID } from "./app-shell-internals";
import {
  describeCodexModelAdjustmentNotice,
  describeSessionModelRefreshError,
  normalizedCodexReasoningEffort,
  normalizedRequestedSessionModel,
  resolveUnknownSessionModelSendAttempt,
  usesSessionModelPicker,
} from "./session-model-utils";
import {
  buildOptimisticSessionSettingsUpdate,
  rollbackOptimisticSessionSettingsUpdate,
  sessionSupportsModelRefresh,
} from "./app-session-settings-optimism";
import { upsertSessionStoreSession } from "./session-store";
import { syncActionComposerDraftSlice } from "./app-session-draft-sync";
import { requestedModelForNewSession } from "./app-session-model-requests";
import { conversationMarkerSatisfiesResponse } from "./conversation-marker-response-match";
import { buildCreateConversationMarkerRequest } from "./conversation-marker-requests";
import {
  deleteConversationMarkerLocally,
  upsertConversationMarkerLocally,
} from "./conversation-marker-session-mutations";
import {
  findWorkspacePaneIdForSession,
  openSessionInWorkspaceState,
  reconcileWorkspaceState,
  type WorkspaceState,
} from "./workspace";
import {
  classifyRejectedActionState,
  type StaleActionTargetEvidenceOptions,
} from "./action-state-adoption";
import type {
  AgentReadiness,
  AgentType,
  ApprovalDecision,
  ApprovalPolicy,
  ClaudeApprovalMode,
  ClaudeEffortLevel,
  CodexReasoningEffort,
  CreateConversationMarkerOptions,
  CursorMode,
  GeminiApprovalMode,
  JsonValue,
  McpElicitationAction,
  Project,
  SandboxMode,
  Session,
  SessionSettingsField,
  SessionSettingsValue,
} from "./types";
import {
  resolveProjectRemoteId,
  isLocalRemoteId,
  LOCAL_REMOTE_ID,
} from "./remotes";
import { createOptimisticPendingPrompt } from "./optimistic-pending-prompt";

type UseAppSessionActionsLookups = {
  sessionLookup: Map<string, Session>;
  projectLookup: Map<string, Project>;
  agentReadinessByAgent: Map<AgentType, AgentReadiness>;
  activeSession: Session | null;
  workspace: WorkspaceState;
};

type UseAppSessionActionsDefaults = {
  defaultCodexApprovalPolicy: ApprovalPolicy;
  defaultCodexModel: string;
  defaultCodexReasoningEffort: CodexReasoningEffort;
  defaultCodexSandboxMode: SandboxMode;
  defaultClaudeApprovalMode: ClaudeApprovalMode;
  defaultClaudeEffort: ClaudeEffortLevel;
  defaultClaudeModel: string;
  defaultCursorModel: string;
  defaultCursorMode: CursorMode;
  defaultGeminiApprovalMode: GeminiApprovalMode;
  defaultGeminiModel: string;
};

export type ActionStateClassifierContext = {
  // Snapshot getter for the state inputs that prove a rejected action response
  // still achieved the requested local outcome. Keep classifier-only evidence
  // here so the action hook signature does not grow one ref at a time.
  getSnapshot: () => {
    revision: number | null;
    serverInstanceId: string | null;
    projects: Project[];
    sessions: Session[];
  };
};

type UseAppSessionActionsRefs = {
  isMountedRef: MutableRefObject<boolean>;
  latestStateRevisionRef: MutableRefObject<number | null>;
  lastSeenServerInstanceIdRef: MutableRefObject<string | null>;
  sessionsRef: MutableRefObject<Session[]>;
  actionStateClassifierContextRef: MutableRefObject<ActionStateClassifierContext>;
  draftsBySessionIdRef: MutableRefObject<Record<string, string>>;
  draftAttachmentsBySessionIdRef: MutableRefObject<
    Record<string, DraftImageAttachment[]>
  >;
  confirmedUnknownModelSendsRef: MutableRefObject<Set<string>>;
  activePromptPollCancelRef: MutableRefObject<(() => void) | null>;
  activePromptPollSessionIdRef: MutableRefObject<string | null>;
  refreshingSessionModelOptionIdsRef: MutableRefObject<SessionFlagMap>;
  refreshingAgentCommandSessionIdsRef: MutableRefObject<SessionFlagMap>;
};

type UseAppSessionActionsSetters = {
  setSessions: Dispatch<SetStateAction<Session[]>>;
  setWorkspace: Dispatch<SetStateAction<WorkspaceState>>;
  setRequestError: Dispatch<SetStateAction<string | null>>;
  setIsCreating: Dispatch<SetStateAction<boolean>>;
  setSendingSessionIds: Dispatch<SetStateAction<SessionFlagMap>>;
  setDraftsBySessionId: Dispatch<SetStateAction<Record<string, string>>>;
  setDraftAttachmentsBySessionId: Dispatch<
    SetStateAction<Record<string, DraftImageAttachment[]>>
  >;
  setIsCreatingProject: Dispatch<SetStateAction<boolean>>;
  setNewProjectRootPath: Dispatch<SetStateAction<string>>;
  setNewProjectRemoteId: Dispatch<SetStateAction<string>>;
  setSelectedProjectId: Dispatch<SetStateAction<string>>;
  setStoppingSessionIds: Dispatch<SetStateAction<SessionFlagMap>>;
  setKillingSessionIds: Dispatch<SetStateAction<SessionFlagMap>>;
  setUpdatingSessionIds: Dispatch<SetStateAction<SessionFlagMap>>;
  setSessionSettingNotices: Dispatch<SetStateAction<SessionNoticeMap>>;
  setRefreshingSessionModelOptionIds: Dispatch<SetStateAction<SessionFlagMap>>;
  setSessionModelOptionErrors: Dispatch<SetStateAction<SessionErrorMap>>;
  setAgentCommandsBySessionId: Dispatch<SetStateAction<SessionAgentCommandMap>>;
  setRefreshingAgentCommandSessionIds: Dispatch<SetStateAction<SessionFlagMap>>;
  setAgentCommandErrors: Dispatch<SetStateAction<SessionErrorMap>>;
};

type UseAppSessionActionsParams = {
  lookups: UseAppSessionActionsLookups;
  newProjectRootPath: string;
  newProjectRemoteId: string;
  newProjectUsesLocalRemote: boolean;
  defaults: UseAppSessionActionsDefaults;
  refs: UseAppSessionActionsRefs;
  setters: UseAppSessionActionsSetters;
  adoptState: UseAppLiveStateReturn["adoptState"];
  adoptCreatedSessionResponse: UseAppLiveStateReturn["adoptCreatedSessionResponse"];
  clearHydrationMismatchSessionIds: UseAppLiveStateReturn["clearHydrationMismatchSessionIds"];
  applyControlPanelLayout: (
    nextWorkspace: WorkspaceState,
    side?: "left" | "right",
  ) => WorkspaceState;
  reportRequestError: (error: unknown, options?: { message?: string }) => void;
  requestActionRecoveryResync: (options?: {
    openSessionId?: string;
    paneId?: string | null;
    allowUnknownServerInstance?: boolean;
    sseReconnectRequestId?: number;
  }) => void;
  /**
   * Forces the SSE transport effect to re-run, closing the current
   * `EventSource` (which may still be pointing at a now-exited backend
   * via a stale Vite-proxy connection) and constructing a fresh one.
   *
   * The targeted use case is "send-after-restart": when `handleSend`
   * detects via `isServerInstanceMismatch` that the POST response came
   * from a different backend instance than the tab last saw, the
   * `requestActionRecoveryResync` call repairs the state metadata, but
   * any future streaming chunks (assistant response text deltas) still
   * need a live EventSource on the new backend. Without this callback
   * the user has to hard-refresh to see the streamed response — exactly
   * the symptom in bugs.md "Send-after-restart leaves session preview
   * tooltip stale for 30 s" extended to the live-stream side.
   *
   * Idempotent and cheap; the live-state hook already has retry-backoff
   * on the EventSource recreation path. Safe to call alongside
   * `requestActionRecoveryResync`.
   */
  forceSseReconnect: () => number;
};

type HandleNewSessionArgs = {
  agent: AgentType;
  model: string;
  preferredPaneId?: string | null;
  projectSelectionId?: string;
};

type AdoptActionStateForwardOptions = Pick<
  AdoptStateOptions,
  "openSessionId" | "paneId"
>;

// Adoption navigation and recovery navigation are intentionally separate:
// `openSessionId`/`paneId` are forwarded only to accepted `adoptState` calls,
// while `recoveryOpenSessionId`/`recoveryPaneId` are used only when a rejected
// snapshot schedules authoritative recovery. Session-scoped callers should use
// `adoptSessionActionState` so the recovery target stays tied to the acted
// session without forcing ordinary successful adoption to open that session.
type AdoptActionStateOptions = AdoptActionStateForwardOptions &
  StaleActionTargetEvidenceOptions & {
    hydrationMismatchSessionIds?: Iterable<string>;
    recoveryOpenSessionId?: string;
    recoveryPaneId?: string | null;
  };

// `deferred` covers both defensive unmounted calls and recovery resyncs. Current
// callers only need to distinguish immediate UI success from no-success.
type AdoptActionStateOutcome =
  | "adopted"
  | "stale-success"
  | "deferred";

type SuccessfulAdoptActionStateOutcome = Extract<
  AdoptActionStateOutcome,
  "adopted" | "stale-success"
>;

export type UseAppSessionActionsReturn = {
  handleSend: (
    sessionId: string,
    draftTextOverride?: string,
    expandedTextOverride?: string | null,
  ) => boolean;
  handleDraftAttachmentsAdd: (
    sessionId: string,
    attachments: DraftImageAttachment[],
  ) => void;
  handleDraftAttachmentRemove: (
    sessionId: string,
    attachmentId: string,
  ) => void;
  handleNewSession: (args: HandleNewSessionArgs) => Promise<boolean>;
  handleCloneSessionFromExisting: (
    sessionId: string,
    preferredPaneId?: string | null,
  ) => Promise<boolean>;
  handleCreateProject: () => Promise<boolean>;
  handlePickProjectRoot: () => Promise<void>;
  handleApprovalDecision: (
    sessionId: string,
    messageId: string,
    decision: ApprovalDecision,
  ) => Promise<void>;
  handleUserInputSubmit: (
    sessionId: string,
    messageId: string,
    answers: Record<string, string[]>,
  ) => Promise<void>;
  handleMcpElicitationSubmit: (
    sessionId: string,
    messageId: string,
    action: McpElicitationAction,
    content?: JsonValue,
  ) => Promise<void>;
  handleCodexAppRequestSubmit: (
    sessionId: string,
    messageId: string,
    result: JsonValue,
  ) => Promise<void>;
  handleCancelQueuedPrompt: (
    sessionId: string,
    promptId: string,
  ) => Promise<void>;
  handleStopSession: (sessionId: string) => Promise<void>;
  executeKillSession: (sessionId: string) => Promise<void>;
  handleRenameSession: (
    sessionId: string,
    nextName: string,
  ) => Promise<boolean>;
  handleSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => Promise<void>;
  handleRefreshSessionModelOptions: (
    sessionId: string,
    options?: { reportGlobalError?: boolean },
  ) => Promise<void>;
  handleForkCodexThread: (
    sessionId: string,
    preferredPaneId: string | null,
  ) => Promise<void>;
  handleArchiveCodexThread: (sessionId: string) => Promise<void>;
  handleUnarchiveCodexThread: (sessionId: string) => Promise<void>;
  handleCompactCodexThread: (sessionId: string) => Promise<void>;
  handleRollbackCodexThread: (
    sessionId: string,
    numTurns: number,
  ) => Promise<void>;
  handleRefreshAgentCommands: (sessionId: string) => Promise<void>;
  handleCreateConversationMarker: (
    sessionId: string,
    messageId: string,
    options?: CreateConversationMarkerOptions,
  ) => Promise<boolean>;
  handleUpdateConversationMarker: (
    sessionId: string,
    markerId: string,
    payload: UpdateConversationMarkerRequest,
  ) => Promise<boolean>;
  handleDeleteConversationMarker: (
    sessionId: string,
    markerId: string,
  ) => Promise<boolean>;
};

export function useAppSessionActions(
  params: UseAppSessionActionsParams,
): UseAppSessionActionsReturn {
  const {
    lookups: {
      sessionLookup,
      projectLookup,
      agentReadinessByAgent,
      activeSession,
      workspace,
    },
    newProjectRootPath,
    newProjectRemoteId,
    newProjectUsesLocalRemote,
    defaults: {
      defaultCodexApprovalPolicy,
      defaultCodexModel,
      defaultCodexReasoningEffort,
      defaultCodexSandboxMode,
      defaultClaudeApprovalMode,
      defaultClaudeEffort,
      defaultClaudeModel,
      defaultCursorModel,
      defaultCursorMode,
      defaultGeminiApprovalMode,
      defaultGeminiModel,
    },
    refs: {
      isMountedRef,
      latestStateRevisionRef,
      lastSeenServerInstanceIdRef,
      sessionsRef,
      actionStateClassifierContextRef,
      draftsBySessionIdRef,
      draftAttachmentsBySessionIdRef,
      confirmedUnknownModelSendsRef,
      activePromptPollCancelRef,
      activePromptPollSessionIdRef,
      refreshingSessionModelOptionIdsRef,
      refreshingAgentCommandSessionIdsRef,
    },
    setters: {
      setSessions,
      setWorkspace,
      setRequestError,
      setIsCreating,
      setSendingSessionIds,
      setDraftsBySessionId,
      setDraftAttachmentsBySessionId,
      setIsCreatingProject,
      setNewProjectRootPath,
      setNewProjectRemoteId,
      setSelectedProjectId,
      setStoppingSessionIds,
      setKillingSessionIds,
      setUpdatingSessionIds,
      setSessionSettingNotices,
      setRefreshingSessionModelOptionIds,
      setSessionModelOptionErrors,
      setAgentCommandsBySessionId,
      setRefreshingAgentCommandSessionIds,
      setAgentCommandErrors,
    },
    adoptState,
    adoptCreatedSessionResponse,
    clearHydrationMismatchSessionIds,
    applyControlPanelLayout,
    reportRequestError,
    requestActionRecoveryResync,
    forceSseReconnect,
  } = params;

  function stopStaleSendResponseRecoveryPoll() {
    activePromptPollCancelRef.current?.();
    activePromptPollCancelRef.current = null;
    activePromptPollSessionIdRef.current = null;
  }

  function adoptActionState(
    state: StateResponse,
    options?: AdoptActionStateOptions,
  ): AdoptActionStateOutcome {
    // Defensive for direct helper calls; current action callers check mount
    // immediately before invoking this.
    if (!isMountedRef.current) {
      return "deferred";
    }
    const hydrationMismatchSessionIds = options?.hydrationMismatchSessionIds;
    const recoveryOpenSessionId = options?.recoveryOpenSessionId;
    const recoveryPaneId = options?.recoveryPaneId;
    const forwardedAdoptOptions: AdoptActionStateForwardOptions | undefined =
      options?.openSessionId !== undefined || options?.paneId !== undefined
        ? {
            openSessionId: options.openSessionId,
            paneId: options.paneId,
          }
        : undefined;
    const adopted = adoptState(state, forwardedAdoptOptions);
    if (adopted) {
      return "adopted";
    }
    const classifierContext =
      actionStateClassifierContextRef.current.getSnapshot();
    const rejectedDecision = classifyRejectedActionState({
      currentProjects: new Map(
        classifierContext.projects.map((project) => [project.id, project]),
      ),
      currentRevision: classifierContext.revision,
      currentServerInstanceId: classifierContext.serverInstanceId,
      currentSessions: classifierContext.sessions,
      options,
      state,
    });
    if (rejectedDecision === "stale-success") {
      const sessionIds =
        hydrationMismatchSessionIds ??
        (options?.openSessionId ? [options.openSessionId] : []);
      clearHydrationMismatchSessionIds(sessionIds);
      return "stale-success";
    }
    const recoveryOptions: {
      openSessionId?: string;
      paneId?: string | null;
      allowUnknownServerInstance: true;
    } = {
      allowUnknownServerInstance: true,
    };
    if (recoveryOpenSessionId !== undefined) {
      recoveryOptions.openSessionId = recoveryOpenSessionId;
      recoveryOptions.paneId = recoveryPaneId ?? null;
    }
    // Cross-instance action recovery (approval / user-input / MCP /
    // Codex app-request, plus settings updates and project actions
    // routed through this helper) needs the EventSource recreated for
    // the same reason `handleSend` does: a backend that returned a new
    // `serverInstanceId` is a backend that the existing SSE connection
    // is no longer talking to. Without `forceSseReconnect()`, the
    // `/api/state` probe above repairs the snapshot metadata but live
    // assistant deltas keep arriving on the stale stream (or no stream
    // at all if the proxy returned 502 during the restart gap), and the
    // user has to hard-refresh to see them. Mirrors the `handleSend`
    // mismatch branch; see bugs.md "Cross-instance non-send action
    // recovery does not force SSE recreation".
    let sseReconnectRequestId: number | undefined;
    if (
      isServerInstanceMismatch(
        lastSeenServerInstanceIdRef.current,
        state.serverInstanceId,
      )
    ) {
      sseReconnectRequestId = forceSseReconnect();
    }
    requestActionRecoveryResync(
      sseReconnectRequestId === undefined
        ? recoveryOptions
        : {
            ...recoveryOptions,
            sseReconnectRequestId,
          },
    );
    return "deferred";
  }

  function isSuccessfulAdoptActionStateOutcome(
    outcome: AdoptActionStateOutcome,
  ): outcome is SuccessfulAdoptActionStateOutcome {
    return outcome === "adopted" || outcome === "stale-success";
  }

  function sessionAfterActionStateOutcome(
    sessionId: string,
    state: StateResponse,
    outcome: SuccessfulAdoptActionStateOutcome,
  ) {
    const sessions =
      outcome === "adopted" ? state.sessions : sessionsRef.current;
    return sessions.find((entry) => entry.id === sessionId) ?? null;
  }

  function adoptSessionActionState(
    sessionId: string,
    state: StateResponse,
    options?: Pick<
      AdoptActionStateOptions,
      "recoveryPaneId" | "staleSuccessSessionEvidence"
    >,
  ) {
    return adoptActionState(state, {
      ...options,
      staleSuccessSessionId: sessionId,
      hydrationMismatchSessionIds: [sessionId],
      recoveryOpenSessionId: sessionId,
      recoveryPaneId:
        options?.recoveryPaneId ??
        findWorkspacePaneIdForSession(workspace, sessionId),
    });
  }

  function startStaleSendResponseRecoveryPoll(sessionId: string) {
    stopStaleSendResponseRecoveryPoll();
    const cancelPoll = startActivePromptPoll({
      fetchState,
      isMounted: () => isMountedRef.current,
      onState: (freshState) => {
        const adopted = adoptState(freshState, {
          allowUnknownServerInstance: true,
        });
        const shouldStop = !freshState.sessions?.some(
          (session) => session.id === sessionId && session.status === "active",
        );
        if (
          adopted &&
          shouldStop &&
          activePromptPollSessionIdRef.current === sessionId
        ) {
          activePromptPollSessionIdRef.current = null;
        }
        return adopted && shouldStop;
      },
    });
    activePromptPollSessionIdRef.current = sessionId;
    activePromptPollCancelRef.current = () => {
      cancelPoll();
      if (activePromptPollSessionIdRef.current === sessionId) {
        activePromptPollSessionIdRef.current = null;
      }
      if (activePromptPollCancelRef.current) {
        activePromptPollCancelRef.current = null;
      }
    };
  }

  function syncComposerDraftSlice(
    sessionId: string,
    committedDraft: string,
    draftAttachments: readonly DraftImageAttachment[],
  ) {
    syncActionComposerDraftSlice(
      { draftsBySessionIdRef, draftAttachmentsBySessionIdRef },
      sessionId,
      committedDraft,
      draftAttachments,
    );
  }

  function syncSessionSlice(session: Session) {
    upsertSessionStoreSession({
      session,
      committedDraft: draftsBySessionIdRef.current[session.id] ?? "",
      draftAttachments:
        draftAttachmentsBySessionIdRef.current[session.id] ?? [],
    });
  }

  function updateSessionLocally(
    sessionId: string,
    update: (session: Session) => Session,
  ) {
    const nextSessions = sessionsRef.current.map((entry) => {
      if (entry.id !== sessionId) {
        return entry;
      }

      return update(entry);
    });
    const hasChanged = nextSessions.some(
      (entry, index) => entry !== sessionsRef.current[index],
    );
    if (!hasChanged) {
      return;
    }

    sessionsRef.current = nextSessions;
    const updatedSession =
      nextSessions.find((entry) => entry.id === sessionId) ?? null;
    if (updatedSession) {
      syncSessionSlice(updatedSession);
    }
    setSessions(nextSessions);
    setWorkspace((current) =>
      applyControlPanelLayout(reconcileWorkspaceState(current, nextSessions)),
    );
  }

  function removeOptimisticPendingPrompt(sessionId: string, promptId: string) {
    updateSessionLocally(sessionId, (currentSession) => {
      const [nextSession] = removeQueuedPromptFromSessions(
        [currentSession],
        sessionId,
        promptId,
      );
      return nextSession ?? currentSession;
    });
  }

  function shouldApplyMarkerMutationResponse(
    sessionId: string,
    response: {
      revision: number;
      serverInstanceId: string;
      marker?: NonNullable<Session["markers"]>[number];
      markerId?: string;
      sessionMutationStamp?: number | null;
    },
    options: { deleted?: boolean } = {},
  ): "apply" | "stale-success" | "deferred" {
    const markerId = response.marker?.id ?? response.markerId;
    if (!markerId) {
      return "deferred";
    }

    if (
      isServerInstanceMismatch(
        lastSeenServerInstanceIdRef.current,
        response.serverInstanceId,
      )
    ) {
      // A server-instance mismatch means this response came from a restarted
      // backend. Do not optimistically apply its marker; let recovery adopt
      // the authoritative snapshot from the new instance.
      const sseReconnectRequestId = forceSseReconnect();
      requestActionRecoveryResync({
        openSessionId: sessionId,
        paneId: findWorkspacePaneIdForSession(workspace, sessionId),
        allowUnknownServerInstance: true,
        sseReconnectRequestId,
      });
      return "deferred";
    }

    if (
      latestStateRevisionRef.current !== null &&
      response.revision <= latestStateRevisionRef.current
    ) {
      const currentSession =
        sessionsRef.current.find((session) => session.id === sessionId) ?? null;
      const currentMarker = currentSession?.markers?.find(
        (marker) => marker.id === markerId,
      );
      const responseMutationStamp = response.sessionMutationStamp ?? null;
      const currentMutationStamp = currentSession?.sessionMutationStamp ?? null;
      const targetStateMatches = options.deleted
        ? currentMarker === undefined
        : conversationMarkerSatisfiesResponse(currentMarker, response.marker);
      const hasTargetEvidence =
        currentSession !== null &&
        targetStateMatches &&
        (responseMutationStamp === null ||
          (currentMutationStamp !== null &&
            currentMutationStamp >= responseMutationStamp));
      if (hasTargetEvidence) {
        return "stale-success";
      }

      requestActionRecoveryResync({
        openSessionId: sessionId,
        paneId: findWorkspacePaneIdForSession(workspace, sessionId),
        allowUnknownServerInstance: true,
      });
      return "deferred";
    }

    latestStateRevisionRef.current = response.revision;
    return "apply";
  }

  async function openCreatedSession(
    created: Awaited<ReturnType<typeof createSession>>,
    paneId: string | null,
    agent: AgentType,
  ) {
    const canOpenStaleCreatedSession = () => {
      if (
        sessionsRef.current.some((session) => session.id === created.sessionId)
      ) {
        return true;
      }
      requestActionRecoveryResync({
        openSessionId: created.sessionId,
        paneId,
        allowUnknownServerInstance: true,
      });
      return false;
    };
    const adopted = adoptCreatedSessionResponse(created, {
      openSessionId: created.sessionId,
      paneId,
    });
    const canUseCreatedSession =
      adopted === "adopted" ||
      (adopted === "stale" && canOpenStaleCreatedSession());
    if (adopted === "stale" && canUseCreatedSession) {
      setWorkspace((current) =>
        applyControlPanelLayout(
          openSessionInWorkspaceState(current, created.sessionId, paneId),
        ),
      );
    }
    if (canUseCreatedSession && sessionSupportsModelRefresh(agent)) {
      void handleRefreshSessionModelOptions(created.sessionId, {
        reportGlobalError: false,
      });
    }
  }

  function handleSend(
    sessionId: string,
    draftTextOverride?: string,
    expandedTextOverride?: string | null,
  ) {
    const session = sessionLookup.get(sessionId);
    if (!session) {
      return false;
    }

    const draftText =
      draftTextOverride ?? draftsBySessionIdRef.current[sessionId] ?? "";
    const prompt = draftText.trim();
    const expandedText = expandedTextOverride?.trim() || null;
    const normalizedExpandedText =
      expandedText && expandedText !== prompt ? expandedText : null;
    const attachments = draftAttachmentsBySessionIdRef.current[sessionId] ?? [];
    if (!prompt && attachments.length === 0) {
      return false;
    }
    if (
      session.agent === "Codex" &&
      session.externalSessionId &&
      session.codexThreadState === "archived"
    ) {
      setRequestError(
        "This Codex thread is archived. Unarchive it before sending another prompt.",
      );
      return false;
    }
    const unknownModelAttempt = resolveUnknownSessionModelSendAttempt(
      confirmedUnknownModelSendsRef.current,
      session,
    );
    confirmedUnknownModelSendsRef.current =
      unknownModelAttempt.nextConfirmedKeys;
    if (!unknownModelAttempt.allowSend) {
      setRequestError(unknownModelAttempt.warning);
      return false;
    }

    setSendingSessionIds((current) => setSessionFlag(current, sessionId, true));
    syncComposerDraftSlice(sessionId, "", []);
    setDraftsBySessionId((current) => {
      if ((current[sessionId] ?? "") === "") {
        return current;
      }

      return {
        ...current,
        [sessionId]: "",
      };
    });
    setDraftAttachmentsBySessionId((current) => {
      if (!current[sessionId]?.length) {
        return current;
      }

      const nextState = { ...current };
      delete nextState[sessionId];
      return nextState;
    });
    const optimisticPendingPrompt = createOptimisticPendingPrompt(
      sessionId,
      prompt,
      normalizedExpandedText,
      attachments,
    );
    const optimisticPromptId = optimisticPendingPrompt.id;
    updateSessionLocally(sessionId, (currentSession) => ({
      ...currentSession,
      pendingPrompts: [
        ...(currentSession.pendingPrompts ?? []),
        optimisticPendingPrompt,
      ],
    }));

    void (async () => {
      try {
        const state = await sendMessage(
          sessionId,
          prompt,
          attachments.map((attachment) => ({
            data: attachment.base64Data,
            fileName: attachment.fileName,
            mediaType: attachment.mediaType,
          })),
          normalizedExpandedText,
        );
        if (!isMountedRef.current) {
          releaseDraftAttachments(attachments);
          return;
        }
        const adopted = adoptState(state);
        removeOptimisticPendingPrompt(sessionId, optimisticPromptId);
        releaseDraftAttachments(attachments);
        setRequestError(null);
        const responseKeepsSessionActive = state.sessions?.some(
          (candidate) =>
            candidate.id === sessionId && candidate.status === "active",
        );
        if (
          !adopted &&
          isServerInstanceMismatch(
            lastSeenServerInstanceIdRef.current,
            state.serverInstanceId,
          )
        ) {
          // The send response was rejected by `adoptState` AND it carried a
          // different `serverInstanceId` from the last one this tab saw —
          // that's the "backend restarted between the EventSource opening
          // and this POST returning" case. Two things must happen for the
          // user to see streaming chunks of the assistant's response on
          // the new backend without a hard refresh:
          //
          // 1. State metadata (preview, message count, status) needs to
          //    reflect the new instance. `requestActionRecoveryResync`
          //    fires an immediate `/api/state` probe with
          //    `allowUnknownServerInstance: true`; the probe response is
          //    a fresh observation of the new instance, so adoption flips
          //    the local view to the new state within a single round trip.
          //
          // 2. The `EventSource` needs to be on the NEW backend, not stuck
          //    on a Vite-proxy / browser cached connection to the old one.
          //    `forceSseReconnect` re-runs the transport effect: the dead
          //    socket is closed via cleanup and a fresh `EventSource` is
          //    constructed against the new backend. Without this, future
          //    streaming chunks (text deltas of the assistant's reply)
          //    never reach the tab — the user has to hard-refresh to see
          //    the response, exactly the symptom that motivated this fix.
          //    Codex/Idle tabs in the same browser that have no active
          //    streaming are unaffected because their state is fully
          //    captured in the `/api/state` probe.
          //
          // Stale same-instance rejections (e.g. SSE already advanced past
          // this response's revision) still route through the existing 30 s
          // safety-net poll because the tab is otherwise healthy and forcing
          // an EventSource recreate would be wasteful. See bugs.md
          // "Send-after-restart leaves session preview tooltip stale for
          // 30 s".
          const sseReconnectRequestId = forceSseReconnect();
          requestActionRecoveryResync({
            allowUnknownServerInstance: true,
            sseReconnectRequestId,
          });
        }
        if (!adopted || responseKeepsSessionActive) {
          startStaleSendResponseRecoveryPoll(sessionId);
        }
      } catch (error) {
        if (!isMountedRef.current) {
          releaseDraftAttachments(attachments);
          return;
        }
        const currentDraft = draftsBySessionIdRef.current[sessionId] ?? "";
        const currentAttachments =
          draftAttachmentsBySessionIdRef.current[sessionId] ?? [];
        const restoredDraft = !!draftText && currentDraft === "";
        const restoredAttachments =
          attachments.length > 0 && currentAttachments.length === 0;

        if (restoredDraft || restoredAttachments) {
          syncComposerDraftSlice(
            sessionId,
            restoredDraft ? draftText : currentDraft,
            restoredAttachments ? attachments : currentAttachments,
          );
        }

        setDraftsBySessionId((current) => {
          if (!draftText || (current[sessionId] ?? "") !== "") {
            return current;
          }

          return {
            ...current,
            [sessionId]: draftText,
          };
        });
        setDraftAttachmentsBySessionId((current) => {
          if (
            attachments.length === 0 ||
            (current[sessionId]?.length ?? 0) > 0
          ) {
            return current;
          }

          return {
            ...current,
            [sessionId]: attachments,
          };
        });
        if (!restoredAttachments) {
          releaseDraftAttachments(attachments);
        }
        removeOptimisticPendingPrompt(sessionId, optimisticPromptId);
        reportRequestError(error);
      } finally {
        if (isMountedRef.current) {
          setSendingSessionIds((current) =>
            setSessionFlag(current, sessionId, false),
          );
        }
      }
    })();

    return true;
  }

  function handleDraftAttachmentsAdd(
    sessionId: string,
    attachments: DraftImageAttachment[],
  ) {
    const nextAttachments = [
      ...(draftAttachmentsBySessionIdRef.current[sessionId] ?? []),
      ...attachments,
    ];
    syncComposerDraftSlice(
      sessionId,
      draftsBySessionIdRef.current[sessionId] ?? "",
      nextAttachments,
    );
    setDraftAttachmentsBySessionId((current) => ({
      ...current,
      [sessionId]: [...(current[sessionId] ?? []), ...attachments],
    }));
  }

  function handleDraftAttachmentRemove(
    sessionId: string,
    attachmentId: string,
  ) {
    const existingAttachments =
      draftAttachmentsBySessionIdRef.current[sessionId] ?? [];
    const removed = existingAttachments.filter(
      (attachment) => attachment.id === attachmentId,
    );
    if (removed.length === 0) {
      return;
    }

    releaseDraftAttachments(removed);
    const nextRefAttachments = existingAttachments.filter(
      (attachment) => attachment.id !== attachmentId,
    );
    syncComposerDraftSlice(
      sessionId,
      draftsBySessionIdRef.current[sessionId] ?? "",
      nextRefAttachments,
    );
    setDraftAttachmentsBySessionId((current) => {
      const existing = current[sessionId];
      if (!existing) {
        return current;
      }

      const nextAttachments = existing.filter(
        (attachment) => attachment.id !== attachmentId,
      );
      if (nextAttachments.length === existing.length) {
        return current;
      }
      if (nextAttachments.length === 0) {
        const nextState = { ...current };
        delete nextState[sessionId];
        return nextState;
      }

      return {
        ...current,
        [sessionId]: nextAttachments,
      };
    });
  }

  async function handleNewSession({
    agent,
    model,
    preferredPaneId = null,
    projectSelectionId = CREATE_SESSION_WORKSPACE_ID,
  }: HandleNewSessionArgs) {
    const trimmedModel = model.trim();
    if (!trimmedModel && !usesSessionModelPicker(agent)) {
      setRequestError("Choose a model.");
      return false;
    }
    if (
      projectSelectionId === CREATE_SESSION_WORKSPACE_ID &&
      !!activeSession?.projectId &&
      !projectLookup.has(activeSession.projectId)
    ) {
      setRequestError(
        "The current workspace project is unavailable. Choose a project before creating a session.",
      );
      return false;
    }
    const workspaceProject =
      projectSelectionId === CREATE_SESSION_WORKSPACE_ID &&
      activeSession?.projectId &&
      projectLookup.has(activeSession.projectId)
        ? (projectLookup.get(activeSession.projectId) ?? null)
        : null;
    const targetProject =
      projectSelectionId !== CREATE_SESSION_WORKSPACE_ID
        ? (projectLookup.get(projectSelectionId) ?? null)
        : workspaceProject &&
            !isLocalRemoteId(resolveProjectRemoteId(workspaceProject))
          ? workspaceProject
          : null;
    const targetUsesRemoteProject =
      !!targetProject &&
      !isLocalRemoteId(resolveProjectRemoteId(targetProject));
    const readiness = targetUsesRemoteProject
      ? null
      : agentReadinessByAgent.get(agent);
    if (readiness?.blocking) {
      setRequestError(readiness.detail);
      return false;
    }

    setIsCreating(true);
    try {
      const targetPaneId = preferredPaneId ?? workspace.activePaneId;
      const targetProjectId =
        projectSelectionId === CREATE_SESSION_WORKSPACE_ID
          ? null
          : projectSelectionId;
      const requestedModel = requestedModelForNewSession(agent, model, {
        Claude: defaultClaudeModel,
        Codex: defaultCodexModel,
        Cursor: defaultCursorModel,
        Gemini: defaultGeminiModel,
      });
      const created = await createSession({
        agent,
        model: requestedModel,
        approvalPolicy:
          agent === "Codex" ? defaultCodexApprovalPolicy : undefined,
        reasoningEffort:
          agent === "Codex" ? defaultCodexReasoningEffort : undefined,
        cursorMode: agent === "Cursor" ? defaultCursorMode : undefined,
        claudeApprovalMode:
          agent === "Claude" ? defaultClaudeApprovalMode : undefined,
        claudeEffort: agent === "Claude" ? defaultClaudeEffort : undefined,
        geminiApprovalMode:
          agent === "Gemini" ? defaultGeminiApprovalMode : undefined,
        sandboxMode: agent === "Codex" ? defaultCodexSandboxMode : undefined,
        projectId: targetProjectId ?? targetProject?.id ?? undefined,
        workdir:
          targetProjectId || targetProject ? undefined : activeSession?.workdir,
      });
      if (!isMountedRef.current) {
        return false;
      }

      await openCreatedSession(created, targetPaneId, agent);
      setRequestError(null);
      return true;
    } catch (error) {
      if (!isMountedRef.current) {
        return false;
      }
      reportRequestError(error);
      return false;
    } finally {
      if (isMountedRef.current) {
        setIsCreating(false);
      }
    }
  }

  async function handleCloneSessionFromExisting(
    sessionId: string,
    preferredPaneId: string | null = null,
  ) {
    const session = sessionLookup.get(sessionId);
    if (!session) {
      return false;
    }

    setIsCreating(true);
    try {
      const targetPaneId =
        preferredPaneId ??
        findWorkspacePaneIdForSession(workspace, session.id) ??
        workspace.activePaneId;
      const created = await createSession({
        agent: session.agent,
        model: session.model,
        approvalPolicy:
          session.agent === "Codex"
            ? (session.approvalPolicy ?? defaultCodexApprovalPolicy)
            : undefined,
        reasoningEffort:
          session.agent === "Codex"
            ? (session.reasoningEffort ?? defaultCodexReasoningEffort)
            : undefined,
        cursorMode:
          session.agent === "Cursor"
            ? (session.cursorMode ?? defaultCursorMode)
            : undefined,
        claudeApprovalMode:
          session.agent === "Claude"
            ? (session.claudeApprovalMode ?? defaultClaudeApprovalMode)
            : undefined,
        claudeEffort:
          session.agent === "Claude"
            ? (session.claudeEffort ?? defaultClaudeEffort)
            : undefined,
        geminiApprovalMode:
          session.agent === "Gemini"
            ? (session.geminiApprovalMode ?? defaultGeminiApprovalMode)
            : undefined,
        sandboxMode:
          session.agent === "Codex"
            ? (session.sandboxMode ?? defaultCodexSandboxMode)
            : undefined,
        projectId:
          session.projectId && projectLookup.has(session.projectId)
            ? session.projectId
            : undefined,
        workdir: session.workdir,
      });
      if (!isMountedRef.current) {
        return false;
      }

      await openCreatedSession(created, targetPaneId, session.agent);
      setRequestError(null);
      return true;
    } catch (error) {
      if (!isMountedRef.current) {
        return false;
      }

      reportRequestError(error);
      return false;
    } finally {
      if (isMountedRef.current) {
        setIsCreating(false);
      }
    }
  }

  async function handleCreateProject() {
    const rootPath = newProjectRootPath.trim();
    if (!rootPath) {
      setRequestError("Enter a project root path.");
      return false;
    }

    setIsCreatingProject(true);
    try {
      const created = await createProject({
        rootPath,
        remoteId: newProjectRemoteId,
      });
      if (!isMountedRef.current) {
        return false;
      }
      // Project creation is global; there is no session-scoped hydration
      // mismatch entry for `adoptSessionActionState` to clear.
      if (
        !isSuccessfulAdoptActionStateOutcome(
          adoptActionState(created.state, {
            staleSuccessProjectId: created.projectId,
          }),
        )
      ) {
        return false;
      }
      setSelectedProjectId(created.projectId);
      setNewProjectRootPath("");
      setNewProjectRemoteId(LOCAL_REMOTE_ID);
      setRequestError(null);
      return true;
    } catch (error) {
      if (!isMountedRef.current) {
        return false;
      }
      reportRequestError(error);
      return false;
    } finally {
      if (isMountedRef.current) {
        setIsCreatingProject(false);
      }
    }
  }

  async function handlePickProjectRoot() {
    if (!newProjectUsesLocalRemote) {
      setRequestError(
        "Remote projects need a path from the remote machine. Enter it manually.",
      );
      return;
    }

    setIsCreatingProject(true);
    try {
      const response = await pickProjectRoot();
      if (!isMountedRef.current) {
        return;
      }
      if (response.path) {
        setNewProjectRootPath(response.path);
        setRequestError(null);
      }
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      reportRequestError(error);
    } finally {
      if (isMountedRef.current) {
        setIsCreatingProject(false);
      }
    }
  }

  async function handleApprovalDecision(
    sessionId: string,
    messageId: string,
    decision: ApprovalDecision,
  ) {
    try {
      const state = await submitApproval(sessionId, messageId, decision);
      if (!isMountedRef.current) {
        return;
      }
      if (
        !isSuccessfulAdoptActionStateOutcome(
          adoptSessionActionState(sessionId, state),
        )
      ) {
        return;
      }
      setRequestError(null);
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      reportRequestError(error);
    }
  }

  async function handleUserInputSubmit(
    sessionId: string,
    messageId: string,
    answers: Record<string, string[]>,
  ) {
    try {
      const state = await submitUserInput(sessionId, messageId, answers);
      if (!isMountedRef.current) {
        return;
      }
      if (
        !isSuccessfulAdoptActionStateOutcome(
          adoptSessionActionState(sessionId, state),
        )
      ) {
        return;
      }
      setRequestError(null);
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      reportRequestError(error);
    }
  }

  async function handleMcpElicitationSubmit(
    sessionId: string,
    messageId: string,
    action: McpElicitationAction,
    content?: JsonValue,
  ) {
    try {
      const state = await submitMcpElicitation(
        sessionId,
        messageId,
        action,
        content,
      );
      if (!isMountedRef.current) {
        return;
      }
      if (
        !isSuccessfulAdoptActionStateOutcome(
          adoptSessionActionState(sessionId, state),
        )
      ) {
        return;
      }
      setRequestError(null);
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      reportRequestError(error);
    }
  }

  async function handleCodexAppRequestSubmit(
    sessionId: string,
    messageId: string,
    result: JsonValue,
  ) {
    try {
      const state = await submitCodexAppRequest(sessionId, messageId, result);
      if (!isMountedRef.current) {
        return;
      }
      if (
        !isSuccessfulAdoptActionStateOutcome(
          adoptSessionActionState(sessionId, state),
        )
      ) {
        return;
      }
      setRequestError(null);
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      reportRequestError(error);
    }
  }

  async function handleCancelQueuedPrompt(sessionId: string, promptId: string) {
    const previousSessions = sessionsRef.current;
    const next = removeQueuedPromptFromSessions(
      previousSessions,
      sessionId,
      promptId,
    );
    const hasChanged = next.some(
      (entry, index) => entry !== previousSessions[index],
    );
    if (hasChanged) {
      sessionsRef.current = next;
      const updatedSession =
        next.find((entry) => entry.id === sessionId) ?? null;
      if (updatedSession) {
        syncSessionSlice(updatedSession);
      }
      setSessions(next);
    }
    try {
      const state = await cancelQueuedPrompt(sessionId, promptId);
      if (!isMountedRef.current) {
        return;
      }
      if (
        !isSuccessfulAdoptActionStateOutcome(
          adoptSessionActionState(sessionId, state),
        )
      ) {
        return;
      }
      setRequestError(null);
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      try {
        const state = await fetchState();
        if (isMountedRef.current) {
          // Passive refresh after a failed cancel request. Do not classify stale
          // same-instance snapshots as action success or trigger action recovery
          // from this best-effort probe; the original error is reported below.
          adoptState(state);
        }
      } catch {
        // Keep the original request error below; state refresh is best-effort.
      }
      if (!isMountedRef.current) {
        return;
      }
      reportRequestError(error);
    }
  }

  async function handleStopSession(sessionId: string) {
    setStoppingSessionIds((current) =>
      setSessionFlag(current, sessionId, true),
    );
    try {
      const state = await stopSession(sessionId);
      if (!isMountedRef.current) {
        return;
      }
      if (
        !isSuccessfulAdoptActionStateOutcome(
          adoptSessionActionState(sessionId, state),
        )
      ) {
        return;
      }
      setRequestError(null);
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      reportRequestError(error);
    } finally {
      if (isMountedRef.current) {
        setStoppingSessionIds((current) =>
          setSessionFlag(current, sessionId, false),
        );
      }
    }
  }

  async function executeKillSession(sessionId: string) {
    setKillingSessionIds((current) => setSessionFlag(current, sessionId, true));
    try {
      const state = await killSession(sessionId);
      if (!isMountedRef.current) {
        return;
      }

      if (
        !isSuccessfulAdoptActionStateOutcome(
          adoptSessionActionState(sessionId, state),
        )
      ) {
        return;
      }
      setRequestError(null);
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }

      reportRequestError(error);
    } finally {
      if (isMountedRef.current) {
        setKillingSessionIds((current) =>
          setSessionFlag(current, sessionId, false),
        );
      }
    }
  }

  async function handleRenameSession(sessionId: string, nextName: string) {
    setUpdatingSessionIds((current) =>
      setSessionFlag(current, sessionId, true),
    );
    try {
      const state = await renameSession(sessionId, nextName);
      if (!isMountedRef.current) {
        return false;
      }

      if (
        !isSuccessfulAdoptActionStateOutcome(
          adoptSessionActionState(sessionId, state),
        )
      ) {
        return false;
      }
      setRequestError(null);
      return true;
    } catch (error) {
      if (!isMountedRef.current) {
        return false;
      }

      reportRequestError(error);
      return false;
    } finally {
      if (isMountedRef.current) {
        setUpdatingSessionIds((current) =>
          setSessionFlag(current, sessionId, false),
        );
      }
    }
  }

  async function handleSessionSettingsChange(
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) {
    const session = sessionLookup.get(sessionId);
    if (!session) {
      return;
    }
    const normalizedModelValue =
      field === "model"
        ? normalizedRequestedSessionModel(session, value as string)
        : null;
    const payload =
      session.agent === "Codex"
        ? {
            ...(field === "model"
              ? { model: normalizedModelValue ?? (value as string) }
              : {}),
            reasoningEffort:
              field === "reasoningEffort"
                ? (value as CodexReasoningEffort)
                : normalizedCodexReasoningEffort(
                    session,
                    field === "model"
                      ? (normalizedModelValue ?? (value as string))
                      : session.model,
                  ),
            sandboxMode:
              field === "sandboxMode"
                ? (value as SandboxMode)
                : (session.sandboxMode ?? "workspace-write"),
            approvalPolicy:
              field === "approvalPolicy"
                ? (value as ApprovalPolicy)
                : (session.approvalPolicy ?? "never"),
          }
        : session.agent === "Cursor"
          ? field === "model"
            ? {
                model: normalizedModelValue ?? (value as string),
              }
            : field === "cursorMode"
              ? {
                  cursorMode: value as CursorMode,
                }
              : null
          : session.agent === "Claude"
            ? field === "model"
              ? {
                  model: normalizedModelValue ?? (value as string),
                }
              : field === "claudeApprovalMode"
                ? {
                    claudeApprovalMode: value as ClaudeApprovalMode,
                  }
                : field === "claudeEffort"
                  ? {
                      claudeEffort: value as ClaudeEffortLevel,
                    }
                  : null
            : session.agent === "Gemini"
              ? field === "model"
                ? {
                    model: normalizedModelValue ?? (value as string),
                  }
                : field === "geminiApprovalMode"
                  ? {
                      geminiApprovalMode: value as GeminiApprovalMode,
                    }
                  : null
              : null;
    if (!payload) {
      return;
    }

    const optimisticSession = buildOptimisticSessionSettingsUpdate(
      session,
      field,
      value,
    );
    const hasOptimisticUpdate = optimisticSession !== session;
    const preOptimisticSession =
      sessionsRef.current.find((entry) => entry.id === sessionId) ?? null;

    setRequestError(null);
    if (hasOptimisticUpdate) {
      updateSessionLocally(sessionId, () => optimisticSession);
    }
    setUpdatingSessionIds((current) =>
      setSessionFlag(current, sessionId, true),
    );
    try {
      const state = await updateSessionSettings(sessionId, payload);
      if (!isMountedRef.current) {
        return;
      }
      const adoptionOutcome = adoptSessionActionState(sessionId, state, {
        staleSuccessSessionEvidence: preOptimisticSession,
      });
      if (!isSuccessfulAdoptActionStateOutcome(adoptionOutcome)) {
        return;
      }
      const updatedSession = sessionAfterActionStateOutcome(
        sessionId,
        state,
        adoptionOutcome,
      );
      const nextNotice =
        session.agent === "Codex" && field === "model" && updatedSession
          ? describeCodexModelAdjustmentNotice(session, updatedSession)
          : null;
      setSessionSettingNotices((current) => {
        if (nextNotice) {
          return {
            ...current,
            [sessionId]: nextNotice,
          };
        }
        if (!current[sessionId]) {
          return current;
        }

        const nextState = { ...current };
        delete nextState[sessionId];
        return nextState;
      });
      setRequestError(null);
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      if (hasOptimisticUpdate) {
        updateSessionLocally(sessionId, (current) =>
          rollbackOptimisticSessionSettingsUpdate(
            current,
            session,
            optimisticSession,
          ),
        );
      }
      reportRequestError(error);
    } finally {
      if (isMountedRef.current) {
        setUpdatingSessionIds((current) =>
          setSessionFlag(current, sessionId, false),
        );
      }
    }
  }

  async function handleRefreshSessionModelOptions(
    sessionId: string,
    options?: { reportGlobalError?: boolean },
  ) {
    if (!isMountedRef.current) {
      return;
    }
    const previousSession =
      sessionsRef.current.find((entry) => entry.id === sessionId) ?? null;
    if (refreshingSessionModelOptionIdsRef.current[sessionId]) {
      return;
    }
    const nextRefreshingSessionIds = setSessionFlag(
      refreshingSessionModelOptionIdsRef.current,
      sessionId,
      true,
    );
    refreshingSessionModelOptionIdsRef.current = nextRefreshingSessionIds;
    setRefreshingSessionModelOptionIds(nextRefreshingSessionIds);

    setSessionModelOptionErrors((current) => {
      if (!current[sessionId]) {
        return current;
      }

      const nextState = { ...current };
      delete nextState[sessionId];
      return nextState;
    });

    try {
      const state = await refreshSessionModelOptions(sessionId);
      if (!isMountedRef.current) {
        return;
      }
      const adoptionOutcome = adoptSessionActionState(sessionId, state);
      if (!isSuccessfulAdoptActionStateOutcome(adoptionOutcome)) {
        return;
      }
      if (previousSession?.agent === "Codex") {
        const refreshedSession = sessionAfterActionStateOutcome(
          sessionId,
          state,
          adoptionOutcome,
        );
        const nextNotice = refreshedSession
          ? describeCodexModelAdjustmentNotice(
              previousSession,
              refreshedSession,
            )
          : null;
        if (nextNotice) {
          setSessionSettingNotices((current) => ({
            ...current,
            [sessionId]: nextNotice,
          }));
        }
      }
      setRequestError(null);
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      const rawMessage = getErrorMessage(error);
      const session =
        sessionsRef.current.find((entry) => entry.id === sessionId) ?? null;
      const message = session
        ? describeSessionModelRefreshError(
            session.agent,
            rawMessage,
            agentReadinessByAgent.get(session.agent) ?? null,
          )
        : rawMessage;
      setSessionModelOptionErrors((current) => ({
        ...current,
        [sessionId]: message,
      }));
      if (options?.reportGlobalError !== false) {
        reportRequestError(error, { message });
      }
    } finally {
      if (!isMountedRef.current) {
        return;
      }
      const nextRefreshingSessionIds = setSessionFlag(
        refreshingSessionModelOptionIdsRef.current,
        sessionId,
        false,
      );
      refreshingSessionModelOptionIdsRef.current = nextRefreshingSessionIds;
      setRefreshingSessionModelOptionIds(nextRefreshingSessionIds);
    }
  }

  async function runCodexThreadStateAction(
    sessionId: string,
    request: () => Promise<StateResponse>,
    successNotice: string,
  ) {
    setRequestError(null);
    setUpdatingSessionIds((current) =>
      setSessionFlag(current, sessionId, true),
    );
    try {
      const state = await request();
      if (!isMountedRef.current) {
        return;
      }
      if (
        !isSuccessfulAdoptActionStateOutcome(
          adoptSessionActionState(sessionId, state),
        )
      ) {
        return;
      }
      setSessionSettingNotices((current) => ({
        ...current,
        [sessionId]: successNotice,
      }));
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      reportRequestError(error);
    } finally {
      if (isMountedRef.current) {
        setUpdatingSessionIds((current) =>
          setSessionFlag(current, sessionId, false),
        );
      }
    }
  }

  async function handleForkCodexThread(
    sessionId: string,
    preferredPaneId: string | null,
  ) {
    setRequestError(null);
    setUpdatingSessionIds((current) =>
      setSessionFlag(current, sessionId, true),
    );
    try {
      const created = await forkCodexThread(sessionId);
      if (!isMountedRef.current) {
        return;
      }
      const adopted = adoptCreatedSessionResponse(created, {
        openSessionId: created.sessionId,
        paneId: preferredPaneId,
      });
      const canOpenStaleCreatedSession =
        adopted === "stale" &&
        sessionsRef.current.some((session) => session.id === created.sessionId);
      if (adopted === "stale" && !canOpenStaleCreatedSession) {
        requestActionRecoveryResync({
          openSessionId: created.sessionId,
          paneId: preferredPaneId,
          allowUnknownServerInstance: true,
        });
      }
      const canUseCreatedSession =
        adopted === "adopted" || canOpenStaleCreatedSession;
      if (canOpenStaleCreatedSession) {
        setWorkspace((current) =>
          applyControlPanelLayout(
            openSessionInWorkspaceState(
              current,
              created.sessionId,
              preferredPaneId,
            ),
          ),
        );
      }
      if (canUseCreatedSession) {
        setSessionSettingNotices((current) => ({
          ...current,
          [sessionId]: "Forked the live Codex thread into a new session.",
          [created.sessionId]:
            "This session is attached to a forked Codex thread. Earlier Codex history was restored from Codex where available.",
        }));
      }
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      reportRequestError(error);
    } finally {
      if (isMountedRef.current) {
        setUpdatingSessionIds((current) =>
          setSessionFlag(current, sessionId, false),
        );
      }
    }
  }

  async function handleArchiveCodexThread(sessionId: string) {
    await runCodexThreadStateAction(
      sessionId,
      () => archiveCodexThread(sessionId),
      "Archived the live Codex thread for this session.",
    );
  }

  async function handleUnarchiveCodexThread(sessionId: string) {
    await runCodexThreadStateAction(
      sessionId,
      () => unarchiveCodexThread(sessionId),
      "Restored the archived Codex thread for this session.",
    );
  }

  async function handleCompactCodexThread(sessionId: string) {
    await runCodexThreadStateAction(
      sessionId,
      () => compactCodexThread(sessionId),
      "Started Codex context compaction for this session.",
    );
  }

  async function handleRollbackCodexThread(
    sessionId: string,
    numTurns: number,
  ) {
    const turnLabel = numTurns === 1 ? "turn" : "turns";
    await runCodexThreadStateAction(
      sessionId,
      () => rollbackCodexThread(sessionId, numTurns),
      `Rolled the live Codex thread back by ${numTurns} ${turnLabel}.`,
    );
  }

  async function handleRefreshAgentCommands(sessionId: string) {
    if (refreshingAgentCommandSessionIdsRef.current[sessionId]) {
      return;
    }

    const nextRefreshingSessionIds = setSessionFlag(
      refreshingAgentCommandSessionIdsRef.current,
      sessionId,
      true,
    );
    refreshingAgentCommandSessionIdsRef.current = nextRefreshingSessionIds;
    setRefreshingAgentCommandSessionIds(nextRefreshingSessionIds);
    setAgentCommandErrors((current) => {
      if (!current[sessionId]) {
        return current;
      }

      const nextState = { ...current };
      delete nextState[sessionId];
      return nextState;
    });

    try {
      const response = await fetchAgentCommands(sessionId);
      if (!isMountedRef.current) {
        return;
      }
      setAgentCommandsBySessionId((current) => ({
        ...current,
        [sessionId]: response.commands,
      }));
    } catch (error) {
      if (!isMountedRef.current) {
        return;
      }
      const message = getErrorMessage(error);
      setAgentCommandErrors((current) => ({
        ...current,
        [sessionId]: message,
      }));
    } finally {
      if (isMountedRef.current) {
        const nextRefreshingSessionIds = setSessionFlag(
          refreshingAgentCommandSessionIdsRef.current,
          sessionId,
          false,
        );
        refreshingAgentCommandSessionIdsRef.current = nextRefreshingSessionIds;
        setRefreshingAgentCommandSessionIds(nextRefreshingSessionIds);
      }
    }
  }

  async function handleCreateConversationMarker(
    sessionId: string,
    messageId: string,
    options: CreateConversationMarkerOptions = {},
  ) {
    const session = sessionLookup.get(sessionId);
    if (!session || !session.messages.some((message) => message.id === messageId)) {
      return false;
    }

    setUpdatingSessionIds((current) =>
      setSessionFlag(current, sessionId, true),
    );
    try {
      const response = await createConversationMarker(
        sessionId,
        buildCreateConversationMarkerRequest(messageId, options),
      );
      if (!isMountedRef.current) {
        return false;
      }

      const responseOutcome = shouldApplyMarkerMutationResponse(sessionId, response);
      if (responseOutcome === "deferred") {
        return false;
      }
      if (responseOutcome === "apply") {
        updateSessionLocally(sessionId, (currentSession) =>
          upsertConversationMarkerLocally(
            currentSession,
            response.marker,
            response.sessionMutationStamp,
          ),
        );
      }
      setRequestError(null);
      return true;
    } catch (error) {
      if (!isMountedRef.current) {
        return false;
      }
      reportRequestError(error);
      return false;
    } finally {
      if (isMountedRef.current) {
        setUpdatingSessionIds((current) =>
          setSessionFlag(current, sessionId, false),
        );
      }
    }
  }

  async function handleUpdateConversationMarker(
    sessionId: string,
    markerId: string,
    payload: UpdateConversationMarkerRequest,
  ) {
    const session = sessionLookup.get(sessionId);
    if (!session?.markers?.some((marker) => marker.id === markerId)) {
      return false;
    }

    setUpdatingSessionIds((current) =>
      setSessionFlag(current, sessionId, true),
    );
    try {
      const response = await updateConversationMarker(sessionId, markerId, payload);
      if (!isMountedRef.current) {
        return false;
      }

      const responseOutcome = shouldApplyMarkerMutationResponse(sessionId, response);
      if (responseOutcome === "deferred") {
        return false;
      }
      if (responseOutcome === "apply") {
        updateSessionLocally(sessionId, (currentSession) =>
          upsertConversationMarkerLocally(
            currentSession,
            response.marker,
            response.sessionMutationStamp,
          ),
        );
      }
      setRequestError(null);
      return true;
    } catch (error) {
      if (!isMountedRef.current) {
        return false;
      }
      reportRequestError(error);
      return false;
    } finally {
      if (isMountedRef.current) {
        setUpdatingSessionIds((current) =>
          setSessionFlag(current, sessionId, false),
        );
      }
    }
  }

  async function handleDeleteConversationMarker(
    sessionId: string,
    markerId: string,
  ) {
    const session = sessionLookup.get(sessionId);
    if (!session?.markers?.some((marker) => marker.id === markerId)) {
      return false;
    }

    setUpdatingSessionIds((current) =>
      setSessionFlag(current, sessionId, true),
    );
    try {
      const response = await deleteConversationMarker(sessionId, markerId);
      if (!isMountedRef.current) {
        return false;
      }

      const responseOutcome = shouldApplyMarkerMutationResponse(
        sessionId,
        {
          revision: response.revision,
          serverInstanceId: response.serverInstanceId,
          markerId: response.markerId,
          sessionMutationStamp: response.sessionMutationStamp,
        },
        { deleted: true },
      );
      if (responseOutcome === "deferred") {
        return false;
      }
      if (responseOutcome === "apply") {
        updateSessionLocally(sessionId, (currentSession) =>
          deleteConversationMarkerLocally(
            currentSession,
            response.markerId,
            response.sessionMutationStamp,
          ),
        );
      }
      setRequestError(null);
      return true;
    } catch (error) {
      if (!isMountedRef.current) {
        return false;
      }
      reportRequestError(error);
      return false;
    } finally {
      if (isMountedRef.current) {
        setUpdatingSessionIds((current) =>
          setSessionFlag(current, sessionId, false),
        );
      }
    }
  }

  return {
    handleSend,
    handleDraftAttachmentsAdd,
    handleDraftAttachmentRemove,
    handleNewSession,
    handleCloneSessionFromExisting,
    handleCreateProject,
    handlePickProjectRoot,
    handleApprovalDecision,
    handleUserInputSubmit,
    handleMcpElicitationSubmit,
    handleCodexAppRequestSubmit,
    handleCancelQueuedPrompt,
    handleStopSession,
    executeKillSession,
    handleRenameSession,
    handleSessionSettingsChange,
    handleRefreshSessionModelOptions,
    handleForkCodexThread,
    handleArchiveCodexThread,
    handleUnarchiveCodexThread,
    handleCompactCodexThread,
    handleRollbackCodexThread,
    handleRefreshAgentCommands,
    handleCreateConversationMarker,
    handleUpdateConversationMarker,
    handleDeleteConversationMarker,
  };
}
