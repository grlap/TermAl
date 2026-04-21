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

import type {
  Dispatch,
  MutableRefObject,
  SetStateAction,
} from "react";
import {
  archiveCodexThread,
  cancelQueuedPrompt,
  compactCodexThread,
  createProject,
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
  updateSessionSettings,
  type StateResponse,
} from "./api";
import type { UseAppLiveStateReturn } from "./app-live-state";
import {
  startActivePromptPoll,
} from "./active-prompt-poll";
import {
  getErrorMessage,
  releaseDraftAttachments,
  removeQueuedPromptFromSessions,
  setSessionFlag,
  type DraftImageAttachment,
  type SessionAgentCommandMap,
  type SessionFlagMap,
} from "./app-utils";
import type {
  SessionErrorMap,
  SessionNoticeMap,
} from "./app-shell-internals";
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
  findWorkspacePaneIdForSession,
  openSessionInWorkspaceState,
  reconcileWorkspaceState,
  type WorkspaceState,
} from "./workspace";
import type {
  AgentReadiness,
  AgentType,
  ApprovalDecision,
  ApprovalPolicy,
  ClaudeApprovalMode,
  ClaudeEffortLevel,
  CodexReasoningEffort,
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
import { resolveProjectRemoteId, isLocalRemoteId, LOCAL_REMOTE_ID } from "./remotes";

type UseAppSessionActionsLookups = {
  sessionLookup: Map<string, Session>;
  projectLookup: Map<string, Project>;
  agentReadinessByAgent: Map<AgentType, AgentReadiness>;
  activeSession: Session | null;
  workspace: WorkspaceState;
};

type UseAppSessionActionsDefaults = {
  defaultCodexApprovalPolicy: ApprovalPolicy;
  defaultCodexReasoningEffort: CodexReasoningEffort;
  defaultCodexSandboxMode: SandboxMode;
  defaultClaudeApprovalMode: ClaudeApprovalMode;
  defaultClaudeEffort: ClaudeEffortLevel;
  defaultCursorMode: CursorMode;
  defaultGeminiApprovalMode: GeminiApprovalMode;
};

type UseAppSessionActionsRefs = {
  isMountedRef: MutableRefObject<boolean>;
  sessionsRef: MutableRefObject<Session[]>;
  confirmedUnknownModelSendsRef: MutableRefObject<Set<string>>;
  activePromptPollCancelRef: MutableRefObject<(() => void) | null>;
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
  draftsBySessionId: Record<string, string>;
  draftAttachmentsBySessionId: Record<string, DraftImageAttachment[]>;
  newProjectRootPath: string;
  newProjectRemoteId: string;
  newProjectUsesLocalRemote: boolean;
  defaults: UseAppSessionActionsDefaults;
  refs: UseAppSessionActionsRefs;
  setters: UseAppSessionActionsSetters;
  adoptState: UseAppLiveStateReturn["adoptState"];
  adoptCreatedSessionResponse: UseAppLiveStateReturn["adoptCreatedSessionResponse"];
  applyControlPanelLayout: (
    nextWorkspace: WorkspaceState,
    side?: "left" | "right",
  ) => WorkspaceState;
  reportRequestError: (
    error: unknown,
    options?: { message?: string },
  ) => void;
};

type HandleNewSessionArgs = {
  agent: AgentType;
  model: string;
  preferredPaneId?: string | null;
  projectSelectionId?: string;
};

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
  handleRenameSession: (sessionId: string, nextName: string) => Promise<boolean>;
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
    draftsBySessionId,
    draftAttachmentsBySessionId,
    newProjectRootPath,
    newProjectRemoteId,
    newProjectUsesLocalRemote,
    defaults: {
      defaultCodexApprovalPolicy,
      defaultCodexReasoningEffort,
      defaultCodexSandboxMode,
      defaultClaudeApprovalMode,
      defaultClaudeEffort,
      defaultCursorMode,
      defaultGeminiApprovalMode,
    },
    refs: {
      isMountedRef,
      sessionsRef,
      confirmedUnknownModelSendsRef,
      activePromptPollCancelRef,
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
    applyControlPanelLayout,
    reportRequestError,
  } = params;

  function startActivePromptRecoveryPoll(sessionId: string) {
    activePromptPollCancelRef.current?.();
    activePromptPollCancelRef.current = startActivePromptPoll({
      fetchState,
      isMounted: () => isMountedRef.current,
      onState: (freshState) => {
        adoptState(freshState);
        return !freshState.sessions?.some(
          (session) => session.id === sessionId && session.status === "active",
        );
      },
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
    setSessions(nextSessions);
    setWorkspace((current) =>
      applyControlPanelLayout(reconcileWorkspaceState(current, nextSessions)),
    );
  }

  function buildOptimisticSessionSettingsUpdate(
    session: Session,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) {
    const normalizedModelValue =
      field === "model"
        ? normalizedRequestedSessionModel(session, value as string)
        : null;

    switch (session.agent) {
      case "Codex": {
        const nextModel = normalizedModelValue ?? session.model;
        const nextReasoningEffort =
          field === "reasoningEffort"
            ? (value as CodexReasoningEffort)
            : normalizedCodexReasoningEffort(session, nextModel);
        const nextSandboxMode =
          field === "sandboxMode"
            ? (value as SandboxMode)
            : session.sandboxMode;
        const nextApprovalPolicy =
          field === "approvalPolicy"
            ? (value as ApprovalPolicy)
            : session.approvalPolicy;

        if (
          nextModel === session.model &&
          nextReasoningEffort === session.reasoningEffort &&
          nextSandboxMode === session.sandboxMode &&
          nextApprovalPolicy === session.approvalPolicy
        ) {
          return session;
        }

        return {
          ...session,
          model: nextModel,
          reasoningEffort: nextReasoningEffort,
          sandboxMode: nextSandboxMode,
          approvalPolicy: nextApprovalPolicy,
        };
      }
      case "Cursor": {
        const nextModel = normalizedModelValue ?? session.model;
        const nextCursorMode =
          field === "cursorMode" ? (value as CursorMode) : session.cursorMode;

        if (
          nextModel === session.model &&
          nextCursorMode === session.cursorMode
        ) {
          return session;
        }

        return {
          ...session,
          model: nextModel,
          cursorMode: nextCursorMode,
        };
      }
      case "Claude": {
        const nextModel = normalizedModelValue ?? session.model;
        const nextClaudeApprovalMode =
          field === "claudeApprovalMode"
            ? (value as ClaudeApprovalMode)
            : session.claudeApprovalMode;
        const nextClaudeEffort =
          field === "claudeEffort"
            ? (value as ClaudeEffortLevel)
            : session.claudeEffort;

        if (
          nextModel === session.model &&
          nextClaudeApprovalMode === session.claudeApprovalMode &&
          nextClaudeEffort === session.claudeEffort
        ) {
          return session;
        }

        return {
          ...session,
          model: nextModel,
          claudeApprovalMode: nextClaudeApprovalMode,
          claudeEffort: nextClaudeEffort,
        };
      }
      case "Gemini": {
        const nextModel = normalizedModelValue ?? session.model;
        const nextGeminiApprovalMode =
          field === "geminiApprovalMode"
            ? (value as GeminiApprovalMode)
            : session.geminiApprovalMode;

        if (
          nextModel === session.model &&
          nextGeminiApprovalMode === session.geminiApprovalMode
        ) {
          return session;
        }

        return {
          ...session,
          model: nextModel,
          geminiApprovalMode: nextGeminiApprovalMode,
        };
      }
    }
  }

  function rollbackOptimisticSessionSettingsUpdate(
    currentSession: Session,
    previousSession: Session,
    optimisticSession: Session,
  ) {
    let changed = false;
    const nextSession = { ...currentSession };

    if (
      currentSession.model === optimisticSession.model &&
      currentSession.model !== previousSession.model
    ) {
      nextSession.model = previousSession.model;
      changed = true;
    }
    if (
      currentSession.approvalPolicy === optimisticSession.approvalPolicy &&
      currentSession.approvalPolicy !== previousSession.approvalPolicy
    ) {
      nextSession.approvalPolicy = previousSession.approvalPolicy;
      changed = true;
    }
    if (
      currentSession.reasoningEffort === optimisticSession.reasoningEffort &&
      currentSession.reasoningEffort !== previousSession.reasoningEffort
    ) {
      nextSession.reasoningEffort = previousSession.reasoningEffort;
      changed = true;
    }
    if (
      currentSession.sandboxMode === optimisticSession.sandboxMode &&
      currentSession.sandboxMode !== previousSession.sandboxMode
    ) {
      nextSession.sandboxMode = previousSession.sandboxMode;
      changed = true;
    }
    if (
      currentSession.cursorMode === optimisticSession.cursorMode &&
      currentSession.cursorMode !== previousSession.cursorMode
    ) {
      nextSession.cursorMode = previousSession.cursorMode;
      changed = true;
    }
    if (
      currentSession.claudeApprovalMode ===
        optimisticSession.claudeApprovalMode &&
      currentSession.claudeApprovalMode !== previousSession.claudeApprovalMode
    ) {
      nextSession.claudeApprovalMode = previousSession.claudeApprovalMode;
      changed = true;
    }
    if (
      currentSession.claudeEffort === optimisticSession.claudeEffort &&
      currentSession.claudeEffort !== previousSession.claudeEffort
    ) {
      nextSession.claudeEffort = previousSession.claudeEffort;
      changed = true;
    }
    if (
      currentSession.geminiApprovalMode ===
        optimisticSession.geminiApprovalMode &&
      currentSession.geminiApprovalMode !== previousSession.geminiApprovalMode
    ) {
      nextSession.geminiApprovalMode = previousSession.geminiApprovalMode;
      changed = true;
    }

    return changed ? nextSession : currentSession;
  }

  function sessionSupportsModelRefresh(agent: AgentType) {
    return (
      agent === "Claude" ||
      agent === "Codex" ||
      agent === "Cursor" ||
      agent === "Gemini"
    );
  }

  async function openCreatedSession(
    created: Awaited<ReturnType<typeof createSession>>,
    paneId: string | null,
    agent: AgentType,
  ) {
    const adopted = adoptCreatedSessionResponse(created, {
      openSessionId: created.sessionId,
      paneId,
    });
    if (adopted === "stale") {
      setWorkspace((current) =>
        applyControlPanelLayout(
          openSessionInWorkspaceState(current, created.sessionId, paneId),
        ),
      );
    }
    if (sessionSupportsModelRefresh(agent)) {
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

    const draftText = draftTextOverride ?? draftsBySessionId[sessionId] ?? "";
    const prompt = draftText.trim();
    const expandedText = expandedTextOverride?.trim() || null;
    const normalizedExpandedText =
      expandedText && expandedText !== prompt ? expandedText : null;
    const attachments = draftAttachmentsBySessionId[sessionId] ?? [];
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
        const adopted = adoptState(state);
        releaseDraftAttachments(attachments);
        setRequestError(null);
        startActivePromptRecoveryPoll(sessionId);
        if (!adopted) {
          return;
        }
      } catch (error) {
        let restoredDraft = false;
        let restoredAttachments = false;

        setDraftsBySessionId((current) => {
          if (!draftText || (current[sessionId] ?? "") !== "") {
            return current;
          }

          restoredDraft = true;
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

          restoredAttachments = true;
          return {
            ...current,
            [sessionId]: attachments,
          };
        });
        if (!restoredAttachments) {
          releaseDraftAttachments(attachments);
        }
        reportRequestError(error);
      } finally {
        setSendingSessionIds((current) =>
          setSessionFlag(current, sessionId, false),
        );
      }
    })();

    return true;
  }

  function handleDraftAttachmentsAdd(
    sessionId: string,
    attachments: DraftImageAttachment[],
  ) {
    setDraftAttachmentsBySessionId((current) => ({
      ...current,
      [sessionId]: [...(current[sessionId] ?? []), ...attachments],
    }));
  }

  function handleDraftAttachmentRemove(
    sessionId: string,
    attachmentId: string,
  ) {
    setDraftAttachmentsBySessionId((current) => {
      const existing = current[sessionId];
      if (!existing) {
        return current;
      }

      const removed = existing.filter(
        (attachment) => attachment.id === attachmentId,
      );
      if (removed.length === 0) {
        return current;
      }

      releaseDraftAttachments(removed);
      const nextAttachments = existing.filter(
        (attachment) => attachment.id !== attachmentId,
      );
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
      const created = await createSession({
        agent,
        model: usesSessionModelPicker(agent) ? undefined : trimmedModel,
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
      adoptState(created.state);
      setSelectedProjectId(created.projectId);
      setNewProjectRootPath("");
      setNewProjectRemoteId(LOCAL_REMOTE_ID);
      setRequestError(null);
      return true;
    } catch (error) {
      reportRequestError(error);
      return false;
    } finally {
      setIsCreatingProject(false);
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
      if (response.path) {
        setNewProjectRootPath(response.path);
        setRequestError(null);
      }
    } catch (error) {
      reportRequestError(error);
    } finally {
      setIsCreatingProject(false);
    }
  }

  async function handleApprovalDecision(
    sessionId: string,
    messageId: string,
    decision: ApprovalDecision,
  ) {
    try {
      const state = await submitApproval(sessionId, messageId, decision);
      adoptState(state);
      setRequestError(null);
    } catch (error) {
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
      adoptState(state);
      setRequestError(null);
    } catch (error) {
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
      adoptState(state);
      setRequestError(null);
    } catch (error) {
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
      adoptState(state);
      setRequestError(null);
    } catch (error) {
      reportRequestError(error);
    }
  }

  async function handleCancelQueuedPrompt(sessionId: string, promptId: string) {
    setSessions((current) => {
      const next = removeQueuedPromptFromSessions(current, sessionId, promptId);
      sessionsRef.current = next;
      return next;
    });
    try {
      const state = await cancelQueuedPrompt(sessionId, promptId);
      adoptState(state);
      setRequestError(null);
    } catch (error) {
      try {
        const state = await fetchState();
        adoptState(state);
      } catch {
        // Keep the original request error below; state refresh is best-effort.
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
      adoptState(state);
      setRequestError(null);
    } catch (error) {
      reportRequestError(error);
    } finally {
      setStoppingSessionIds((current) =>
        setSessionFlag(current, sessionId, false),
      );
    }
  }

  async function executeKillSession(sessionId: string) {
    setKillingSessionIds((current) => setSessionFlag(current, sessionId, true));
    try {
      const state = await killSession(sessionId);
      if (!isMountedRef.current) {
        return;
      }

      adoptState(state);
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

      adoptState(state);
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

    setRequestError(null);
    if (hasOptimisticUpdate) {
      updateSessionLocally(sessionId, () => optimisticSession);
    }
    setUpdatingSessionIds((current) =>
      setSessionFlag(current, sessionId, true),
    );
    try {
      const state = await updateSessionSettings(sessionId, payload);
      adoptState(state);
      const updatedSession =
        state.sessions.find((entry) => entry.id === sessionId) ?? null;
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
      setUpdatingSessionIds((current) =>
        setSessionFlag(current, sessionId, false),
      );
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
      adoptState(state);
      if (previousSession?.agent === "Codex") {
        const refreshedSession =
          state.sessions.find((entry) => entry.id === sessionId) ?? null;
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
      adoptState(state);
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
      if (adopted === "stale") {
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
      setSessionSettingNotices((current) => ({
        ...current,
        [sessionId]: "Forked the live Codex thread into a new session.",
        [created.sessionId]:
          "This session is attached to a forked Codex thread. Earlier Codex history was restored from Codex where available.",
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
      setAgentCommandsBySessionId((current) => ({
        ...current,
        [sessionId]: response.commands,
      }));
    } catch (error) {
      const message = getErrorMessage(error);
      setAgentCommandErrors((current) => ({
        ...current,
        [sessionId]: message,
      }));
    } finally {
      const nextRefreshingSessionIds = setSessionFlag(
        refreshingAgentCommandSessionIdsRef.current,
        sessionId,
        false,
      );
      refreshingAgentCommandSessionIdsRef.current = nextRefreshingSessionIds;
      setRefreshingAgentCommandSessionIds(nextRefreshingSessionIds);
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
  };
}
