export type AgentType = "Claude" | "Codex" | "Cursor" | "Gemini";
export type ExhaustiveValueCoverage<
  Union extends string,
  Options extends ReadonlyArray<{ value: Union }>,
> = Exclude<Union, Options[number]["value"]> extends never ? true : never;
export type SessionStatus = "active" | "idle" | "approval" | "error";
export type SandboxMode =
  | "read-only"
  | "workspace-write"
  | "danger-full-access";
export type ApprovalPolicy =
  | "untrusted"
  | "on-failure"
  | "on-request"
  | "never";
export type ClaudeApprovalMode = "ask" | "auto-approve" | "plan";
export type ClaudeEffortLevel = "default" | "low" | "medium" | "high" | "max";
export type CodexReasoningEffort =
  | "none"
  | "minimal"
  | "low"
  | "medium"
  | "high"
  | "xhigh";
export type CursorMode = "agent" | "plan" | "ask";
export type GeminiApprovalMode = "default" | "auto_edit" | "yolo" | "plan";
export type AgentReadinessStatus = "ready" | "missing" | "needsSetup";

export type AgentReadiness = {
  agent: AgentType;
  status: AgentReadinessStatus;
  blocking: boolean;
  detail: string;
  warningDetail?: string | null;
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

export type ConversationMarkerKind =
  | "checkpoint"
  | "decision"
  | "review"
  | "bug"
  | "question"
  | "handoff"
  | "custom";

export type ConversationMarkerAuthor = "user" | "agent" | "system";

export type ConversationMarker = {
  id: string;
  sessionId: string;
  kind: ConversationMarkerKind;
  name: string;
  body?: string | null;
  color: string;
  messageId: string;
  messageIndexHint: number;
  endMessageId?: string | null;
  endMessageIndexHint?: number | null;
  createdAt: string;
  updatedAt: string;
  createdBy: ConversationMarkerAuthor;
};

export type CreateConversationMarkerOptions = {
  name?: string;
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

export type CodexNoticeKind =
  | "configWarning"
  | "deprecationNotice"
  | "runtimeNotice";
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
  defaultCodexModel: string;
  defaultClaudeModel: string;
  defaultCursorModel: string;
  defaultGeminiModel: string;
  defaultCodexReasoningEffort: CodexReasoningEffort;
  defaultClaudeApprovalMode: ClaudeApprovalMode;
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
  inputMode: OrchestratorSessionInputMode;
  position: OrchestratorNodePosition;
};

export type OrchestratorSessionInputMode = "queue" | "consolidate";

export type OrchestratorTransitionTrigger = "onCompletion";
export type OrchestratorTransitionResultMode =
  | "none"
  | "lastResponse"
  | "summary"
  | "summaryAndLastResponse";

export type OrchestratorTransitionAnchor =
  | "top"
  | "top-right"
  | "right"
  | "bottom-right"
  | "bottom"
  | "bottom-left"
  | "left"
  | "top-left";

export type OrchestratorTemplateTransition = {
  id: string;
  fromSessionId: string;
  toSessionId: string;
  fromAnchor?: OrchestratorTransitionAnchor;
  toAnchor?: OrchestratorTransitionAnchor;
  trigger: OrchestratorTransitionTrigger;
  resultMode: OrchestratorTransitionResultMode;
  promptTemplate?: string | null;
};

export type OrchestratorTemplateDraft = {
  name: string;
  description: string;
  projectId?: string | null;
  sessions: OrchestratorSessionTemplate[];
  transitions: OrchestratorTemplateTransition[];
};

export type OrchestratorTemplate = OrchestratorTemplateDraft & {
  id: string;
  createdAt: string;
  updatedAt: string;
};

export type OrchestratorInstanceStatus = "running" | "paused" | "stopped";

export type OrchestratorSessionInstance = {
  templateSessionId: string;
  sessionId: string;
  lastCompletionRevision?: number | null;
  lastDeliveredCompletionRevision?: number | null;
};

export type PendingTransition = {
  id: string;
  transitionId: string;
  sourceSessionId: string;
  destinationSessionId: string;
  completionRevision: number;
  renderedPrompt: string;
  createdAt: string;
};

export type OrchestratorInstance = {
  id: string;
  templateId: string;
  projectId: string;
  templateSnapshot: OrchestratorTemplate;
  status: OrchestratorInstanceStatus;
  sessionInstances: OrchestratorSessionInstance[];
  pendingTransitions?: PendingTransition[];
  createdAt: string;
  errorMessage?: string | null;
  completedAt?: string | null;
};

export type Session = {
  id: string;
  name: string;
  emoji: string;
  agent: AgentType;
  workdir: string;
  projectId?: string | null;
  // Non-empty when present; omitted for local sessions. Rust never emits null.
  remoteId?: string;
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
  messageCount?: number | null;
  messagesLoaded?: boolean | null;
  markers?: ConversationMarker[];
  pendingPrompts?: PendingPrompt[];
  sessionMutationStamp?: number | null;
  parentDelegationId?: string | null;
};

export type CodexThreadState = "active" | "archived";

export type DelegationMode = "reviewer" | "explorer" | "worker";
export type DelegationStatus =
  | "queued"
  | "running"
  | "completed"
  | "failed"
  | "canceled";

export type DelegationWritePolicy =
  | { kind: "readOnly" }
  | { kind: "sharedWorktree"; ownedPaths: string[] }
  | {
      kind: "isolatedWorktree";
      ownedPaths: string[];
      worktreePath?: string;
    };

export type DelegationFinding = {
  severity: string;
  file?: string | null;
  line?: number | null;
  message: string;
};

export type DelegationCommandResult = {
  command: string;
  status: string;
};

export type DelegationResult = {
  delegationId: string;
  childSessionId: string;
  status: DelegationStatus;
  summary: string;
  findings?: DelegationFinding[];
  changedFiles?: string[];
  commandsRun?: DelegationCommandResult[];
  notes?: string[];
};

export type DelegationResultSummary = {
  delegationId: string;
  childSessionId: string;
  status: DelegationStatus;
  summary: string;
};

export type DelegationRecord = {
  id: string;
  parentSessionId: string;
  childSessionId: string;
  mode: DelegationMode;
  status: DelegationStatus;
  title: string;
  prompt: string;
  cwd: string;
  agent: AgentType;
  model?: string | null;
  writePolicy: DelegationWritePolicy;
  createdAt: string;
  startedAt?: string | null;
  completedAt?: string | null;
  result?: DelegationResult | null;
};

export type DelegationSummary = Omit<
  DelegationRecord,
  "prompt" | "cwd" | "result"
> & {
  result?: DelegationResultSummary | null;
};

export type DelegationWaitMode = "any" | "all";

export type DelegationWaitRecord = {
  id: string;
  parentSessionId: string;
  delegationIds: string[];
  mode: DelegationWaitMode;
  createdAt: string;
  title?: string | null;
};

export type DelegationWaitConsumedReason =
  | "completed"
  | "parentSessionUnavailable"
  | "parentSessionRemoved";

export type Message =
  | TextMessage
  | ThinkingMessage
  | CommandMessage
  | DiffMessage
  | MarkdownMessage
  | ParallelAgentsMessage
  | FileChangesMessage
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

// Persisted transcript identity fields. If a new field is added here, update
// `hydrationRetainedMessagesMatch` in `app-live-state.ts` (or its extracted
// projection helper) so targeted hydration does not silently treat new persisted
// message data as interchangeable during retained-message comparisons.
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

export type ParallelAgentStatus =
  | "initializing"
  | "running"
  | "completed"
  | "error";

export type ParallelAgentSource = "delegation" | "tool";

export type ParallelAgentProgress = {
  detail?: string | null;
  id: string;
  source: ParallelAgentSource;
  status: ParallelAgentStatus;
  title: string;
};

export type ParallelAgentsMessage = BaseMessage & {
  type: "parallelAgents";
  agents: ParallelAgentProgress[];
};

export type FileChangeSummaryFile = {
  path: string;
  kind: WorkspaceFileChangeKind;
};

export type FileChangesMessage = BaseMessage & {
  type: "fileChanges";
  title: string;
  files: FileChangeSummaryFile[];
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

export type InteractionRequestState =
  | "pending"
  | "submitted"
  | "interrupted"
  | "canceled";

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
  messageCount: number;
  delta: string;
  preview?: string | null;
  sessionMutationStamp?: number | null;
};

export type TextReplaceEvent = {
  type: "textReplace";
  revision: number;
  sessionId: string;
  messageId: string;
  messageIndex: number;
  messageCount: number;
  text: string;
  preview?: string | null;
  sessionMutationStamp?: number | null;
};

export type MessageCreatedEvent = {
  type: "messageCreated";
  revision: number;
  sessionId: string;
  messageId: string;
  messageIndex: number;
  messageCount: number;
  message: Message;
  preview: string;
  status: SessionStatus;
  sessionMutationStamp?: number | null;
};

export type MessageUpdatedEvent = {
  type: "messageUpdated";
  revision: number;
  sessionId: string;
  messageId: string;
  messageIndex: number;
  messageCount: number;
  message: Message;
  preview: string;
  status: SessionStatus;
  sessionMutationStamp?: number | null;
};

export type SessionCreatedEvent = {
  type: "sessionCreated";
  revision: number;
  sessionId: string;
  session: Session;
};

export type CommandUpdateEvent = {
  type: "commandUpdate";
  revision: number;
  sessionId: string;
  messageId: string;
  messageIndex: number;
  messageCount: number;
  command: string;
  commandLanguage?: string | null;
  output: string;
  outputLanguage?: string | null;
  status: "running" | "success" | "error";
  preview: string;
  sessionMutationStamp?: number | null;
};

export type ParallelAgentsUpdateEvent = {
  type: "parallelAgentsUpdate";
  revision: number;
  sessionId: string;
  messageId: string;
  messageIndex: number;
  messageCount: number;
  agents: ParallelAgentProgress[];
  preview: string;
  sessionMutationStamp?: number | null;
};

export type ConversationMarkerCreatedEvent = {
  type: "conversationMarkerCreated";
  revision: number;
  sessionId: string;
  marker: ConversationMarker;
  sessionMutationStamp?: number | null;
};

export type ConversationMarkerUpdatedEvent = {
  type: "conversationMarkerUpdated";
  revision: number;
  sessionId: string;
  marker: ConversationMarker;
  sessionMutationStamp?: number | null;
};

export type ConversationMarkerDeletedEvent = {
  type: "conversationMarkerDeleted";
  revision: number;
  sessionId: string;
  markerId: string;
  sessionMutationStamp?: number | null;
};

export type OrchestratorsUpdatedEvent = {
  type: "orchestratorsUpdated";
  revision: number;
  orchestrators: OrchestratorInstance[];
  sessions?: Session[];
};

export type DelegationCreatedEvent = {
  type: "delegationCreated";
  revision: number;
  delegation: DelegationSummary;
};

export type DelegationWaitCreatedEvent = {
  type: "delegationWaitCreated";
  revision: number;
  wait: DelegationWaitRecord;
};

export type DelegationWaitConsumedEvent = {
  type: "delegationWaitConsumed";
  revision: number;
  waitId: string;
  parentSessionId: string;
  reason: DelegationWaitConsumedReason;
};

export type DelegationWaitResumeDispatchFailedEvent = {
  type: "delegationWaitResumeDispatchFailed";
  revision: number;
  parentSessionId: string;
  error: string;
};

export type DelegationUpdatedEvent = {
  type: "delegationUpdated";
  revision: number;
  delegationId: string;
  status: DelegationStatus;
  updatedAt: string;
};

export type DelegationCompletedEvent = {
  type: "delegationCompleted";
  revision: number;
  delegationId: string;
  result: DelegationResultSummary;
  completedAt: string;
};

export type DelegationFailedEvent = {
  type: "delegationFailed";
  revision: number;
  delegationId: string;
  result: DelegationResultSummary;
  failedAt: string;
};

export type DelegationCanceledEvent = {
  type: "delegationCanceled";
  revision: number;
  delegationId: string;
  canceledAt: string;
  reason?: string | null;
};

export type CodexUpdatedEvent = {
  type: "codexUpdated";
  revision: number;
  codex: CodexState;
};

export type WorkspaceFileChangeKind =
  | "created"
  | "modified"
  | "deleted"
  | "other";

export type WorkspaceFileChange = {
  path: string;
  kind: WorkspaceFileChangeKind;
  rootPath?: string | null;
  sessionId?: string | null;
  mtimeMs?: number | null;
  sizeBytes?: number | null;
};

export type WorkspaceFilesChangedEvent = {
  revision: number;
  changes: WorkspaceFileChange[];
};

export type DeltaEvent =
  | SessionCreatedEvent
  | MessageCreatedEvent
  | MessageUpdatedEvent
  | TextDeltaEvent
  | TextReplaceEvent
  | CommandUpdateEvent
  | ParallelAgentsUpdateEvent
  | ConversationMarkerCreatedEvent
  | ConversationMarkerUpdatedEvent
  | ConversationMarkerDeletedEvent
  | CodexUpdatedEvent
  | OrchestratorsUpdatedEvent
  | DelegationCreatedEvent
  | DelegationWaitCreatedEvent
  | DelegationWaitConsumedEvent
  | DelegationWaitResumeDispatchFailedEvent
  | DelegationUpdatedEvent
  | DelegationCompletedEvent
  | DelegationFailedEvent
  | DelegationCanceledEvent;

export type SessionSettingsField =
  | "model"
  | "sandboxMode"
  | "approvalPolicy"
  | "reasoningEffort"
  | "claudeApprovalMode"
  | "claudeEffort"
  | "cursorMode"
  | "geminiApprovalMode";
export type SessionSettingsValue =
  | string
  | SandboxMode
  | ApprovalPolicy
  | ClaudeEffortLevel
  | CodexReasoningEffort
  | ClaudeApprovalMode
  | CursorMode
  | GeminiApprovalMode;
