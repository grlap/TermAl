import type {
  ApprovalDecision,
  AgentType,
  ApprovalPolicy,
  ClaudeApprovalMode,
  CodexState,
  ImageAttachment,
  SandboxMode,
  Session,
} from "./types";

export type StateResponse = {
  codex?: CodexState;
  sessions: Session[];
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

type CreateSessionRequest = {
  agent?: AgentType;
  name?: string;
  workdir?: string;
  approvalPolicy?: ApprovalPolicy;
  sandboxMode?: SandboxMode;
  claudeApprovalMode?: ClaudeApprovalMode;
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

export function createSession(payload: CreateSessionRequest) {
  return request<Session>("/api/sessions", {
    method: "POST",
    body: JSON.stringify(payload),
  });
}

type SendMessageAttachmentInput = Pick<ImageAttachment, "fileName" | "mediaType"> & {
  data: string;
};

export function sendMessage(sessionId: string, text: string, attachments: SendMessageAttachmentInput[]) {
  return request<StateResponse>(`/api/sessions/${encodeURIComponent(sessionId)}/messages`, {
    method: "POST",
    body: JSON.stringify({ text, attachments }),
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
    sandboxMode?: SandboxMode;
    approvalPolicy?: ApprovalPolicy;
    claudeApprovalMode?: ClaudeApprovalMode;
  },
) {
  return request<StateResponse>(`/api/sessions/${encodeURIComponent(sessionId)}/settings`, {
    method: "POST",
    body: JSON.stringify(payload),
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

export function fetchFile(path: string) {
  return request<FileResponse>(`/api/file?path=${encodeURIComponent(path)}`);
}

export function saveFile(path: string, content: string) {
  return request<FileResponse>("/api/file", {
    method: "PUT",
    body: JSON.stringify({ path, content }),
  });
}

export function fetchDirectory(path: string) {
  return request<DirectoryResponse>(`/api/fs?path=${encodeURIComponent(path)}`);
}

export function fetchGitStatus(path: string) {
  return request<GitStatusResponse>(`/api/git/status?path=${encodeURIComponent(path)}`);
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
