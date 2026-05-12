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
import { sanitizeUserFacingErrorMessage } from "./error-messages";

// Upper bound on how much SSE text the terminal stream reader will buffer
// before surfacing an error. A completion frame carries the full terminal
// response, which can include up to 512 KiB of stdout plus the same of
// stderr. JSON encoding can expand each byte up to 6× for ASCII control
// characters (`\u00XX`), and common newline-heavy output already doubles the
// raw size, so cap at 16× the backend output budget (8 Mi chars) to let
// legitimate completion frames through while still bounding memory if a
// remote stalls without emitting a frame delimiter.
//
// Exported so `ui/src/api.test.ts` can pin the frontend ↔ backend cap
// coupling without duplicating the derivation formula on both sides. The
// backend computes its equivalent pending cap as
// `TERMINAL_OUTPUT_MAX_BYTES * 16` in `src/api.rs`; if either side drifts
// the other's regression test fails.
export const TERMINAL_SSE_BUFFER_MAX_CHARS = 16 * 512 * 1024;

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

export type TerminalCommandResponse = {
  command: string;
  durationMs: number;
  exitCode?: number | null;
  outputTruncated: boolean;
  shell: string;
  stderr: string;
  stdout: string;
  success: boolean;
  timedOut: boolean;
  workdir: string;
};

export type TerminalOutputStream = "stdout" | "stderr";

export type TerminalCommandOutputEvent = {
  stream: TerminalOutputStream;
  text: string;
};

type TerminalCommandStreamErrorEvent = {
  error?: string;
  status?: number;
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

/**
 * High-level request failure category for UI recovery paths.
 *
 * Do not treat `"request-failed"` as a non-5xx guarantee. Routes that opt into
 * `preserveGatewayErrorBody` intentionally keeps parseable 502/503/504 JSON
 * error bodies as `"request-failed"` so callers can surface actionable
 * upstream diagnostics.
 * Branch on `status` and `restartRequired` when status-class behavior matters.
 */
export type ApiRequestErrorKind = "backend-unavailable" | "request-failed";

export class ApiRequestError extends Error {
  declare readonly cause: unknown;
  readonly kind: ApiRequestErrorKind;
  readonly status: number | null;
  readonly restartRequired: boolean;

  constructor(
    kind: ApiRequestErrorKind,
    message: string,
    options?: {
      status?: number | null;
      restartRequired?: boolean;
      cause?: unknown;
    },
  ) {
    // TypeScript's current lib target in this repo does not yet model the ES2022
    // Error options bag, but supported runtimes do and tooling reads it there.
    // @ts-expect-error ES2022 Error options are available at runtime.
    super(message, { cause: options?.cause });
    this.name = "ApiRequestError";
    this.kind = kind;
    this.status = options?.status ?? null;
    this.restartRequired = options?.restartRequired ?? false;
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

export function isBackendUnavailableError(
  error: unknown,
): error is ApiRequestError {
  return (
    error instanceof ApiRequestError && error.kind === "backend-unavailable"
  );
}

type RequestOptions = {
  /**
   * Keep parseable 502/503/504 JSON error bodies as request-failed details
   * instead of mapping them to the generic backend-unavailable UI path. Use
   * this only for routes that deliberately proxy a third-party service and
   * return actionable JSON errors for upstream failures.
   */
  preserveGatewayErrorBody?: boolean;
};

async function request<T>(
  path: string,
  init?: RequestInit,
  options?: RequestOptions,
): Promise<T> {
  const response = await performRequest(path, init);

  const contentType = response.headers.get("content-type") ?? "";
  const raw = await response.text();
  if (looksLikeHtmlResponse(raw, contentType)) {
    throw createBackendUnavailableError(
      formatUnavailableApiMessage(path, response.status),
      response.status,
      { restartRequired: true },
    );
  }

  if (!response.ok) {
    throw createResponseError(raw, response.status, options);
  }

  if (!raw) {
    return {} as T;
  }

  return JSON.parse(raw) as T;
}

export function fetchState() {
  return request<StateResponse>("/api/state");
}

export function fetchSession(sessionId: string) {
  return request<SessionResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}`,
  );
}

export function fetchSessionTail(sessionId: string, messageLimit: number) {
  const query = new URLSearchParams({
    tail: String(Math.max(1, Math.floor(messageLimit))),
  });
  return request<SessionResponse>(
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
  running: boolean;
  // lifecycle describes who owns relay startup; running is the current poll-loop state.
  // UI labels derive "Stopped" only when lifecycle is inProcess and
  // enabled/configured are true while running is false.
  lifecycle: "manual" | "inProcess";
  linkedChatId?: number | null;
  botTokenMasked?: string | null;
  subscribedProjectIds: string[];
  defaultProjectId?: string | null;
  defaultSessionId?: string | null;
};

export type UpdateTelegramConfigPayload = {
  enabled?: boolean;
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

export function runTerminalCommand(payload: {
  command: string;
  sessionId?: string | null;
  projectId?: string | null;
  workdir: string;
}) {
  return request<TerminalCommandResponse>("/api/terminal/run", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export async function runTerminalCommandStream(
  payload: {
    command: string;
    sessionId?: string | null;
    projectId?: string | null;
    workdir: string;
  },
  options: {
    onOutput?: (event: TerminalCommandOutputEvent) => void;
    signal?: AbortSignal;
  } = {},
) {
  const endpoint = "/api/terminal/run/stream";
  const response = await performRequest(endpoint, {
    method: "POST",
    body: JSON.stringify(payload),
    signal: options.signal,
  });

  const contentType = response.headers.get("content-type") ?? "";
  if (looksLikeHtmlResponse("", contentType)) {
    await response.text().catch(() => "");
    throw createBackendUnavailableError(
      formatUnavailableApiMessage(endpoint, response.status),
      response.status,
      { restartRequired: true },
    );
  }

  if (!response.ok) {
    const raw = await response.text();
    throw createResponseError(raw, response.status);
  }

  if (!response.body) {
    throw createBackendUnavailableError(
      "The TermAl backend did not return a terminal stream.",
      response.status,
    );
  }

  return readTerminalCommandEventStream(response.body, options.onOutput);
}

async function readTerminalCommandEventStream(
  body: ReadableStream<Uint8Array>,
  onOutput: ((event: TerminalCommandOutputEvent) => void) | undefined,
) {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  let completed = false;

  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) {
        break;
      }
      buffer = normalizeSseBuffer(
        buffer + decoder.decode(value, { stream: true }),
      );
      // Drain every complete frame first so that coalesced valid frames
      // whose accumulated text exceeds `TERMINAL_SSE_BUFFER_MAX_CHARS` are
      // not rejected before parsing. `processTerminalSseBuffer` enforces
      // the per-frame cap on each drained frame, and the post-drain
      // `assertTerminalSseBufferSize` check bounds the trailing incomplete
      // buffer so a single in-flight frame cannot grow unboundedly across
      // chunks. Without the drain-first order, a fetch chunk that carries
      // a valid `output` plus a valid `complete` frame (each under the
      // cap) would trip the old whole-buffer check whenever their sum
      // crossed the cap.
      const result = processTerminalSseBuffer(buffer, onOutput);
      buffer = result.buffer;
      assertTerminalSseBufferSize(buffer.length);
      if (result.response) {
        completed = true;
        return result.response;
      }
    }
    buffer = normalizeSseBuffer(buffer + decoder.decode());
    const result = processTerminalSseBuffer(buffer, onOutput, true);
    buffer = result.buffer;
    assertTerminalSseBufferSize(buffer.length);
    if (result.response) {
      completed = true;
      return result.response;
    }

    throw new ApiRequestError(
      "request-failed",
      "Terminal stream ended before the command completed.",
      { status: 500 },
    );
  } finally {
    if (!completed) {
      await reader.cancel().catch(() => {});
    }
    reader.releaseLock();
  }
}

function processTerminalSseBuffer(
  buffer: string,
  onOutput: ((event: TerminalCommandOutputEvent) => void) | undefined,
  flush = false,
) {
  let remaining = buffer;
  let response: TerminalCommandResponse | null = null;

  while (true) {
    const frameEnd = remaining.indexOf("\n\n");
    if (frameEnd < 0) {
      if (!flush || remaining.trim() === "") {
        break;
      }
    }
    // Each individual SSE frame must stay within the buffer cap. The
    // `readTerminalCommandEventStream` loop drains complete frames before
    // re-checking the trailing buffer, so without this per-frame guard a
    // malicious or broken remote could smuggle a single frame of arbitrary
    // size inside one coalesced fetch chunk: the drain would consume it,
    // the post-drain check would see an empty trailing buffer, and the
    // parser would accept the oversized frame outright. Routed through
    // the same `assertTerminalSseBufferSize` helper as the trailing-
    // buffer check so both cap violations share one error shape (413 +
    // `request-failed`) and never drift out of sync.
    const frameLength = frameEnd >= 0 ? frameEnd : remaining.length;
    assertTerminalSseBufferSize(frameLength);
    const frame = frameEnd >= 0 ? remaining.slice(0, frameEnd) : remaining;
    remaining = frameEnd >= 0 ? remaining.slice(frameEnd + 2) : "";
    if (!frame.trim()) {
      if (frameEnd < 0) {
        break;
      }
      continue;
    }

    const parsed = parseSseFrame(frame);
    if (parsed.event === "output") {
      const output = parseTerminalOutputEvent(parsed.data);
      onOutput?.(output);
    } else if (parsed.event === "complete") {
      response = parseTerminalCommandResponse(parsed.data);
      break;
    } else if (parsed.event === "error") {
      throw createTerminalStreamEventError(parsed.data);
    }

    if (frameEnd < 0) {
      break;
    }
  }

  return { buffer: remaining, response };
}

function normalizeSseBuffer(value: string) {
  return value.replace(/\r\n/g, "\n").replace(/\r/g, "\n");
}

// Shared guard for the two SSE buffer-cap violation sites:
//
// 1. `processTerminalSseBuffer` passes the length of a single drained
//    frame, so no individual frame can slip past the cap even when it
//    arrives inside a coalesced chunk that would also fit another valid
//    frame (which the drain-first loop would otherwise let through).
// 2. `readTerminalCommandEventStream` passes the length of the trailing
//    incomplete buffer after draining every complete frame, so an
//    in-flight frame cannot grow unboundedly across chunks.
//
// The thrown status is `413 Payload Too Large`, intentionally distinct
// from the 502/503/504 statuses that `createResponseError` maps to
// `kind: "backend-unavailable"`. A cap rejection is a semantic payload
// violation, not an upstream gateway failure, and using a status that
// `createResponseError` does NOT special-case keeps the synthetic
// cap-rejection error shape aligned with the rest of the codebase
// regardless of how the same route handles a real HTTP 502 response.
function assertTerminalSseBufferSize(length: number) {
  if (length <= TERMINAL_SSE_BUFFER_MAX_CHARS) {
    return;
  }
  throw new ApiRequestError(
    "request-failed",
    "Terminal stream frame exceeded the allowed size.",
    { status: 413 },
  );
}

function parseSseFrame(frame: string) {
  let event = "message";
  const data: string[] = [];

  for (const line of frame.split("\n")) {
    if (line.startsWith(":")) {
      continue;
    }
    const separator = line.indexOf(":");
    const field = separator >= 0 ? line.slice(0, separator) : line;
    let value = separator >= 0 ? line.slice(separator + 1) : "";
    if (value.startsWith(" ")) {
      value = value.slice(1);
    }
    if (field === "event") {
      event = value;
    } else if (field === "data") {
      data.push(value);
    }
  }

  return { data: data.join("\n"), event };
}

// Shared constructor for malformed SSE payload validation errors thrown by
// `parseTerminalOutputEvent` and `parseTerminalCommandResponse`.
//
// Status is `422 Unprocessable Entity`, intentionally distinct from the
// 502/503/504 statuses that `createResponseError` maps to
// `kind: "backend-unavailable"`. A malformed SSE payload from an
// otherwise-reachable backend is a schema violation on a single stream,
// not an upstream gateway failure. Surfacing it as `status: 502` on the
// `"request-failed"` kind would leave the same numeric status attached
// to two different `ApiRequestError.kind` values on the same route (the
// pre-stream `if (!response.ok)` branch in `runTerminalCommandStream`
// uses `createResponseError` for real HTTP 502 responses and yields
// `kind: "backend-unavailable" / restartRequired: true`). 422 avoids
// that divergence because `createResponseError` does not special-case
// it, and the semantic "valid HTTP request, malformed response body"
// is exactly what 422 is meant for.
//
// This is the same discipline `assertTerminalSseBufferSize` uses for its
// 413 choice: pick a status that `createResponseError` does NOT
// special-case so the synthetic error shape stays consistent with the
// rest of the codebase.
function createMalformedTerminalStreamPayloadError(message: string) {
  return new ApiRequestError("request-failed", message, { status: 422 });
}

function parseTerminalOutputEvent(raw: string): TerminalCommandOutputEvent {
  const parsed = JSON.parse(raw) as unknown;
  if (
    !isRecord(parsed) ||
    (parsed.stream !== "stdout" && parsed.stream !== "stderr") ||
    typeof parsed.text !== "string"
  ) {
    throw createMalformedTerminalStreamPayloadError(
      "Terminal stream returned an invalid output event.",
    );
  }

  return {
    stream: parsed.stream,
    text: parsed.text,
  };
}

function parseTerminalCommandResponse(raw: string): TerminalCommandResponse {
  const parsed = JSON.parse(raw) as unknown;
  if (isRecord(parsed) && typeof parsed.error === "string") {
    throw createTerminalStreamEventError(raw);
  }
  if (
    !isRecord(parsed) ||
    typeof parsed.command !== "string" ||
    typeof parsed.durationMs !== "number" ||
    !Number.isFinite(parsed.durationMs) ||
    // `exitCode` is optional (`null`/`undefined` mean "process was killed
    // or never exited normally"), but when it IS a number it must be
    // finite. Without the `Number.isFinite` branch a remote or malformed
    // JSON payload carrying `NaN` / `Infinity` (the latter via
    // non-standard JSON extensions or a misbehaving proxy) would slip
    // through the validator and corrupt downstream exit-code checks.
    !(
      (typeof parsed.exitCode === "number" &&
        Number.isFinite(parsed.exitCode)) ||
      parsed.exitCode === null ||
      parsed.exitCode === undefined
    ) ||
    typeof parsed.outputTruncated !== "boolean" ||
    typeof parsed.shell !== "string" ||
    typeof parsed.stderr !== "string" ||
    typeof parsed.stdout !== "string" ||
    typeof parsed.success !== "boolean" ||
    typeof parsed.timedOut !== "boolean" ||
    typeof parsed.workdir !== "string"
  ) {
    throw createMalformedTerminalStreamPayloadError(
      "Terminal stream returned an invalid completion event.",
    );
  }

  return {
    command: parsed.command,
    durationMs: parsed.durationMs,
    exitCode: parsed.exitCode,
    outputTruncated: parsed.outputTruncated,
    shell: parsed.shell,
    stderr: parsed.stderr,
    stdout: parsed.stdout,
    success: parsed.success,
    timedOut: parsed.timedOut,
    workdir: parsed.workdir,
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function createTerminalStreamEventError(raw: string) {
  try {
    const parsed = JSON.parse(raw) as TerminalCommandStreamErrorEvent;
    const status = parsed.status ?? 500;
    return new ApiRequestError(
      "request-failed",
      sanitizeUserFacingErrorMessage(
        parsed.error ?? `Request failed with status ${status}.`,
      ),
      { status },
    );
  } catch {
    return new ApiRequestError(
      "request-failed",
      sanitizeUserFacingErrorMessage(raw || "Request failed with status 500."),
      { status: 500 },
    );
  }
}

async function performRequest(path: string, init?: RequestInit) {
  try {
    return await fetch(path, {
      headers: {
        "Content-Type": "application/json",
      },
      ...init,
    });
  } catch (error) {
    throw createBackendUnavailableError(
      "The TermAl backend is unavailable.",
      undefined,
      { cause: error },
    );
  }
}

function createBackendUnavailableError(
  message: string,
  status?: number,
  options?: { restartRequired?: boolean; cause?: unknown },
) {
  return new ApiRequestError("backend-unavailable", message, {
    status,
    restartRequired: options?.restartRequired,
    cause: options?.cause,
  });
}

function createResponseError(
  raw: string,
  status: number,
  options?: RequestOptions,
) {
  if (status === 502 || status === 503 || status === 504) {
    if (options?.preserveGatewayErrorBody) {
      const gatewayError = extractIntentionalGatewayError(raw);
      if (gatewayError) {
        return new ApiRequestError("request-failed", gatewayError, {
          status,
        });
      }
    }
    return createBackendUnavailableError(
      "The TermAl backend is unavailable.",
      status,
    );
  }

  return new ApiRequestError("request-failed", extractError(raw, status), {
    status,
  });
}

function extractIntentionalGatewayError(raw: string) {
  const trimmed = raw.trim();
  if (trimmed.length === 0) {
    return null;
  }

  try {
    const parsed = JSON.parse(trimmed) as { error?: unknown };
    if (typeof parsed.error === "string" && parsed.error.trim().length > 0) {
      return sanitizeUserFacingErrorMessage(parsed.error);
    }
  } catch {
    return null;
  }

  return null;
}

function extractError(raw: string, status: number) {
  if (!raw) {
    return `Request failed with status ${status}.`;
  }

  try {
    const parsed = JSON.parse(raw) as { error?: string };
    if (parsed.error) {
      return sanitizeUserFacingErrorMessage(parsed.error);
    }
  } catch {
    return sanitizeUserFacingErrorMessage(raw);
  }

  return `Request failed with status ${status}.`;
}

function looksLikeHtmlResponse(raw: string, contentType: string) {
  if (contentType.toLowerCase().includes("text/html")) {
    return true;
  }

  const trimmed = raw
    .trimStart()
    .slice(0, 256)
    .toLowerCase();
  return trimmed.startsWith("<!doctype html") || trimmed.startsWith("<html");
}

function formatUnavailableApiMessage(path: string, status: number) {
  const endpoint = path.split("?")[0] ?? path;
  const statusSuffix = status > 0 ? ` (HTTP ${status})` : "";
  return `The running backend does not expose ${endpoint}${statusSuffix}. Restart TermAl so the latest API routes are loaded.`;
}
