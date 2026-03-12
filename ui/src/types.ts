export type AgentType = "Claude" | "Codex";
export type SessionStatus = "active" | "idle" | "approval" | "error";
export type SandboxMode = "read-only" | "workspace-write" | "danger-full-access";
export type ApprovalPolicy = "untrusted" | "on-failure" | "on-request" | "never";
export type ClaudeApprovalMode = "ask" | "auto-approve";

export type CodexRateLimitWindow = {
  resetsAt?: number | null;
  usedPercent?: number | null;
  windowDurationMins?: number | null;
};

export type CodexRateLimits = {
  credits?: unknown | null;
  limitId?: string | null;
  limitName?: string | null;
  planType?: string | null;
  primary?: CodexRateLimitWindow | null;
  secondary?: CodexRateLimitWindow | null;
};

export type CodexState = {
  rateLimits?: CodexRateLimits | null;
};

export type Project = {
  id: string;
  name: string;
  rootPath: string;
};

export type Session = {
  id: string;
  name: string;
  emoji: string;
  agent: AgentType;
  workdir: string;
  projectId?: string | null;
  model: string;
  approvalPolicy?: ApprovalPolicy | null;
  sandboxMode?: SandboxMode | null;
  claudeApprovalMode?: ClaudeApprovalMode | null;
  externalSessionId?: string | null;
  status: SessionStatus;
  preview: string;
  messages: Message[];
  pendingPrompts?: PendingPrompt[];
};

export type Message =
  | TextMessage
  | ThinkingMessage
  | CommandMessage
  | DiffMessage
  | MarkdownMessage
  | ApprovalMessage;

export type ImageAttachment = {
  fileName: string;
  mediaType: string;
  byteSize: number;
};

export type PendingPrompt = {
  id: string;
  timestamp: string;
  text: string;
  attachments?: ImageAttachment[];
};

type BaseMessage = {
  id: string;
  timestamp: string;
  author: "you" | "assistant";
};

export type TextMessage = BaseMessage & {
  type: "text";
  attachments?: ImageAttachment[];
  text: string;
};

export type ThinkingMessage = BaseMessage & {
  type: "thinking";
  title: string;
  lines: string[];
};

export type CommandMessage = BaseMessage & {
  type: "command";
  command: string;
  commandLanguage?: string | null;
  output: string;
  outputLanguage?: string | null;
  status: "running" | "success" | "error";
};

export type DiffMessage = BaseMessage & {
  type: "diff";
  filePath: string;
  summary: string;
  diff: string;
  language?: string | null;
  changeType: "edit" | "create";
};

export type MarkdownMessage = BaseMessage & {
  type: "markdown";
  title: string;
  markdown: string;
};

export type ApprovalDecision = "pending" | "accepted" | "acceptedForSession" | "rejected";

export type ApprovalMessage = BaseMessage & {
  type: "approval";
  title: string;
  command: string;
  commandLanguage?: string | null;
  detail: string;
  decision: ApprovalDecision;
};

export type TextDeltaEvent = {
  type: "textDelta";
  revision: number;
  sessionId: string;
  messageId: string;
  delta: string;
  preview?: string | null;
};

export type CommandUpdateEvent = {
  type: "commandUpdate";
  revision: number;
  sessionId: string;
  messageId: string;
  command: string;
  commandLanguage?: string | null;
  output: string;
  outputLanguage?: string | null;
  status: "running" | "success" | "error";
  preview: string;
};

export type DeltaEvent = TextDeltaEvent | CommandUpdateEvent;
