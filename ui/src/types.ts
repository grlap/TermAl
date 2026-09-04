export type AgentType = "Claude" | "Codex" | "Cursor" | "Gemini" | "OpenCode";
export type ExhaustiveValueCoverage<
  Union extends string,
  Options extends ReadonlyArray<{ value: Union }>,
> = Exclude<Union, Options[number]["value"]> extends never ? true : never;
export type SessionStatus = "active" | "idle" | "approval" | "stopping" | "error";
export type SandboxMode =
  | "read-only"
  | "workspace-write"
  | "danger-full-access";
export type ApprovalPolicy =
  | "untrusted"
  | "on-failure"
  | "on-request"
  | "auto-approve"
  | "never";
export type ClaudeApprovalMode =
  | "ask"
  | "auto-approve"
  | "plan"
  | "read-only-auto-approve";
export type ClaudeEffortLevel = "default" | "low" | "medium" | "high" | "xhigh" | "max";
export type CodexReasoningEffort =
  | "none"
  | "minimal"
  | "low"
  | "medium"
  | "high"
  | "xhigh"
  | "max"
  | "ultra";
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

export type SessionModelServiceTier = {
  id: string;
  label: string;
  description?: string | null;
};

export type SessionModelOption = {
  label: string;
  value: string;
  description?: string | null;
  badges?: string[];
  supportedClaudeEffortLevels?: ClaudeEffortLevel[];
  defaultReasoningEffort?: CodexReasoningEffort | null;
  supportedReasoningEfforts?: CodexReasoningEffort[];
  serviceTiers?: SessionModelServiceTier[];
};

export type SessionModelOptionsRefreshOutcome =
  | "deferred"
  | "failed"
  | "refreshed"
  | "skipped";

export type SessionModelOptionsRefreshOptions = {
  reportGlobalError?: boolean;
};

export type SessionModelOptionsRefreshRequest = (
  sessionId: string,
  options?: SessionModelOptionsRefreshOptions,
) =>
  | SessionModelOptionsRefreshOutcome
  | Promise<SessionModelOptionsRefreshOutcome | void>
  | void;

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
  defaultCodexSandboxMode?: SandboxMode;
  defaultCodexApprovalPolicy?: ApprovalPolicy;
  defaultClaudeModel: string;
  defaultCursorModel: string;
  defaultGeminiModel: string;
  defaultOpenCodeModel?: string;
  defaultCodexReasoningEffort: CodexReasoningEffort;
  defaultClaudeApprovalMode: ClaudeApprovalMode;
  defaultClaudeEffort: ClaudeEffortLevel;
  remotes?: RemoteConfig[] | null;
  telegram?: TelegramUiConfig | null;
  engram?: EngramHostSettings | null;
};

export type EngramHostSettings = {
  binaryPath: string;
  home: string;
  bootRecoveryBudgetMs: number;
};

export type TelegramUiConfig = {
  enabled?: boolean;
  forwardAssistantReplies?: boolean;
  subscribedProjectIds?: string[];
  defaultProjectId?: string | null;
  defaultSessionId?: string | null;
};

export type Project = {
  id: string;
  name: string;
  rootPath: string;
  remoteId?: string | null;
  engram?: EngramProjectStateSettings | null;
  engramDeclared?: boolean;
  engramOperatorDisabled?: boolean;
  engramCleanupWarning?: string | null;
};

export type EngramProjectSettings = {
  enabled: boolean;
  turnGatedControl?: boolean;
  binaryPath?: string | null;
  home?: string | null;
  deadlineMs?: number | null;
};

export type EngramProjectStateSettings = EngramProjectSettings;

export type EngramProjectVerification = {
  verified: boolean;
  binaryPath: string;
  home: string;
  projectId: string;
  database: string;
  requiredAssurance: string;
  healthy: boolean;
  errors?: string[];
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
  codexFastMode?: boolean;
  sandboxMode?: SandboxMode | null;
  cursorMode?: CursorMode | null;
  claudeApprovalMode?: ClaudeApprovalMode | null;
  geminiApprovalMode?: GeminiApprovalMode | null;
  opencodeModel?: string | null;
  opencodeEffort?: string | null;
  opencodeCurrentEffort?: string | null;
  opencodeEffortOptions?: SessionModelOption[];
  opencodeMode?: string | null;
  opencodeCurrentMode?: string | null;
  opencodeModeOptions?: SessionModelOption[];
  externalSessionId?: string | null;
  agentCommandsRevision?: number;
  codexThreadState?: CodexThreadState | null;
  liveActivity?: SessionLiveActivity | null;
  /** True while TermAl restores this session's Engram authority after restart. */
  engramBootRecoveryPending?: boolean;
  status: SessionStatus;
  preview: string;
  messages: Message[];
  /** Recent user prompts, oldest to newest; supplied by targeted hydration. */
  promptHistory?: string[];
  /** True when a metadata-only projection intentionally omitted promptHistory. */
  promptHistoryRedacted?: boolean;
  messageCount?: number | null;
  messagesLoaded?: boolean | null;
  /** Global index of `messages[0]` inside the bounded transcript. */
  messageStartIndex?: number;
  /** Client-side location of the resident bounded transcript window. */
  hasOlderHistory?: boolean;
  /** Client-side location of the resident bounded transcript window. */
  hasNewerHistory?: boolean;
  markers?: ConversationMarker[];
  pendingPrompts?: PendingPrompt[];
  /** True while queued prompts are parked behind the explicit-resume latch a
   * Stop leaves behind; nothing starts until the user resumes the queue or
   * sends a new prompt. */
  queuePaused?: boolean;
  sessionMutationStamp?: number | null;
  parentDelegationId?: string | null;
};

/** Transcript-free metadata returned by broad state snapshots and summary
 * lifecycle deltas. Targeted session endpoints return `Session` instead. */
export type StateSessionSummary = Omit<
  Session,
  | "messages"
  | "promptHistory"
  | "promptHistoryRedacted"
  | "messagesLoaded"
  | "messageStartIndex"
  | "hasOlderHistory"
  | "hasNewerHistory"
  | "pendingPrompts"
  | "messageCount"
  | "queuePaused"
> & {
  messageCount: number;
  queuePaused: boolean;
};

export type SessionLiveActivity = {
  prompt: string;
  command?: string | null;
  commandStatus?: "running" | "success" | "error" | null;
};

export type CodexThreadState = "active" | "archived";

export type CodexMcpToolSummary = {
  name: string;
  title?: string | null;
  description?: string | null;
};

export type CodexMcpServerStatus = {
  name: string;
  authStatus: string;
  tools: CodexMcpToolSummary[];
};

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
  reviewResultRequired: boolean;
  postSubmissionTransportError?: string | null;
  reviewResultRecoveryError?: string | null;
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
  | "parentSessionStopped"
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
  | EngramControlMessage
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

// Identity of the peer session that authored a message delivered via
// `termal_send_to_session`. Absent for ordinary human/agent messages. The name
// is resolved backend-side, so it is safe to render directly as the author
// label.
export type MessageSource = {
  sessionId?: string | null;
  name: string;
  kind?: "peer" | "peerBatch" | "mailbox";
  mailbox?: MailboxMessageSource | null;
};

export type MailboxMessageSource = {
  mailboxId: string;
  messageId: string;
  sequence: number;
  unreadCount: number;
};

export type MailboxParticipant = {
  sessionId: string;
  displayName: string;
  processedThrough: number;
  leftAt?: string | null;
};

export type MailboxSummary = {
  id: string;
  participants: MailboxParticipant[];
  latestSequence: number;
  unreadCount: number;
  latestMessagePreview?: string | null;
  latestMessageAt?: string | null;
};

export type MailboxMessage = {
  id: string;
  mailboxId: string;
  sequence: number;
  senderSessionId: string;
  senderName: string;
  targetSessionId: string;
  targetName: string;
  createdAt: string;
  class: "routine";
  topic?: string | null;
  stateStamp?: string | null;
  body: string;
  notificationState: string;
};

export type PendingPrompt = {
  id: string;
  timestamp: string;
  text: string;
  expandedText?: string | null;
  attachments?: ImageAttachment[];
  localOnly?: boolean;
  /** Global transcript end observed when this in-memory optimistic send was queued. */
  transcriptEndIndexAtEnqueue?: number;
  source?: MessageSource | null;
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
  // Present when this text was delivered from another session via
  // `termal_send_to_session`; drives the sender label instead of "You".
  source?: MessageSource | null;
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

export type EngramControlStage = "dispatch" | "checkpoint" | "restart";
export type EngramControlDecision =
  | "grant"
  | "defer"
  | "refuse"
  | "degraded";
export type EngramControlDispatch =
  | "sent_on_grant"
  | "sent_without_grant"
  | "queued";
export type EngramControlFailMode = "enforced" | "shadow" | "degraded";
export type EngramControlDirective = {
  directiveId: string;
  kind: string;
  audience: string;
  satisfaction: string;
};
export type EngramControlMessage = BaseMessage & {
  type: "engramControl";
  schemaVersion: number;
  stage: EngramControlStage;
  assurance: string;
  decision: EngramControlDecision;
  dispatch: EngramControlDispatch;
  refusalCode?: string | null;
  deferCode?: string | null;
  grantId?: string | null;
  directives?: EngramControlDirective[];
  deliveredRange?: { from: number; to: number; head: number } | null;
  latencyMs: {
    evaluate?: number | null;
    begin?: number | null;
    checkpoint?: number | null;
    total: number;
  };
  failMode: EngramControlFailMode;
  repairArmed?: boolean;
  nextIntent?: "continue" | "wait" | "exit" | null;
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
  supportedDecisions?: ApprovalDecision[] | null;
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
  multiSelect?: boolean;
  options?: UserInputQuestionOption[] | null;
  question: string;
};

export type InteractionRequestState =
  | "pending"
  | "submitted"
  | "interrupted"
  | "canceled"
  /** Resolved without answers: either the user skipped a declinable card or
   * TermAl self-resolved an unattended question. Distinct from "canceled",
   * which is an agent- or turn-side cancel. */
  | "declined";

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
  /**
   * Whether the card offers a Skip action that resolves the request without
   * answers. Only Claude questions that arrived over the permission channel
   * are declinable; skipping sends a deny telling Claude to decide alone.
   * Always present — the backend serializes it unconditionally.
   */
  declinable: boolean;
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
  textStartByte: number;
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
  session: StateSessionSummary;
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
  sessions?: StateSessionSummary[];
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
  | "codexFastMode"
  | "claudeApprovalMode"
  | "claudeEffort"
  | "cursorMode"
  | "geminiApprovalMode"
  | "opencodeEffort"
  | "opencodeMode";
export type SessionSettingsValue =
  | string
  | SandboxMode
  | ApprovalPolicy
  | ClaudeEffortLevel
  | CodexReasoningEffort
  | ClaudeApprovalMode
  | CursorMode
  | GeminiApprovalMode;

// Coordination board: level-triggered per-project facts. Read-only in the UI;
// agents write via MCP/HTTP, humans observe.
export type BoardEntry = {
  key: string;
  revision: number;
  updatedAtGeneration: number;
  value: unknown;
  deleted: boolean;
  authorSessionId: string;
  authorName: string;
  updatedAt: string;
  stateStamp?: string | null;
};

export type BoardListPage = {
  generation: number;
  entries: BoardEntry[];
  nextAfterKey?: string | null;
  unchanged: boolean;
};
