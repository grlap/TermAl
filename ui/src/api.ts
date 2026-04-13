import type {
  ApprovalDecision,
  AgentType,
  AgentReadiness,
  AgentCommand,
  ApprovalPolicy,
  AppPreferences,
  ClaudeApprovalMode,
  ClaudeEffortLevel,
  CodexReasoningEffort,
  CodexState,
  CursorMode,
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

export type StateResponse = {
  revision: number;
  codex: CodexState;
  agentReadiness: AgentReadiness[];
  preferences: AppPreferences;
  projects: Project[];
  orchestrators: OrchestratorInstance[];
  workspaces: WorkspaceLayoutSummary[];
  sessions: Session[];
};

export type CreateSessionResponse = {
  sessionId: string;
  state: StateResponse;
};

export type CreateProjectResponse = {
  projectId: string;
  state: StateResponse;
};

export type WorkspaceLayoutDocument = {
  id: string;
  revision: number;
  updatedAt: string;
  controlPanelSide: "left" | "right";
  themeId?: string;
  styleId?: string;
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

export type GitDiffResponse = {
  changeType: "edit" | "create";
  changeSetId?: string | null;
  diff: string;
  diffId: string;
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

type CreateProjectRequest = {
  name?: string;
  rootPath: string;
  remoteId?: string;
};

export type AgentCommandsResponse = {
  commands: AgentCommand[];
};

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

async function request<T>(path: string, init?: RequestInit): Promise<T> {
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
    throw createResponseError(raw, response.status);
  }

  if (!raw) {
    return {} as T;
  }

  return JSON.parse(raw) as T;
}

export function fetchState() {
  return request<StateResponse>("/api/state");
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
    fontSizePx?: number;
    editorFontSizePx?: number;
    densityPercent?: number;
    workspace: unknown;
  },
  options?: {
    keepalive?: boolean;
  },
) {
  return request<WorkspaceLayoutResponse>(`/api/workspaces/${encodeURIComponent(workspaceId)}`, {
    method: "PUT",
    body: JSON.stringify(payload),
    keepalive: options?.keepalive,
  });
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

export function createSession(payload: CreateSessionRequest) {
  return request<CreateSessionResponse>("/api/sessions", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export function createProject(payload: CreateProjectRequest) {
  return request<CreateProjectResponse>("/api/projects", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

export function deleteProject(projectId: string) {
  return request<StateResponse>(`/api/projects/${encodeURIComponent(projectId)}`, {
    method: "DELETE",
  });
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
  return request<StateResponse>(`/api/orchestrators/${encodeURIComponent(instanceId)}/pause`, {
    method: "POST",
  });
}

export function resumeOrchestratorInstance(instanceId: string) {
  return request<StateResponse>(`/api/orchestrators/${encodeURIComponent(instanceId)}/resume`, {
    method: "POST",
  });
}

export function stopOrchestratorInstance(instanceId: string) {
  return request<StateResponse>(`/api/orchestrators/${encodeURIComponent(instanceId)}/stop`, {
    method: "POST",
  });
}
export function pickProjectRoot() {
  return request<PickProjectRootResponse>("/api/projects/pick", {
    method: "POST",
  });
}

type SendMessageAttachmentInput = Pick<ImageAttachment, "fileName" | "mediaType"> & {
  data: string;
};

export function sendMessage(
  sessionId: string,
  text: string,
  attachments: SendMessageAttachmentInput[],
  expandedText?: string | null,
) {
  return request<StateResponse>(`/api/sessions/${encodeURIComponent(sessionId)}/messages`, {
    method: "POST",
    body: JSON.stringify({ text, attachments, expandedText }),
  });
}

export function cancelQueuedPrompt(sessionId: string, promptId: string) {
  return request<StateResponse>(
    `/api/sessions/${encodeURIComponent(sessionId)}/queued-prompts/${encodeURIComponent(promptId)}/cancel`,
    {
      method: "POST",
    },
  );
}

export function submitApproval(sessionId: string, messageId: string, decision: ApprovalDecision) {
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
  return request<StateResponse>(`/api/sessions/${encodeURIComponent(sessionId)}/settings`, {
    method: "POST",
    body: JSON.stringify(payload),
  });
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

export function fetchInstructionSearch(sessionId: string, queryText: string) {
  const query = new URLSearchParams({
    q: queryText,
    sessionId,
  });

  return request<InstructionSearchResponse>(`/api/instructions/search?${query.toString()}`);
}

export function renameSession(sessionId: string, name: string) {
  return request<StateResponse>(`/api/sessions/${encodeURIComponent(sessionId)}/settings`, {
    method: "POST",
    body: JSON.stringify({ name }),
  });
}

export function killSession(sessionId: string) {
  return request<StateResponse>(`/api/sessions/${encodeURIComponent(sessionId)}/kill`, {
    method: "POST",
  });
}

export function stopSession(sessionId: string) {
  return request<StateResponse>(`/api/sessions/${encodeURIComponent(sessionId)}/stop`, {
    method: "POST",
  });
}

export function fetchReviewDocument(changeSetId: string, scope?: RequestScope) {
  return request<ReviewDocumentResponse>(
    buildScopedPath(`/api/reviews/${encodeURIComponent(changeSetId)}`, scope),
  );
}

export function saveReviewDocument(changeSetId: string, review: ReviewDocument, scope?: RequestScope) {
  return request<ReviewDocumentResponse>(buildScopedPath(`/api/reviews/${encodeURIComponent(changeSetId)}`, scope), {
    method: "PUT",
    body: JSON.stringify(review),
  });
}

export function fetchReviewSummary(changeSetId: string, scope?: RequestScope) {
  return request<ReviewSummaryResponse>(
    buildScopedPath(`/api/reviews/${encodeURIComponent(changeSetId)}/summary`, scope),
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

export function saveFile(path: string, content: string, options?: SaveFileOptions) {
  const sessionId = options?.sessionId?.trim();
  const projectId = options?.projectId?.trim();
  const baseHash = options?.baseHash?.trim();

  return request<FileResponse>("/api/file", {
    method: "PUT",
    body: JSON.stringify({
      path,
      content,
      ...(baseHash ? { baseHash } : {}),
      ...(options?.overwrite !== undefined ? { overwrite: options.overwrite } : {}),
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

  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) {
        break;
      }
      buffer = normalizeSseBuffer(buffer + decoder.decode(value, { stream: true }));
      const result = processTerminalSseBuffer(buffer, onOutput);
      buffer = result.buffer;
      if (result.response) {
        return result.response;
      }
    }
  } finally {
    reader.releaseLock();
  }

  buffer = normalizeSseBuffer(buffer + decoder.decode());
  const result = processTerminalSseBuffer(buffer, onOutput, true);
  if (result.response) {
    return result.response;
  }

  throw new ApiRequestError(
    "request-failed",
    "Terminal stream ended before the command completed.",
    { status: 500 },
  );
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
      onOutput?.(JSON.parse(parsed.data) as TerminalCommandOutputEvent);
    } else if (parsed.event === "complete") {
      response = JSON.parse(parsed.data) as TerminalCommandResponse;
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

function createTerminalStreamEventError(raw: string) {
  try {
    const parsed = JSON.parse(raw) as TerminalCommandStreamErrorEvent;
    const status = parsed.status ?? 500;
    return createResponseError(
      JSON.stringify({ error: parsed.error ?? `Request failed with status ${status}.` }),
      status,
    );
  } catch {
    return createResponseError(raw, 500);
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

function createResponseError(raw: string, status: number) {
  if (status === 502 || status === 503 || status === 504) {
    return createBackendUnavailableError(
      "The TermAl backend is unavailable.",
      status,
    );
  }

  return new ApiRequestError("request-failed", extractError(raw, status), {
    status,
  });
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

  const trimmed = raw.trimStart().toLowerCase();
  return trimmed.startsWith("<!doctype html") || trimmed.startsWith("<html");
}

function formatUnavailableApiMessage(path: string, status: number) {
  const endpoint = path.split("?")[0] ?? path;
  const statusSuffix = status > 0 ? ` (HTTP ${status})` : "";
  return `The running backend does not expose ${endpoint}${statusSuffix}. Restart TermAl so the latest API routes are loaded.`;
}
