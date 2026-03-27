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
  codex?: CodexState;
  agentReadiness?: AgentReadiness[];
  preferences?: AppPreferences;
  projects: Project[];
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

export type OrchestratorTemplatesResponse = {
  templates: OrchestratorTemplate[];
};

export type OrchestratorTemplateResponse = {
  template: OrchestratorTemplate;
};
export type PickProjectRootResponse = {
  path?: string | null;
};

export type FileResponse = {
  path: string;
  content: string;
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

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    headers: {
      "Content-Type": "application/json",
    },
    ...init,
  });

  const contentType = response.headers.get("content-type") ?? "";
  const raw = await response.text();
  if (looksLikeHtmlResponse(raw, contentType)) {
    throw new Error(formatUnavailableApiMessage(path, response.status));
  }

  if (!response.ok) {
    throw new Error(extractError(raw, response.status));
  }

  if (!raw) {
    return {} as T;
  }

  return JSON.parse(raw) as T;
}

export function fetchState() {
  return request<StateResponse>("/api/state");
}

export function updateAppSettings(payload: {
  defaultCodexReasoningEffort?: CodexReasoningEffort;
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

export function saveFile(path: string, content: string, scope?: RequestScope) {
  const sessionId = scope?.sessionId?.trim();
  const projectId = scope?.projectId?.trim();

  return request<FileResponse>("/api/file", {
    method: "PUT",
    body: JSON.stringify({ path, content, sessionId, projectId }),
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

export function fetchGitDiff(payload: {
  originalPath?: string | null;
  path: string;
  sectionId: GitDiffSection;
  sessionId?: string | null;
  projectId?: string | null;
  statusCode?: string | null;
  workdir: string;
}) {
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
