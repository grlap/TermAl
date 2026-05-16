// Owns: public and internal type surface for useAppSessionActions.
// Does not own: action implementation, API calls, or optimistic state mutation helpers.
// Split from: ui/src/app-session-actions.ts.

import type { Dispatch, MutableRefObject, SetStateAction } from "react";

import type { UpdateConversationMarkerRequest } from "./api";
import type { UseAppLiveStateReturn } from "./app-live-state";
import type { SessionErrorMap, SessionNoticeMap } from "./app-shell-internals";
import type {
  DraftImageAttachment,
  SessionAgentCommandMap,
  SessionFlagMap,
} from "./app-utils";
import type { WorkspaceState } from "./workspace";
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

export type UseAppSessionActionsLookups = {
  sessionLookup: Map<string, Session>;
  projectLookup: Map<string, Project>;
  agentReadinessByAgent: Map<AgentType, AgentReadiness>;
  activeSession: Session | null;
  workspace: WorkspaceState;
};

export type UseAppSessionActionsDefaults = {
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

export type UseAppSessionActionsRefs = {
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

export type UseAppSessionActionsSetters = {
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

export type UseAppSessionActionsParams = {
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

export type HandleNewSessionArgs = {
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
