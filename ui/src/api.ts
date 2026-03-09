import type {
  ApprovalDecision,
  AgentType,
  ApprovalPolicy,
  ClaudeApprovalMode,
  ImageAttachment,
  SandboxMode,
  Session,
} from "./types";

export type StateResponse = {
  sessions: Session[];
};

export type FileResponse = {
  path: string;
  content: string;
};

type CreateSessionRequest = {
  agent?: AgentType;
  name?: string;
  workdir?: string;
};

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    headers: {
      "Content-Type": "application/json",
    },
    ...init,
  });

  const raw = await response.text();
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
