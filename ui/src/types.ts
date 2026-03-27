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

export type AgentCommandKind = "promptTemplate" | "nativeSlash";

export type AgentCommand = {
  kind?: AgentCommandKind;
  name: string;
  description: string;
  content: string;
  source: string;
  argumentHint?: string | null;
};

export type InstructionDocumentKind =
  | "rootInstruction"
  | "commandInstruction"
  | "reviewerInstruction"
  | "rulesInstruction"
  | "skillInstruction"
  | "referencedInstruction";

export type InstructionRelation =
  | "markdownLink"
  | "fileReference"
  | "directoryDiscovery";

export type InstructionPathStep = {
  excerpt: string;
  fromPath: string;
  line: number;
  relation: InstructionRelation;
  toPath: string;
};

export type InstructionRootPath = {
  rootKind: InstructionDocumentKind;
  rootPath: string;
  steps: InstructionPathStep[];
};

export type InstructionSearchMatch = {
  line: number;
  path: string;
  rootPaths: InstructionRootPath[];
  text: string;
};

export type InstructionSearchResponse = {
  matches: InstructionSearchMatch[];
  query: string;
  workdir: string;
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

export type CodexNoticeKind = "configWarning" | "deprecationNotice" | "runtimeNotice";
export type CodexNoticeLevel = "info" | "warning";

export type CodexNotice = {
  kind: CodexNoticeKind;
  level: CodexNoticeLevel;
  title: string;
  detail: string;
  timestamp: string;
  code?: string | null;
};

export type CodexState = {
  rateLimits?: CodexRateLimits | null;
  notices?: CodexNotice[] | null;
};

export type RemoteTransport = "local" | "ssh";

export type RemoteConfig = {
  id: string;
  name: string;
  transport: RemoteTransport;
  enabled: boolean;
  host?: string | null;
  port?: number | null;
  user?: string | null;
};

export type AppPreferences = {
  defaultCodexReasoningEffort: CodexReasoningEffort;
  defaultClaudeEffort: ClaudeEffortLevel;
  remotes?: RemoteConfig[] | null;
};

export type Project = {
  id: string;
  name: string;
  rootPath: string;
  remoteId?: string | null;
};

export type OrchestratorNodePosition = {
  x: number;
  y: number;
};

export type OrchestratorSessionTemplate = {
  id: string;
  name: string;
  agent: AgentType;
  model?: string | null;
  instructions: string;
  autoApprove: boolean;
  position: OrchestratorNodePosition;
};

export type OrchestratorTransitionTrigger = "onCompletion";
export type OrchestratorTransitionResultMode =
  | "none"
  | "lastResponse"
  | "summary"
  | "summaryAndLastResponse";

export type OrchestratorTransitionAnchor = "top" | "top-right" | "right" | "bottom-right" | "bottom" | "bottom-left" | "left" | "top-left";

export type OrchestratorTemplateTransition = {
  id: string;
  fromSessionId: string;
  toSessionId: string;
  fromAnchor?: OrchestratorTransitionAnchor | null;
  toAnchor?: OrchestratorTransitionAnchor | null;
  trigger: OrchestratorTransitionTrigger;
  resultMode: OrchestratorTransitionResultMode;
  promptTemplate?: string | null;
};

export type OrchestratorTemplateDraft = {
  name: string;
  description: string;
  sessions: OrchestratorSessionTemplate[];
  transitions: OrchestratorTemplateTransition[];
};

export type OrchestratorTemplate = OrchestratorTemplateDraft & {
  id: string;
  createdAt: string;
  updatedAt: string;
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
  agentCommandsRevision?: number;
  codexThreadState?: CodexThreadState | null;
  status: SessionStatus;
  preview: string;
  messages: Message[];
  pendingPrompts?: PendingPrompt[];
};

export type CodexThreadState = "active" | "archived";

export type Message =
  | TextMessage
  | ThinkingMessage
  | CommandMessage
  | DiffMessage
  | MarkdownMessage
  | ParallelAgentsMessage
  | SubagentResultMessage
  | ApprovalMessage
  | UserInputRequestMessage
  | McpElicitationRequestMessage
  | CodexAppRequestMessage;

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
  changeSetId?: string | null;
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

export type ParallelAgentStatus = "initializing" | "running" | "completed" | "error";

export type ParallelAgentProgress = {
  detail?: string | null;
  id: string;
  status: ParallelAgentStatus;
  title: string;
};

export type ParallelAgentsMessage = BaseMessage & {
  type: "parallelAgents";
  agents: ParallelAgentProgress[];
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

export type UserInputQuestionOption = {
  description: string;
  label: string;
};

export type UserInputQuestion = {
  header: string;
  id: string;
  isOther?: boolean;
  isSecret?: boolean;
  options?: UserInputQuestionOption[] | null;
  question: string;
};

export type InteractionRequestState = "pending" | "submitted" | "interrupted" | "canceled";

export type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue | undefined };

export type UserInputRequestMessage = BaseMessage & {
  type: "userInputRequest";
  title: string;
  detail: string;
  questions: UserInputQuestion[];
  state: InteractionRequestState;
  submittedAnswers?: Record<string, string[]> | null;
};

export type McpElicitationAction = "accept" | "decline" | "cancel";

export type McpElicitationConstOption = {
  const: string;
  title: string;
};

export type McpElicitationStringSchema = {
  type: "string";
  title?: string | null;
  description?: string | null;
  default?: string | null;
  enum?: string[] | null;
  enumNames?: string[] | null;
  oneOf?: McpElicitationConstOption[] | null;
  minLength?: number | null;
  maxLength?: number | null;
};

export type McpElicitationNumberSchema = {
  type: "number" | "integer";
  title?: string | null;
  description?: string | null;
  default?: number | null;
  minimum?: number | null;
  maximum?: number | null;
};

export type McpElicitationBooleanSchema = {
  type: "boolean";
  title?: string | null;
  description?: string | null;
  default?: boolean | null;
};

export type McpElicitationArrayItems = {
  type?: "string";
  enum?: string[] | null;
  anyOf?: McpElicitationConstOption[] | null;
};

export type McpElicitationArraySchema = {
  type: "array";
  title?: string | null;
  description?: string | null;
  default?: string[] | null;
  items: McpElicitationArrayItems;
  minItems?: number | null;
  maxItems?: number | null;
};

export type McpElicitationPrimitiveSchema =
  | McpElicitationStringSchema
  | McpElicitationNumberSchema
  | McpElicitationBooleanSchema
  | McpElicitationArraySchema;

export type McpElicitationSchema = {
  $schema?: string | null;
  type: "object";
  properties: Record<string, McpElicitationPrimitiveSchema | undefined>;
  required?: string[] | null;
};

export type McpElicitationRequestPayload = {
  threadId: string;
  turnId?: string | null;
  serverName: string;
} & (
  | {
      mode: "form";
      _meta?: JsonValue | null;
      message: string;
      requestedSchema: McpElicitationSchema;
    }
  | {
      mode: "url";
      _meta?: JsonValue | null;
      elicitationId: string;
      message: string;
      url: string;
    }
);

export type McpElicitationRequestMessage = BaseMessage & {
  type: "mcpElicitationRequest";
  title: string;
  detail: string;
  request: McpElicitationRequestPayload;
  state: InteractionRequestState;
  submittedAction?: McpElicitationAction | null;
  submittedContent?: JsonValue | null;
};

export type CodexAppRequestMessage = BaseMessage & {
  type: "codexAppRequest";
  title: string;
  detail: string;
  method: string;
  params: JsonValue;
  state: InteractionRequestState;
  submittedResult?: JsonValue | null;
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

export type TextReplaceEvent = {
  type: "textReplace";
  revision: number;
  sessionId: string;
  messageId: string;
  messageIndex: number;
  text: string;
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

export type ParallelAgentsUpdateEvent = {
  type: "parallelAgentsUpdate";
  revision: number;
  sessionId: string;
  messageId: string;
  messageIndex: number;
  agents: ParallelAgentProgress[];
  preview: string;
};

export type DeltaEvent =
  | MessageCreatedEvent
  | TextDeltaEvent
  | TextReplaceEvent
  | CommandUpdateEvent
  | ParallelAgentsUpdateEvent;
