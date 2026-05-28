import type {
  ApprovalDecision,
  AgentType,
  AgentReadiness,
  AgentCommand,
  AgentCommandKind,
  ApprovalPolicy,
  AppPreferences,
  ClaudeApprovalMode,
  ClaudeEffortLevel,
  CodexReasoningEffort,
  CodexState,
  ConversationMarker,
  ConversationMarkerKind,
  CursorMode,
  DelegationRecord,
  DelegationResult,
  DelegationSummary,
  GeminiApprovalMode,
  ImageAttachment,
  InstructionSearchResponse,
  JsonValue,
  McpElicitationAction,
  OrchestratorInstance,
  OrchestratorTemplate,
  OrchestratorTemplateDraft,
  Project,
  RemoteConfig,
  SandboxMode,
  Session,
} from "./types";
import {
  createBackendUnavailableError,
  createResponseError,
  formatUnavailableApiMessage,
  looksLikeHtmlResponse,
  performRequest,
  request,
  requestJsonFirst,
} from "./api-request";

export {
  ApiRequestError,
  isBackendUnavailableError,
  type ApiRequestErrorKind,
} from "./api-request";
export {
  runTerminalCommand,
  runTerminalCommandStream,
  TERMINAL_SSE_BUFFER_MAX_CHARS,
  type TerminalCommandOutputEvent,
  type TerminalCommandResponse,
  type TerminalOutputStream,
} from "./api-terminal";

export type StateResponse = {
  revision: number;
  /**
   * Per-process UUID generated at `AppState::new_with_paths` boot on
   * the server. Stable for the lifetime of the server process; changes
   * on every restart. Carried on every snapshot so the client can
   * distinguish "revision decreased because the server just restarted"
   * from "revision decreased because this response is stale". Empty
   * string means "unknown" (older server or fallback payload) — treat
   * as NOT a restart signal. See `shouldAdoptSnapshotRevision`.
   */
  serverInstanceId: string;
  codex: CodexState;
  agentReadiness: AgentReadiness[];
  preferences: AppPreferences;
  projects: Project[];
  orchestrators: OrchestratorInstance[];
  workspaces: WorkspaceLayoutSummary[];
  sessions: Session[];
  delegations?: DelegationSummary[];
  delegationWaits?: DelegationWaitRecord[];
};

export type CreateSessionResponse = {
  sessionId: string;
  session: Session;
  revision: number;
  /**
   * See `StateResponse.serverInstanceId`. Carried on create/fork
   * responses so the frontend's restart-detection can accept a
   * revision downgrade when the user hits Send against a freshly
   * restarted server (the common prompt-invisible case).
   */
  serverInstanceId: string;
};

export type SessionResponse = {
  revision: number;
  session: Session;
  /**
   * See `StateResponse.serverInstanceId`. Carried so
   * `adoptFetchedSession` can detect a server restart mid-hydration
   * and accept a revision downgrade — otherwise a session click at
   * the exact moment of a restart would be silently rejected by the
   * monotonic revision guard until safety-net pollers re-fetch.
   */
  serverInstanceId: string;
};

export type CreateProjectResponse = {
  projectId: string;
  state: StateResponse;
};

export type DelegationResponse = {
  revision: number;
  delegation: DelegationRecord;
  childSession: Session;
  serverInstanceId: string;
};

export type DelegationStatusResponse = {
  revision: number;
  delegation: DelegationRecord;
  serverInstanceId: string;
};

export type DelegationResultResponse = {
  revision: number;
  result: DelegationResult;
  serverInstanceId: string;
};

export type ConversationMarkersResponse = {
  markers: ConversationMarker[];
  revision: number;
  serverInstanceId: string;
};

export type ConversationMarkerResponse = {
  marker: ConversationMarker;
  revision: number;
  serverInstanceId: string;
  sessionMutationStamp?: number | null;
};

export type DeleteConversationMarkerResponse = {
  markerId: string;
  revision: number;
  serverInstanceId: string;
  sessionMutationStamp?: number | null;
};

export type CreateConversationMarkerRequest = {
  kind: ConversationMarkerKind;
  name: string;
  body?: string | null;
  color: string;
  messageId: string;
  endMessageId?: string | null;
};

export type UpdateConversationMarkerRequest = {
  kind?: ConversationMarkerKind;
  name?: string;
  body?: string | null;
  color?: string;
  messageId?: string;
  endMessageId?: string | null;
};

export type WorkspaceLayoutDocument = {
  id: string;
  revision: number;
  updatedAt: string;
  controlPanelSide: "left" | "right";
  themeId?: string;
  styleId?: string;
  markdownThemeId?: string;
  markdownStyleId?: string;
  diagramThemeOverrideMode?: "on" | "off";
  diagramLook?: "classic" | "handDrawn" | "neo";
  diagramPalette?: "match" | "default" | "dark" | "forest" | "neutral" | "base";
  fontSizePx?: number;
  editorFontSizePx?: number;
  densityPercent?: number;
  workspace: unknown;
};

export type WorkspaceLayoutResponse = {
  layout: WorkspaceLayoutDocument;
};

export type WorkspaceLayoutSummary = {
  id: string;
  revision: number;
  updatedAt: string;
  controlPanelSide: "left" | "right";
  themeId?: string;
  styleId?: string;
  fontSizePx?: number;
  editorFontSizePx?: number;
  densityPercent?: number;
};

export type WorkspaceLayoutsResponse = {
  workspaces: WorkspaceLayoutSummary[];
};

export type OrchestratorTemplatesResponse = {
  templates: OrchestratorTemplate[];
};

export type OrchestratorTemplateResponse = {
  template: OrchestratorTemplate;
};

export type OrchestratorInstancesResponse = {
  orchestrators: OrchestratorInstance[];
};

export type OrchestratorInstanceResponse = {
  orchestrator: OrchestratorInstance;
};

export type CreateOrchestratorInstanceResponse = {
  orchestrator: OrchestratorInstance;
  state: StateResponse;
};
export type PickProjectRootResponse = {
  path?: string | null;
};

export type FileResponse = {
  path: string;
  content: string;
  contentHash?: string | null;
  mtimeMs?: number | null;
  sizeBytes?: number | null;
  language?: string | null;
};

export type DirectoryEntry = {
  kind: "directory" | "file";
  name: string;
  path: string;
};

export type DirectoryResponse = {
  entries: DirectoryEntry[];
  name: string;
  path: string;
};

export type GitStatusFile = {
  indexStatus?: string | null;
  originalPath?: string | null;
  path: string;
  worktreeStatus?: string | null;
};

export type GitStatusResponse = {
  ahead: number;
  behind: number;
  branch?: string | null;
  files: GitStatusFile[];
  isClean: boolean;
  repoRoot?: string | null;
  upstream?: string | null;
  workdir: string;
};

export type GitFileAction = "revert" | "stage" | "unstage";
export type GitDiffSection = "staged" | "unstaged";

export type GitDiffDocumentSideSource = "head" | "index" | "worktree" | "empty";

export type GitDiffDocumentSide = {
  content: string;
  source: GitDiffDocumentSideSource;
};

export type GitDiffDocumentContent = {
  before: GitDiffDocumentSide;
  after: GitDiffDocumentSide;
  canEdit: boolean;
  editBlockedReason?: string | null;
  isCompleteDocument: boolean;
  note?: string | null;
};

export type GitDiffResponse = {
  changeType: "edit" | "create";
  changeSetId?: string | null;
  diff: string;
  diffId: string;
  documentEnrichmentNote?: string | null;
  documentContent?: GitDiffDocumentContent | null;
  filePath?: string | null;
  language?: string | null;
  summary: string;
};

export type GitCommitResponse = {
  status: GitStatusResponse;
  summary: string;
};

export type GitRepoActionResponse = {
  status: GitStatusResponse;
  summary: string;
};

export type OpenPathOptions = {
  line?: number;
  column?: number;
  openInNewTab?: boolean;
};

export type ReviewCommentAuthor = "user" | "agent";
export type ReviewThreadStatus = "open" | "resolved" | "applied" | "dismissed";

export type ReviewAnchor =
  | { kind: "changeSet" }
  | { kind: "file"; filePath: string }
  | { kind: "hunk"; filePath: string; hunkHeader: string }
  | {
      kind: "line";
      filePath: string;
      hunkHeader: string;
      oldLine?: number | null;
      newLine?: number | null;
    };

export type ReviewOrigin = {
  sessionId: string;
  messageId: string;
  agent: string;
  workdir: string;
  createdAt: string;
};

export type ReviewFileEntry = {
  filePath: string;
  changeType: "edit" | "create";
};

export type ReviewComment = {
  id: string;
  author: ReviewCommentAuthor;
  body: string;
  createdAt: string;
  updatedAt: string;
};

export type ReviewThread = {
  id: string;
  anchor: ReviewAnchor;
  status: ReviewThreadStatus;
  comments: ReviewComment[];
};

export type ReviewDocument = {
  version: number;
  revision: number;
  changeSetId: string;
  origin?: ReviewOrigin | null;
  files?: ReviewFileEntry[];
  threads?: ReviewThread[];
};

export type ReviewDocumentResponse = {
  reviewFilePath: string;
  review: ReviewDocument;
};

export type ReviewSummaryResponse = {
  changeSetId: string;
  reviewFilePath: string;
  threadCount: number;
  openThreadCount: number;
  resolvedThreadCount: number;
  commentCount: number;
  hasThreads: boolean;
};

type RequestScope = {
  projectId?: string | null;
  sessionId?: string | null;
};

type CreateSessionRequest = {
  agent?: AgentType;
  model?: string;
  name?: string;
  workdir?: string;
  projectId?: string;
  approvalPolicy?: ApprovalPolicy;
  claudeEffort?: ClaudeEffortLevel;
  reasoningEffort?: CodexReasoningEffort;
  sandboxMode?: SandboxMode;
  cursorMode?: CursorMode;
  claudeApprovalMode?: ClaudeApprovalMode;
  geminiApprovalMode?: GeminiApprovalMode;
};

export type CreateDelegationRequest = {
  prompt: string;
  /** Defaults to a backend-generated title when omitted. */
  title?: string;
  /** Defaults to the parent session working directory when omitted. */
  cwd?: string;
  /** Defaults to the parent session agent when omitted. */
  agent?: AgentType;
  /** Defaults to the selected agent's configured app default model when omitted or blank. */
  model?: string;
  /** Defaults to reviewer mode when omitted. */
  mode?: "reviewer" | "explorer" | "worker";
  /** Defaults to read-only delegation when omitted. */
  writePolicy?:
    | { kind: "readOnly" }
    | { kind: "sharedWorktree"; ownedPaths: string[] }
    | {
        kind: "isolatedWorktree";
        ownedPaths: string[];
        /** Backend generates a TermAl-owned worktree path when omitted. */
        worktreePath?: string;
      };
};

export type AbortableRequestOptions = {
  signal?: AbortSignal;
};

type CreateProjectRequest = {
  name?: string;
  rootPath: string;
  remoteId?: string;
};

export type AgentCommandsResponse = {
  commands: AgentCommand[];
};

export type ResolveAgentCommandIntent = "send" | "delegate";

export type ResolveAgentCommandRequest = {
  arguments?: string;
  note?: string;
  /** Delegation-only working directory override; backend rejects this for send intent. */
  cwd?: string;
  intent?: ResolveAgentCommandIntent;
};

export type ResolveAgentCommandResponse = {
  name: string;
  source: string;
  kind: AgentCommandKind;
  visiblePrompt: string;
  expandedPrompt?: string | null;
  title?: string | null;
  delegation?: {
    mode?: CreateDelegationRequest["mode"];
    title?: string | null;
    writePolicy?: CreateDelegationRequest["writePolicy"] | null;
  } | null;
};

export type DelegationWaitMode = "any" | "all";

export type CreateDelegationWaitRequest = {
  delegationIds: string[];
  mode?: DelegationWaitMode;
  title?: string;
};

export type DelegationWaitRecord = {
  id: string;
  parentSessionId: string;
  delegationIds: string[];
  mode: DelegationWaitMode;
  createdAt: string;
  title?: string | null;
};

export type DelegationWaitResponse = {
  revision: number;
  wait: DelegationWaitRecord;
  resumePromptQueued: boolean;
  resumeDispatchRequested: boolean;
  serverInstanceId: string;
};

export function fetchState() {
  return requestJsonFirst<StateResponse>("/api/state");
}

export function fetchSession(sessionId: string) {
  return requestJsonFirst<SessionResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}`,
  );
}

export function fetchSessionTail(sessionId: string, messageLimit: number) {
  const query = new URLSearchParams({
    tail: String(Math.max(1, Math.floor(messageLimit))),
  });
  return requestJsonFirst<SessionResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}?${query.toString()}`,
  );
}

export async function fetchWorkspaceLayout(workspaceId: string) {
  const endpoint = `/api/workspaces/${encodeURIComponent(workspaceId)}`;
  const response = await performRequest(endpoint);

  const contentType = response.headers.get("content-type") ?? "";
  const raw = await response.text();
  if (looksLikeHtmlResponse(raw, contentType)) {
    throw createBackendUnavailableError(
      formatUnavailableApiMessage(endpoint, response.status),
      response.status,
      { restartRequired: true },
    );
  }

  if (response.status === 404) {
    return null;
  }

  if (!response.ok) {
    throw createResponseError(raw, response.status);
  }

  return raw ? (JSON.parse(raw) as WorkspaceLayoutResponse) : null;
}

export function fetchWorkspaceLayouts() {
  return request<WorkspaceLayoutsResponse>("/api/workspaces");
}

/**
 * Saves one workspace layout and returns the saved document.
 *
 * DELETE on the same route intentionally returns the remaining summary list
 * instead, because save and delete callers need different data.
 */
export function saveWorkspaceLayout(
  workspaceId: string,
  payload: {
    controlPanelSide: "left" | "right";
    themeId?: string;
    styleId?: string;
    markdownThemeId?: string;
    markdownStyleId?: string;
    diagramThemeOverrideMode?: "on" | "off";
    diagramLook?: "classic" | "handDrawn" | "neo";
    diagramPalette?: "match" | "default" | "dark" | "forest" | "neutral" | "base";
    fontSizePx?: number;
    editorFontSizePx?: number;
    densityPercent?: number;
    workspace: unknown;
  },
  options?: {
    keepalive?: boolean;
  },
) {
  return request<WorkspaceLayoutResponse>(
    `/api/workspaces/${encodeURIComponent(workspaceId)}`,
    {
      method: "PUT",
      body: JSON.stringify(payload),
      keepalive: options?.keepalive,
    },
  );
}

/**
 * Deletes one workspace layout and returns the remaining summary list.
 *
 * PUT on the same route intentionally returns the saved document instead.
 */
export function deleteWorkspaceLayout(workspaceId: string) {
  return request<WorkspaceLayoutsResponse>(
    `/api/workspaces/${encodeURIComponent(workspaceId)}`,
    {
      method: "DELETE",
    },
  );
}

export function updateAppSettings(payload: {
  defaultCodexModel?: string;
  defaultClaudeModel?: string;
  defaultCursorModel?: string;
  defaultGeminiModel?: string;
  defaultCodexReasoningEffort?: CodexReasoningEffort;
  defaultClaudeApprovalMode?: ClaudeApprovalMode;
  defaultClaudeEffort?: ClaudeEffortLevel;
  remotes?: RemoteConfig[];
}) {
  return request<StateResponse>("/api/settings", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export type TelegramStatusResponse = {
  configured: boolean;
  enabled: boolean;
  forwardAssistantReplies: boolean;
  running: boolean;
  // Always "inProcess" today; retained as the multi-bot status discriminator
  // described in docs/features/telegram-ui-integration.md.
  lifecycle: "inProcess";
  linkedChatId?: number | null;
  botTokenMasked?: string | null;
  subscribedProjectIds: string[];
  defaultProjectId?: string | null;
  defaultSessionId?: string | null;
};

export type UpdateTelegramConfigPayload = {
  enabled?: boolean;
  forwardAssistantReplies?: boolean;
  botToken?: string | null;
  subscribedProjectIds?: string[];
  defaultProjectId?: string | null;
  defaultSessionId?: string | null;
};

export type TelegramTestResponse = {
  botName: string;
  botUsername?: string | null;
};

export function fetchTelegramStatus() {
  return request<TelegramStatusResponse>("/api/telegram/status");
}

export function updateTelegramConfig(payload: UpdateTelegramConfigPayload) {
  return request<TelegramStatusResponse>("/api/telegram/config", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export function testTelegramConnection(payload: {
  botToken?: string | null;
  useSavedToken?: boolean;
}) {
  return request<TelegramTestResponse>(
    "/api/telegram/test",
    {
      method: "POST",
      body: JSON.stringify(payload),
    },
    {
      preserveGatewayErrorBody: true,
    },
  );
}

export function createSession(payload: CreateSessionRequest) {
  return request<CreateSessionResponse>("/api/sessions", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export function createDelegation(
  parentSessionId: string,
  payload: CreateDelegationRequest,
) {
  return request<DelegationResponse>(
    `/api/sessions/${encodeURIComponent(parentSessionId)}/delegations`,
    {
      method: "POST",
      body: JSON.stringify(payload),
    },
  );
}

export function fetchDelegationStatus(
  parentSessionId: string,
  delegationId: string,
  options?: AbortableRequestOptions,
) {
  const parent = encodeURIComponent(parentSessionId);
  const delegation = encodeURIComponent(delegationId);
  return request<DelegationStatusResponse>(
    `/api/sessions/${parent}/delegations/${delegation}`,
    options?.signal ? { signal: options.signal } : undefined,
  );
}

export function fetchDelegationResult(
  parentSessionId: string,
  delegationId: string,
) {
  const parent = encodeURIComponent(parentSessionId);
  const delegation = encodeURIComponent(delegationId);
  return request<DelegationResultResponse>(
    `/api/sessions/${parent}/delegations/${delegation}/result`,
  );
}

export function cancelDelegation(parentSessionId: string, delegationId: string) {
  const parent = encodeURIComponent(parentSessionId);
  const delegation = encodeURIComponent(delegationId);
  return request<DelegationStatusResponse>(
    `/api/sessions/${parent}/delegations/${delegation}/cancel`,
    {
      method: "POST",
    },
  );
}

export function createDelegationWait(
  parentSessionId: string,
  payload: CreateDelegationWaitRequest,
) {
  return request<DelegationWaitResponse>(
    `/api/sessions/${encodeURIComponent(parentSessionId)}/delegation-waits`,
    {
      method: "POST",
      body: JSON.stringify(payload),
    },
  );
}

export function fetchConversationMarkers(sessionId: string) {
  return request<ConversationMarkersResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/markers`,
  );
}

export function createConversationMarker(
  sessionId: string,
  payload: CreateConversationMarkerRequest,
) {
  return request<ConversationMarkerResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/markers`,
    {
      method: "POST",
      body: JSON.stringify(payload),
    },
  );
}

export function updateConversationMarker(
  sessionId: string,
  markerId: string,
  payload: UpdateConversationMarkerRequest,
) {
  const session = encodeURIComponent(sessionId);
  const marker = encodeURIComponent(markerId);
  return request<ConversationMarkerResponse>(
    `/api/sessions/${session}/markers/${marker}`,
    {
      method: "PATCH",
      body: JSON.stringify(payload),
    },
  );
}

export function deleteConversationMarker(sessionId: string, markerId: string) {
  const session = encodeURIComponent(sessionId);
  const marker = encodeURIComponent(markerId);
  return request<DeleteConversationMarkerResponse>(
    `/api/sessions/${session}/markers/${marker}`,
    {
      method: "DELETE",
    },
  );
}

export function createProject(payload: CreateProjectRequest) {
  return request<CreateProjectResponse>("/api/projects", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export function deleteProject(projectId: string) {
  return request<StateResponse>(
    `/api/projects/${encodeURIComponent(projectId)}`,
    {
      method: "DELETE",
    },
  );
}

export function fetchOrchestratorTemplates() {
  return request<OrchestratorTemplatesResponse>("/api/orchestrators/templates");
}

export function createOrchestratorTemplate(payload: OrchestratorTemplateDraft) {
  return request<OrchestratorTemplateResponse>("/api/orchestrators/templates", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export function updateOrchestratorTemplate(
  templateId: string,
  payload: OrchestratorTemplateDraft,
) {
  return request<OrchestratorTemplateResponse>(
    `/api/orchestrators/templates/${encodeURIComponent(templateId)}`,
    {
      method: "PUT",
      body: JSON.stringify(payload),
    },
  );
}

export function deleteOrchestratorTemplate(templateId: string) {
  return request<OrchestratorTemplatesResponse>(
    `/api/orchestrators/templates/${encodeURIComponent(templateId)}`,
    {
      method: "DELETE",
    },
  );
}

export function fetchOrchestratorInstances() {
  return request<OrchestratorInstancesResponse>("/api/orchestrators");
}

export function fetchOrchestratorInstance(instanceId: string) {
  return request<OrchestratorInstanceResponse>(
    `/api/orchestrators/${encodeURIComponent(instanceId)}`,
  );
}

/**
 * Omits empty-string project ids so the backend can fall back to the template project.
 */
export function createOrchestratorInstance(
  templateId: string,
  projectId?: string | null,
  template?: OrchestratorTemplateDraft,
) {
  return request<CreateOrchestratorInstanceResponse>("/api/orchestrators", {
    method: "POST",
    body: JSON.stringify({
      templateId,
      ...(projectId ? { projectId } : {}),
      ...(template ? { template } : {}),
    }),
  });
}

export function pauseOrchestratorInstance(instanceId: string) {
  return request<StateResponse>(
    `/api/orchestrators/${encodeURIComponent(instanceId)}/pause`,
    {
      method: "POST",
    },
  );
}

export function resumeOrchestratorInstance(instanceId: string) {
  return request<StateResponse>(
    `/api/orchestrators/${encodeURIComponent(instanceId)}/resume`,
    {
      method: "POST",
    },
  );
}

export function stopOrchestratorInstance(instanceId: string) {
  return request<StateResponse>(
    `/api/orchestrators/${encodeURIComponent(instanceId)}/stop`,
    {
      method: "POST",
    },
  );
}
export function pickProjectRoot() {
  return request<PickProjectRootResponse>("/api/projects/pick", {
    method: "POST",
  });
}

type SendMessageAttachmentInput = Pick<
  ImageAttachment,
  "fileName" | "mediaType"
> & {
  data: string;
};

export function sendMessage(
  sessionId: string,
  text: string,
  attachments: SendMessageAttachmentInput[],
  expandedText?: string | null,
) {
  return request<StateResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/messages`,
    {
      method: "POST",
      body: JSON.stringify({ text, attachments, expandedText }),
    },
  );
}

export function cancelQueuedPrompt(sessionId: string, promptId: string) {
  return request<StateResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/queued-prompts/${encodeURIComponent(promptId)}/cancel`,
    {
      method: "POST",
    },
  );
}

export function submitApproval(
  sessionId: string,
  messageId: string,
  decision: ApprovalDecision,
) {
  return request<StateResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/approvals/${encodeURIComponent(messageId)}`,
    {
      method: "POST",
      body: JSON.stringify({ decision }),
    },
  );
}

export function submitUserInput(
  sessionId: string,
  messageId: string,
  answers: Record<string, string[]>,
) {
  return request<StateResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/user-input/${encodeURIComponent(messageId)}`,
    {
      method: "POST",
      body: JSON.stringify({ answers }),
    },
  );
}

export function submitMcpElicitation(
  sessionId: string,
  messageId: string,
  action: McpElicitationAction,
  content?: JsonValue | null,
) {
  return request<StateResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/mcp-elicitation/${encodeURIComponent(messageId)}`,
    {
      method: "POST",
      body: JSON.stringify({ action, content: content ?? null }),
    },
  );
}

export function submitCodexAppRequest(
  sessionId: string,
  messageId: string,
  result: JsonValue,
) {
  return request<StateResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/codex/requests/${encodeURIComponent(messageId)}`,
    {
      method: "POST",
      body: JSON.stringify({ result }),
    },
  );
}

export function updateSessionSettings(
  sessionId: string,
  payload: {
    model?: string;
    sandboxMode?: SandboxMode;
    approvalPolicy?: ApprovalPolicy;
    claudeEffort?: ClaudeEffortLevel;
    reasoningEffort?: CodexReasoningEffort;
    cursorMode?: CursorMode;
    claudeApprovalMode?: ClaudeApprovalMode;
    geminiApprovalMode?: GeminiApprovalMode;
  },
) {
  return request<StateResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/settings`,
    {
      method: "POST",
      body: JSON.stringify(payload),
    },
  );
}

export function refreshSessionModelOptions(sessionId: string) {
  return request<StateResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/model-options/refresh`,
    {
      method: "POST",
    },
  );
}

export function forkCodexThread(sessionId: string) {
  return request<CreateSessionResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/codex/thread/fork`,
    {
      method: "POST",
    },
  );
}

export function archiveCodexThread(sessionId: string) {
  return request<StateResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/codex/thread/archive`,
    {
      method: "POST",
    },
  );
}

export function unarchiveCodexThread(sessionId: string) {
  return request<StateResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/codex/thread/unarchive`,
    {
      method: "POST",
    },
  );
}

export function compactCodexThread(sessionId: string) {
  return request<StateResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/codex/thread/compact`,
    {
      method: "POST",
    },
  );
}

export function rollbackCodexThread(sessionId: string, numTurns: number) {
  return request<StateResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/codex/thread/rollback`,
    {
      method: "POST",
      body: JSON.stringify({ numTurns }),
    },
  );
}

export function fetchAgentCommands(sessionId: string) {
  return request<AgentCommandsResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/agent-commands`,
  );
}

export function resolveAgentCommand(
  sessionId: string,
  commandName: string,
  payload: ResolveAgentCommandRequest,
) {
  return request<ResolveAgentCommandResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/agent-commands/${encodeURIComponent(commandName)}/resolve`,
    {
      method: "POST",
      body: JSON.stringify(payload),
    },
  );
}

export function fetchInstructionSearch(sessionId: string, queryText: string) {
  const query = new URLSearchParams({
    q: queryText,
    sessionId,
  });

  return request<InstructionSearchResponse>(
    `/api/instructions/search?${query.toString()}`,
  );
}

export function renameSession(sessionId: string, name: string) {
  return request<StateResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/settings`,
    {
      method: "POST",
      body: JSON.stringify({ name }),
    },
  );
}

export function killSession(sessionId: string) {
  return request<StateResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/kill`,
    {
      method: "POST",
    },
  );
}

export function stopSession(sessionId: string) {
  return request<StateResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/stop`,
    {
      method: "POST",
    },
  );
}

export function fetchReviewDocument(changeSetId: string, scope?: RequestScope) {
  return request<ReviewDocumentResponse>(
    buildScopedPath(`/api/reviews/${encodeURIComponent(changeSetId)}`, scope),
  );
}

export function saveReviewDocument(
  changeSetId: string,
  review: ReviewDocument,
  scope?: RequestScope,
) {
  return request<ReviewDocumentResponse>(
    buildScopedPath(`/api/reviews/${encodeURIComponent(changeSetId)}`, scope),
    {
      method: "PUT",
      body: JSON.stringify(review),
    },
  );
}

export function fetchReviewSummary(changeSetId: string, scope?: RequestScope) {
  return request<ReviewSummaryResponse>(
    buildScopedPath(
      `/api/reviews/${encodeURIComponent(changeSetId)}/summary`,
      scope,
    ),
  );
}

function buildScopedPath(path: string, scope?: RequestScope) {
  const query = new URLSearchParams();
  const sessionId = scope?.sessionId?.trim();
  if (sessionId) {
    query.set("sessionId", sessionId);
  }

  const projectId = scope?.projectId?.trim();
  if (projectId) {
    query.set("projectId", projectId);
  }

  const queryString = query.toString();
  return queryString ? `${path}?${queryString}` : path;
}

export function fetchFile(path: string, scope?: RequestScope) {
  const query = new URLSearchParams({
    path,
  });

  const sessionId = scope?.sessionId?.trim();
  if (sessionId) {
    query.set("sessionId", sessionId);
  }

  const projectId = scope?.projectId?.trim();
  if (projectId) {
    query.set("projectId", projectId);
  }

  return request<FileResponse>(`/api/file?${query.toString()}`);
}

export type SaveFileOptions = RequestScope & {
  baseHash?: string | null;
  overwrite?: boolean;
};

export function saveFile(
  path: string,
  content: string,
  options?: SaveFileOptions,
) {
  const sessionId = options?.sessionId?.trim();
  const projectId = options?.projectId?.trim();
  const baseHash = options?.baseHash?.trim();

  return request<FileResponse>("/api/file", {
    method: "PUT",
    body: JSON.stringify({
      path,
      content,
      ...(baseHash ? { baseHash } : {}),
      ...(options?.overwrite !== undefined
        ? { overwrite: options.overwrite }
        : {}),
      ...(sessionId ? { sessionId } : {}),
      ...(projectId ? { projectId } : {}),
    }),
  });
}

export function fetchDirectory(path: string, scope?: RequestScope) {
  const query = new URLSearchParams({
    path,
  });

  const sessionId = scope?.sessionId?.trim();
  if (sessionId) {
    query.set("sessionId", sessionId);
  }

  const projectId = scope?.projectId?.trim();
  if (projectId) {
    query.set("projectId", projectId);
  }

  return request<DirectoryResponse>(`/api/fs?${query.toString()}`);
}

export function fetchGitStatus(
  path: string,
  sessionId: string | null,
  options?: {
    projectId?: string | null;
  },
) {
  const query = new URLSearchParams({
    path,
  });

  if (sessionId) {
    query.set("sessionId", sessionId);
  }

  const projectId = options?.projectId?.trim();
  if (projectId) {
    query.set("projectId", projectId);
  }

  return request<GitStatusResponse>(`/api/git/status?${query.toString()}`);
}

export type GitDiffRequestPayload = {
  originalPath?: string | null;
  path: string;
  sectionId: GitDiffSection;
  sessionId?: string | null;
  projectId?: string | null;
  statusCode?: string | null;
  workdir: string;
};

export function fetchGitDiff(payload: GitDiffRequestPayload) {
  return request<GitDiffResponse>("/api/git/diff", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export function applyGitFileAction(payload: {
  action: GitFileAction;
  originalPath?: string | null;
  path: string;
  sessionId?: string | null;
  projectId?: string | null;
  statusCode?: string | null;
  workdir: string;
}) {
  return request<GitStatusResponse>("/api/git/file", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export function commitGitChanges(payload: {
  message: string;
  sessionId?: string | null;
  projectId?: string | null;
  workdir: string;
}) {
  return request<GitCommitResponse>("/api/git/commit", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export function pushGitChanges(payload: {
  sessionId?: string | null;
  projectId?: string | null;
  workdir: string;
}) {
  return request<GitRepoActionResponse>("/api/git/push", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export function syncGitChanges(payload: {
  sessionId?: string | null;
  projectId?: string | null;
  workdir: string;
}) {
  return request<GitRepoActionResponse>("/api/git/sync", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}
