export type AgentType = "Claude" | "Codex";
export type SessionStatus = "active" | "idle" | "approval" | "error";
export type SandboxMode = "read-only" | "workspace-write" | "danger-full-access";
export type ApprovalPolicy = "untrusted" | "on-failure" | "on-request" | "never";
export type ClaudeApprovalMode = "ask" | "auto-approve";

export type Session = {
  id: string;
  name: string;
  emoji: string;
  agent: AgentType;
  workdir: string;
  model: string;
  approvalPolicy?: ApprovalPolicy | null;
  sandboxMode?: SandboxMode | null;
  claudeApprovalMode?: ClaudeApprovalMode | null;
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
