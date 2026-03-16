export type AgentType = "Claude" | "Codex" | "Cursor" | "Gemini";
export type SessionStatus = "active" | "idle" | "approval" | "error";
export type SandboxMode = "read-only" | "workspace-write" | "danger-full-access";
export type ApprovalPolicy = "untrusted" | "on-failure" | "on-request" | "never";
export type ClaudeApprovalMode = "ask" | "auto-approve" | "plan";
export type ClaudeEffortLevel = "default" | "low" | "medium" | "high" | "max";
export type CodexReasoningEffort = "none" | "minimal" | "low" | "medium" | "high" | "xhigh";
export type CursorMode = "agent" | "plan" | "ask";
export type GeminiApprovalMode = "default" | "auto_edit" | "yolo" | "plan";
export type AgentReadinessStatus = "ready" | "missing" | "needsSetup";

export type AgentReadiness = {
  agent: AgentType;
  status: AgentReadinessStatus;
  blocking: boolean;
  detail: string;
  commandPath?: string | null;
};

export type AgentCommand = {
  name: string;
  description: string;
  content: string;
  source: string;
};

export type SessionModelOption = {
  label: string;
  value: string;
  description?: string | null;
  badges?: string[];
  supportedClaudeEffortLevels?: ClaudeEffortLevel[];
  defaultReasoningEffort?: CodexReasoningEffort | null;
  supportedReasoningEfforts?: CodexReasoningEffort[];
};

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

export type AppPreferences = {
  defaultCodexReasoningEffort: CodexReasoningEffort;
  defaultClaudeEffort: ClaudeEffortLevel;
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
  modelOptions?: SessionModelOption[];
  approvalPolicy?: ApprovalPolicy | null;
  claudeEffort?: ClaudeEffortLevel | null;
  reasoningEffort?: CodexReasoningEffort | null;
  sandboxMode?: SandboxMode | null;
  cursorMode?: CursorMode | null;
  claudeApprovalMode?: ClaudeApprovalMode | null;
  geminiApprovalMode?: GeminiApprovalMode | null;
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
  | SubagentResultMessage
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
  expandedText?: string | null;
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
  expandedText?: string | null;
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

export type SubagentResultMessage = BaseMessage & {
  type: "subagentResult";
  title: string;
  summary: string;
  conversationId?: string | null;
  turnId?: string | null;
};

export type ApprovalDecision =
  | "pending"
  | "interrupted"
  | "canceled"
  | "accepted"
  | "acceptedForSession"
  | "rejected";

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
  messageIndex: number;
  delta: string;
  preview?: string | null;
};

export type MessageCreatedEvent = {
  type: "messageCreated";
  revision: number;
  sessionId: string;
  messageId: string;
  messageIndex: number;
  message: Message;
  preview: string;
  status: SessionStatus;
};

export type CommandUpdateEvent = {
  type: "commandUpdate";
  revision: number;
  sessionId: string;
  messageId: string;
  messageIndex: number;
  command: string;
  commandLanguage?: string | null;
  output: string;
  outputLanguage?: string | null;
  status: "running" | "success" | "error";
  preview: string;
};

export type DeltaEvent = MessageCreatedEvent | TextDeltaEvent | CommandUpdateEvent;
