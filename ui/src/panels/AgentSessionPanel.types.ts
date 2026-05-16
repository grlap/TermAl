// Owns: public prop and handler types for AgentSessionPanel surfaces.
// Does not own: session rendering, composer behavior, or virtualization logic.
// Split from: ui/src/panels/AgentSessionPanel.tsx.

import type {
  ClipboardEvent as ReactClipboardEvent,
  JSX,
  RefObject,
} from "react";
import type {
  ApprovalDecision,
  AgentCommand,
  ApprovalPolicy,
  ClaudeApprovalMode,
  ClaudeEffortLevel,
  CodexReasoningEffort,
  CommandMessage,
  ConversationMarker,
  CreateConversationMarkerOptions,
  CursorMode,
  DiffMessage,
  GeminiApprovalMode,
  ImageAttachment,
  JsonValue,
  McpElicitationAction,
  SandboxMode,
  Session,
} from "../types";
import type { PaneViewMode } from "../workspace";
import type {
  CodexAppRequestSubmitHandler,
  McpElicitationSubmitHandler,
  RenderMessageCard,
  UserInputSubmitHandler,
} from "./VirtualizedConversationMessageList";
import type { SpawnDelegationOptions } from "./agent-session-panel-helpers";

export type WaitingIndicatorKind = "liveTurn" | "delegationWait" | "send";

export type DraftImageAttachment = ImageAttachment & {
  base64Data: string;
  id: string;
  previewUrl: string;
};

export type PromptHistoryState = {
  index: number;
  draft: string;
};

export type AgentCommandResolverErrorState = {
  message: string;
  sessionId: string;
};

export type CreateConversationMarkerHandlerResult =
  | boolean
  | void
  | Promise<boolean | void>;

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

export type SpawnDelegationHandler = (
  sessionId: string,
  prompt: string,
  options?: SpawnDelegationOptions,
) => Promise<boolean>;

export type AgentSessionPanelProps = {
  paneId: string;
  viewMode: PaneViewMode;
  activeSessionId: string | null;
  liveTailPinned?: boolean;
  isLoading: boolean;
  isUpdating: boolean;
  showWaitingIndicator: boolean;
  waitingIndicatorKind?: WaitingIndicatorKind;
  waitingIndicatorPrompt: string | null;
  commandMessages: CommandMessage[];
  diffMessages: DiffMessage[];
  scrollContainerRef: RefObject<HTMLElement | null>;
  onApprovalDecision: (
    sessionId: string,
    messageId: string,
    decision: ApprovalDecision,
  ) => void;
  onUserInputSubmit: UserInputSubmitHandler;
  onMcpElicitationSubmit: McpElicitationSubmitHandler;
  onCodexAppRequestSubmit: CodexAppRequestSubmitHandler;
  onCancelQueuedPrompt: (sessionId: string, promptId: string) => void;
  onCreateConversationMarker?: (
    sessionId: string,
    messageId: string,
    options?: CreateConversationMarkerOptions,
  ) => CreateConversationMarkerHandlerResult;
  onDeleteConversationMarker?: (sessionId: string, markerId: string) => void;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
  conversationSearchQuery: string;
  conversationSearchMatchedItemKeys: ReadonlySet<string>;
  conversationSearchActiveItemKey: string | null;
  onConversationSearchItemMount: (
    itemKey: string,
    node: HTMLElement | null,
  ) => void;
  renderCommandCard: (message: CommandMessage) => JSX.Element | null;
  renderDiffCard: (message: DiffMessage) => JSX.Element | null;
  renderMessageCard: RenderMessageCard;
  renderPromptSettings: (
    paneId: string,
    session: Session,
    isUpdating: boolean,
    onSessionSettingsChange: (
      sessionId: string,
      field: SessionSettingsField,
      value: SessionSettingsValue,
    ) => void,
  ) => JSX.Element | null;
};

export type AgentSessionPanelFooterProps = {
  paneId: string;
  viewMode: PaneViewMode;
  isPaneActive: boolean;
  activeSessionId: string | null;
  formatByteSize: (byteSize: number) => string;
  isSending: boolean;
  isStopping: boolean;
  isSessionBusy: boolean;
  isUpdating: boolean;
  showNewResponseIndicator: boolean;
  newResponseIndicatorLabel: string;
  footerModeLabel: string;
  onScrollToLatest: () => void;
  onDraftCommit: (sessionId: string, nextValue: string) => void;
  onDraftAttachmentRemove: (sessionId: string, attachmentId: string) => void;
  isRefreshingModelOptions: boolean;
  modelOptionsError: string | null;
  agentCommands: AgentCommand[];
  hasLoadedAgentCommands: boolean;
  isRefreshingAgentCommands: boolean;
  agentCommandsError: string | null;
  onRefreshSessionModelOptions: (sessionId: string) => void;
  onRefreshAgentCommands: (sessionId: string) => void;
  onSend: (
    sessionId: string,
    draftText?: string,
    expandedText?: string | null,
  ) => boolean;
  canSpawnDelegation?: boolean;
  onSpawnDelegation?: SpawnDelegationHandler;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
  onStopSession: (sessionId: string) => void;
  onPaste: (event: ReactClipboardEvent<HTMLTextAreaElement>) => void;
};

export type SessionConversationPageProps = {
  renderMessageCard: RenderMessageCard;
  session: Session;
  liveTailPinned: boolean;
  scrollContainerRef: RefObject<HTMLElement | null>;
  isActive: boolean;
  isLoading: boolean;
  showWaitingIndicator: boolean;
  waitingIndicatorKind: WaitingIndicatorKind;
  waitingIndicatorPrompt: string | null;
  onApprovalDecision: (
    sessionId: string,
    messageId: string,
    decision: ApprovalDecision,
  ) => void;
  onUserInputSubmit: UserInputSubmitHandler;
  onMcpElicitationSubmit: McpElicitationSubmitHandler;
  onCodexAppRequestSubmit: CodexAppRequestSubmitHandler;
  onCancelQueuedPrompt: (sessionId: string, promptId: string) => void;
  onCreateConversationMarker: (
    sessionId: string,
    messageId: string,
    options?: CreateConversationMarkerOptions,
  ) => CreateConversationMarkerHandlerResult;
  onDeleteConversationMarker: (sessionId: string, markerId: string) => void;
  conversationSearchQuery: string;
  conversationSearchMatchedItemKeys: ReadonlySet<string>;
  conversationSearchActiveItemKey: string | null;
  onConversationSearchItemMount: (
    itemKey: string,
    node: HTMLElement | null,
  ) => void;
};
