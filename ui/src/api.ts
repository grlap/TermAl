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
  Project,
  SandboxMode,
  Session,
} from "./types";

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

type FileRequestScope = {
  projectId?: string | null;
  sessionId?: string | null;
};

export function fetchFile(path: string, scope?: FileRequestScope) {
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

export function saveFile(path: string, content: string, scope?: FileRequestScope) {
  const sessionId = scope?.sessionId?.trim();
  const projectId = scope?.projectId?.trim();

  return request<FileResponse>("/api/file", {
    method: "PUT",
    body: JSON.stringify({ path, content, sessionId, projectId }),
  });
}

export function fetchDirectory(path: string, scope?: FileRequestScope) {
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

function extractError(raw: string, status: number) {
  if (!raw) {
    return `Request failed with status ${status}.`;
  }

  try {
    const parsed = JSON.parse(raw) as { error?: string };
    if (parsed.error) {
      return parsed.error;
    }
  } catch {
    return raw;
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
