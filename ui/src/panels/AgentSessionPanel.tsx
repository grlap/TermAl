import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ClipboardEvent as ReactClipboardEvent,
  type ReactNode,
  type RefObject,
} from "react";
import { ExpandedPromptPanel } from "../ExpandedPromptPanel";
import {
  buildSlashPaletteState,
  normalizedAgentCommandKind,
  parseAgentCommandDraft,
  supportsAgentSlashCommands,
  supportsLiveSessionModelOptions,
  type SlashPaletteItem,
} from "./session-slash-palette";
import {
  MessageSlot,
  PanelEmptyState,
  collectUserPromptHistory,
} from "./session-message-leaves";
import {
  PendingPromptCard,
  RunningIndicator,
} from "./session-activity-cards";
import {
  EMPTY_MATCHED_ITEM_KEYS,
  VirtualizedConversationMessageList,
  type CodexAppRequestSubmitHandler,
  type McpElicitationSubmitHandler,
  type RenderMessageCard,
  type UserInputSubmitHandler,
} from "./VirtualizedConversationMessageList";
import {
  renderHighlightedText,
  type SearchHighlightTone,
} from "../search-highlight";
import type {
  ApprovalDecision,
  AgentCommand,
  ApprovalPolicy,
  ClaudeApprovalMode,
  ClaudeEffortLevel,
  CommandMessage,
  CodexReasoningEffort,
  CursorMode,
  DiffMessage,
  GeminiApprovalMode,
  ImageAttachment,
  JsonValue,
  Message,
  McpElicitationAction,
  PendingPrompt,
  SandboxMode,
  Session,
} from "../types";
import type { PaneViewMode } from "../workspace";

type DraftImageAttachment = ImageAttachment & {
  base64Data: string;
  id: string;
  previewUrl: string;
};

type PromptHistoryState = {
  index: number;
  draft: string;
};

type SessionSettingsField =
  | "model"
  | "sandboxMode"
  | "approvalPolicy"
  | "reasoningEffort"
  | "claudeApprovalMode"
  | "claudeEffort"
  | "cursorMode"
  | "geminiApprovalMode";
type SessionSettingsValue =
  | string
  | SandboxMode
  | ApprovalPolicy
  | ClaudeEffortLevel
  | CodexReasoningEffort
  | ClaudeApprovalMode
  | CursorMode
  | GeminiApprovalMode;

const CONVERSATION_VIRTUALIZATION_MIN_MESSAGES = 80;

function isSpaceKey(event: {
  key: string;
  code?: string;
  keyCode?: number;
  which?: number;
}) {
  return (
    event.key === " " ||
    event.key === "Space" ||
    event.key === "Spacebar" ||
    event.code === "Space" ||
    event.keyCode === 32 ||
    event.which === 32
  );
}


export function AgentSessionPanel({
  paneId,
  viewMode,
  activeSession,
  isLoading,
  isUpdating,
  showWaitingIndicator,
  waitingIndicatorPrompt,
  mountedSessions,
  commandMessages,
  diffMessages,
  scrollContainerRef,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
  onCancelQueuedPrompt,
  onSessionSettingsChange,
  conversationSearchQuery,
  conversationSearchMatchedItemKeys,
  conversationSearchActiveItemKey,
  onConversationSearchItemMount,
  renderCommandCard,
  renderDiffCard,
  renderMessageCard,
  renderPromptSettings,
}: {
  paneId: string;
  viewMode: PaneViewMode;
  activeSession: Session | null;
  isLoading: boolean;
  isUpdating: boolean;
  showWaitingIndicator: boolean;
  waitingIndicatorPrompt: string | null;
  mountedSessions: Session[];
  commandMessages: CommandMessage[];
  diffMessages: DiffMessage[];
  scrollContainerRef: RefObject<HTMLElement | null>;
  onApprovalDecision: (sessionId: string, messageId: string, decision: ApprovalDecision) => void;
  onUserInputSubmit: UserInputSubmitHandler;
  onMcpElicitationSubmit: McpElicitationSubmitHandler;
  onCodexAppRequestSubmit: CodexAppRequestSubmitHandler;
  onCancelQueuedPrompt: (sessionId: string, promptId: string) => void;
  onSessionSettingsChange: (
      sessionId: string,
      field: SessionSettingsField,
      value: SessionSettingsValue,
    ) => void;
  conversationSearchQuery: string;
  conversationSearchMatchedItemKeys: ReadonlySet<string>;
  conversationSearchActiveItemKey: string | null;
  onConversationSearchItemMount: (itemKey: string, node: HTMLElement | null) => void;
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
}): JSX.Element {
  return (
    <SessionBody
      paneId={paneId}
      viewMode={viewMode}
      scrollContainerRef={scrollContainerRef}
      activeSession={activeSession}
      isLoading={isLoading}
      isUpdating={isUpdating}
      showWaitingIndicator={showWaitingIndicator}
      waitingIndicatorPrompt={waitingIndicatorPrompt}
      mountedSessions={mountedSessions}
      commandMessages={commandMessages}
      diffMessages={diffMessages}
      onApprovalDecision={onApprovalDecision}
      onUserInputSubmit={onUserInputSubmit}
      onMcpElicitationSubmit={onMcpElicitationSubmit}
      onCodexAppRequestSubmit={onCodexAppRequestSubmit}
      onCancelQueuedPrompt={onCancelQueuedPrompt}
      onSessionSettingsChange={onSessionSettingsChange}
      conversationSearchQuery={conversationSearchQuery}
      conversationSearchMatchedItemKeys={conversationSearchMatchedItemKeys}
      conversationSearchActiveItemKey={conversationSearchActiveItemKey}
      onConversationSearchItemMount={onConversationSearchItemMount}
      renderCommandCard={renderCommandCard}
      renderDiffCard={renderDiffCard}
      renderMessageCard={renderMessageCard}
      renderPromptSettings={renderPromptSettings}
    />
  );
}

export function AgentSessionPanelFooter({
  paneId,
  viewMode,
  isPaneActive,
  activeSession,
  committedDraft,
  draftAttachments,
  formatByteSize,
  isSending,
  isStopping,
  isSessionBusy,
  isUpdating,
  showNewResponseIndicator,
  footerModeLabel,
  onScrollToLatest,
  onDraftCommit,
  onDraftAttachmentRemove,
  isRefreshingModelOptions,
  modelOptionsError,
  agentCommands,
  hasLoadedAgentCommands,
  isRefreshingAgentCommands,
  agentCommandsError,
  onRefreshSessionModelOptions,
  onRefreshAgentCommands,
  onSend,
  onSessionSettingsChange,
  onStopSession,
  onPaste,
}: {
  paneId: string;
  viewMode: PaneViewMode;
  isPaneActive: boolean;
  activeSession: Session | null;
  committedDraft: string;
  draftAttachments: DraftImageAttachment[];
  formatByteSize: (byteSize: number) => string;
  isSending: boolean;
  isStopping: boolean;
  isSessionBusy: boolean;
  isUpdating: boolean;
  showNewResponseIndicator: boolean;
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
  onSend: (sessionId: string, draftText?: string, expandedText?: string | null) => boolean;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
  onStopSession: (sessionId: string) => void;
  onPaste: (event: ReactClipboardEvent<HTMLTextAreaElement>) => void;
}): JSX.Element {
  if (viewMode === "session") {
    return (
      <SessionComposer
        paneId={paneId}
        isPaneActive={isPaneActive}
        session={activeSession}
        committedDraft={committedDraft}
        draftAttachments={draftAttachments}
        formatByteSize={formatByteSize}
        isSending={isSending}
        isStopping={isStopping}
        isSessionBusy={isSessionBusy}
        isUpdating={isUpdating}
        showNewResponseIndicator={showNewResponseIndicator}
        onScrollToLatest={onScrollToLatest}
        onDraftCommit={onDraftCommit}
        onDraftAttachmentRemove={onDraftAttachmentRemove}
        isRefreshingModelOptions={isRefreshingModelOptions}
        modelOptionsError={modelOptionsError}
        agentCommands={agentCommands}
        hasLoadedAgentCommands={hasLoadedAgentCommands}
        isRefreshingAgentCommands={isRefreshingAgentCommands}
        agentCommandsError={agentCommandsError}
        onRefreshSessionModelOptions={onRefreshSessionModelOptions}
        onRefreshAgentCommands={onRefreshAgentCommands}
        onSend={onSend}
        onSessionSettingsChange={onSessionSettingsChange}
        onStopSession={onStopSession}
        onPaste={onPaste}
      />
    );
  }

  return (
    <footer className="pane-footer-note">
      <p className="composer-hint">
        This tile is in {footerModeLabel.toLowerCase()} mode. Use the Session tab to send prompts.
      </p>
    </footer>
  );
}

const SessionBody = memo(function SessionBody({
  paneId,
  viewMode,
  scrollContainerRef,
  activeSession,
  isLoading,
  isUpdating,
  showWaitingIndicator,
  waitingIndicatorPrompt,
  mountedSessions,
  commandMessages,
  diffMessages,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
  onCancelQueuedPrompt,
  onSessionSettingsChange,
  conversationSearchQuery,
  conversationSearchMatchedItemKeys,
  conversationSearchActiveItemKey,
  onConversationSearchItemMount,
  renderCommandCard,
  renderDiffCard,
  renderMessageCard,
  renderPromptSettings,
}: {
  paneId: string;
  viewMode: PaneViewMode;
  scrollContainerRef: RefObject<HTMLElement | null>;
  activeSession: Session | null;
  isLoading: boolean;
  isUpdating: boolean;
  showWaitingIndicator: boolean;
  waitingIndicatorPrompt: string | null;
  mountedSessions: Session[];
  commandMessages: CommandMessage[];
  diffMessages: DiffMessage[];
  onApprovalDecision: (sessionId: string, messageId: string, decision: ApprovalDecision) => void;
  onUserInputSubmit: UserInputSubmitHandler;
  onMcpElicitationSubmit: McpElicitationSubmitHandler;
  onCodexAppRequestSubmit: CodexAppRequestSubmitHandler;
  onCancelQueuedPrompt: (sessionId: string, promptId: string) => void;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
  conversationSearchQuery: string;
  conversationSearchMatchedItemKeys: ReadonlySet<string>;
  conversationSearchActiveItemKey: string | null;
  onConversationSearchItemMount: (itemKey: string, node: HTMLElement | null) => void;
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
}): JSX.Element | null {
  // Stabilize render callbacks so children receive a constant function identity.
  // The latest version is always called through the ref, so closures stay fresh
  // even though the wrapper identity never changes.
  const renderMessageCardRef = useRef(renderMessageCard);
  renderMessageCardRef.current = renderMessageCard;
  const stableRenderMessageCard = useCallback<RenderMessageCard>(
    (...args) => renderMessageCardRef.current(...args),
    [],
  );

  const renderCommandCardRef = useRef(renderCommandCard);
  renderCommandCardRef.current = renderCommandCard;
  const stableRenderCommandCard = useCallback(
    (message: CommandMessage) => renderCommandCardRef.current(message),
    [],
  );

  const renderDiffCardRef = useRef(renderDiffCard);
  renderDiffCardRef.current = renderDiffCard;
  const stableRenderDiffCard = useCallback(
    (message: DiffMessage) => renderDiffCardRef.current(message),
    [],
  );

  const renderPromptSettingsRef = useRef(renderPromptSettings);
  renderPromptSettingsRef.current = renderPromptSettings;

  if (!activeSession) {
    return (
      <PanelEmptyState
        title="Ready for a session"
        body="Click a session on the left to open it in the active tile."
      />
    );
  }

  if (viewMode === "session") {
    const activePendingPrompts = activeSession.pendingPrompts ?? [];
    if (activeSession.messages.length === 0 && activePendingPrompts.length === 0 && !showWaitingIndicator) {
      return (
        <PanelEmptyState
          title={isLoading ? "Connecting to backend" : "Live session is ready"}
          body={
            isLoading
              ? "Fetching session state from the Rust backend."
              : `Send a prompt to ${activeSession.agent} and this tile will fill with live cards.`
          }
        />
      );
    }

    return (
      <>
        {mountedSessions.map((session) => (
          <SessionConversationPage
            key={session.id}
            renderMessageCard={stableRenderMessageCard}
            session={session}
            scrollContainerRef={scrollContainerRef}
            isActive={session.id === activeSession.id}
            isLoading={isLoading && session.id === activeSession.id}
            showWaitingIndicator={showWaitingIndicator && session.id === activeSession.id}
            waitingIndicatorPrompt={session.id === activeSession.id ? waitingIndicatorPrompt : null}
            onApprovalDecision={onApprovalDecision}
            onUserInputSubmit={onUserInputSubmit}
            onMcpElicitationSubmit={onMcpElicitationSubmit}
            onCodexAppRequestSubmit={onCodexAppRequestSubmit}
            onCancelQueuedPrompt={onCancelQueuedPrompt}
            conversationSearchQuery={session.id === activeSession.id ? conversationSearchQuery : ""}
            conversationSearchMatchedItemKeys={
              session.id === activeSession.id ? conversationSearchMatchedItemKeys : EMPTY_MATCHED_ITEM_KEYS
            }
            conversationSearchActiveItemKey={
              session.id === activeSession.id ? conversationSearchActiveItemKey : null
            }
            onConversationSearchItemMount={onConversationSearchItemMount}
          />
        ))}
      </>
    );
  }

  if (viewMode === "prompt") {
    return renderPromptSettingsRef.current(paneId, activeSession, isUpdating, onSessionSettingsChange) ?? (
      <PanelEmptyState
        title="No prompt settings"
        body="Prompt controls are only available for supported agent sessions."
      />
    );
  }

  if (viewMode === "commands") {
    return commandMessages.length > 0 ? (
      <>
        {commandMessages.map((message) => (
          <MessageSlot key={message.id}>{stableRenderCommandCard(message)}</MessageSlot>
        ))}
      </>
    ) : (
      <PanelEmptyState
        title="No commands yet"
        body="This tile is filtered to command executions. Send a prompt that runs tools and they will show up here."
      />
    );
  }

  if (viewMode === "diffs") {
    return diffMessages.length > 0 ? (
      <>
        {diffMessages.map((message) => (
          <MessageSlot key={message.id}>{stableRenderDiffCard(message)}</MessageSlot>
        ))}
      </>
    ) : (
      <PanelEmptyState
        title="No diffs yet"
        body="This tile is filtered to file changes. When the agent edits or creates files, the diffs will appear here."
      />
    );
  }

  return null;
}, (previous, next) =>
  previous.paneId === next.paneId &&
  previous.viewMode === next.viewMode &&
  previous.scrollContainerRef === next.scrollContainerRef &&
  previous.activeSession === next.activeSession &&
  previous.isLoading === next.isLoading &&
  previous.isUpdating === next.isUpdating &&
  previous.showWaitingIndicator === next.showWaitingIndicator &&
  previous.waitingIndicatorPrompt === next.waitingIndicatorPrompt &&
  previous.mountedSessions === next.mountedSessions &&
  previous.commandMessages === next.commandMessages &&
  previous.diffMessages === next.diffMessages &&
  previous.onUserInputSubmit === next.onUserInputSubmit &&
  previous.onMcpElicitationSubmit === next.onMcpElicitationSubmit &&
  previous.conversationSearchQuery === next.conversationSearchQuery &&
  previous.conversationSearchMatchedItemKeys === next.conversationSearchMatchedItemKeys &&
  previous.conversationSearchActiveItemKey === next.conversationSearchActiveItemKey &&
  previous.onConversationSearchItemMount === next.onConversationSearchItemMount
  // Render callbacks (renderMessageCard, renderDiffCard, renderCommandCard,
  // renderPromptSettings) are intentionally excluded — they are inline closures
  // whose identity changes every render. SessionBody wraps them in stable refs
  // so children always call the latest version without triggering re-renders.
);

const SessionConversationPage = memo(function SessionConversationPage({
  renderMessageCard,
  session,
  scrollContainerRef,
  isActive,
  isLoading,
  showWaitingIndicator,
  waitingIndicatorPrompt,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
  onCancelQueuedPrompt,
  conversationSearchQuery,
  conversationSearchMatchedItemKeys,
  conversationSearchActiveItemKey,
  onConversationSearchItemMount,
}: {
  renderMessageCard: RenderMessageCard;
  session: Session;
  scrollContainerRef: RefObject<HTMLElement | null>;
  isActive: boolean;
  isLoading: boolean;
  showWaitingIndicator: boolean;
  waitingIndicatorPrompt: string | null;
  onApprovalDecision: (sessionId: string, messageId: string, decision: ApprovalDecision) => void;
  onUserInputSubmit: UserInputSubmitHandler;
  onMcpElicitationSubmit: McpElicitationSubmitHandler;
  onCodexAppRequestSubmit: CodexAppRequestSubmitHandler;
  onCancelQueuedPrompt: (sessionId: string, promptId: string) => void;
  conversationSearchQuery: string;
  conversationSearchMatchedItemKeys: ReadonlySet<string>;
  conversationSearchActiveItemKey: string | null;
  onConversationSearchItemMount: (itemKey: string, node: HTMLElement | null) => void;
}) {
  const pendingPrompts = session.pendingPrompts ?? [];

  if (session.messages.length === 0 && pendingPrompts.length === 0 && !showWaitingIndicator) {
    return (
      <div className={`session-conversation-page${isActive ? " is-active" : ""}`} hidden={!isActive}>
        <PanelEmptyState
          title={isLoading ? "Connecting to backend" : "Live session is ready"}
          body={
            isLoading
              ? "Fetching session state from the Rust backend."
              : `Send a prompt to ${session.agent} and this tile will fill with live cards.`
          }
        />
      </div>
    );
  }

  return (
    <div className={`session-conversation-page${isActive ? " is-active" : ""}`} hidden={!isActive}>
      <ConversationMessageList
        renderMessageCard={renderMessageCard}
        sessionId={session.id}
        messages={session.messages}
        scrollContainerRef={scrollContainerRef}
        isActive={isActive}
        onApprovalDecision={onApprovalDecision}
        onUserInputSubmit={onUserInputSubmit}
        onMcpElicitationSubmit={onMcpElicitationSubmit}
        onCodexAppRequestSubmit={onCodexAppRequestSubmit}
        conversationSearchQuery={conversationSearchQuery}
        conversationSearchMatchedItemKeys={conversationSearchMatchedItemKeys}
        conversationSearchActiveItemKey={conversationSearchActiveItemKey}
        onConversationSearchItemMount={onConversationSearchItemMount}
      />

      {showWaitingIndicator ? (
        <RunningIndicator agent={session.agent} lastPrompt={waitingIndicatorPrompt} />
      ) : null}

      {/* Only the active mounted page exposes find anchors so cached hidden pages cannot hijack scroll targets. */}
      {pendingPrompts.map((prompt) => (
        <MessageSlot
          key={prompt.id}
          itemKey={isActive ? `pendingPrompt:${prompt.id}` : undefined}
          isSearchMatch={conversationSearchMatchedItemKeys.has(`pendingPrompt:${prompt.id}`)}
          isSearchActive={conversationSearchActiveItemKey === `pendingPrompt:${prompt.id}`}
          onSearchItemMount={onConversationSearchItemMount}
        >
          <PendingPromptCard
            prompt={prompt}
            onCancel={() => onCancelQueuedPrompt(session.id, prompt.id)}
            searchQuery={
              conversationSearchActiveItemKey === `pendingPrompt:${prompt.id}` ? conversationSearchQuery : ""
            }
            searchHighlightTone={
              conversationSearchActiveItemKey === `pendingPrompt:${prompt.id}` ? "active" : "match"
            }
          />
        </MessageSlot>
      ))}
    </div>
  );
}, (previous, next) =>
  previous.renderMessageCard === next.renderMessageCard &&
  previous.session === next.session &&
  previous.scrollContainerRef === next.scrollContainerRef &&
  previous.isActive === next.isActive &&
  previous.isLoading === next.isLoading &&
  previous.showWaitingIndicator === next.showWaitingIndicator &&
  previous.waitingIndicatorPrompt === next.waitingIndicatorPrompt &&
  previous.onUserInputSubmit === next.onUserInputSubmit &&
  previous.onMcpElicitationSubmit === next.onMcpElicitationSubmit &&
  previous.onCodexAppRequestSubmit === next.onCodexAppRequestSubmit &&
  previous.conversationSearchQuery === next.conversationSearchQuery &&
  previous.conversationSearchMatchedItemKeys === next.conversationSearchMatchedItemKeys &&
  previous.conversationSearchActiveItemKey === next.conversationSearchActiveItemKey &&
  previous.onConversationSearchItemMount === next.onConversationSearchItemMount
);

function ConversationMessageList({
  renderMessageCard,
  sessionId,
  messages,
  scrollContainerRef,
  isActive,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
  conversationSearchQuery,
  conversationSearchMatchedItemKeys,
  conversationSearchActiveItemKey,
  onConversationSearchItemMount,
}: {
  renderMessageCard: RenderMessageCard;
  sessionId: string;
  messages: Message[];
  scrollContainerRef: RefObject<HTMLElement | null>;
  isActive: boolean;
  onApprovalDecision: (sessionId: string, messageId: string, decision: ApprovalDecision) => void;
  onUserInputSubmit: UserInputSubmitHandler;
  onMcpElicitationSubmit: McpElicitationSubmitHandler;
  onCodexAppRequestSubmit: CodexAppRequestSubmitHandler;
  conversationSearchQuery: string;
  conversationSearchMatchedItemKeys: ReadonlySet<string>;
  conversationSearchActiveItemKey: string | null;
  onConversationSearchItemMount: (itemKey: string, node: HTMLElement | null) => void;
}) {
  if (messages.length < CONVERSATION_VIRTUALIZATION_MIN_MESSAGES) {
    return (
      <>
        {/* Only the active mounted page exposes find anchors so cached hidden pages cannot hijack scroll targets. */}
        {messages.map((message, index) => (
          <MessageSlot
            key={message.id}
            itemKey={isActive ? `message:${message.id}` : undefined}
            isSearchMatch={conversationSearchMatchedItemKeys.has(`message:${message.id}`)}
            isSearchActive={conversationSearchActiveItemKey === `message:${message.id}`}
            onSearchItemMount={onConversationSearchItemMount}
          >
            {renderMessageCard(
              message,
              isActive && index >= messages.length - 2,
              (messageId, decision) => onApprovalDecision(sessionId, messageId, decision),
              (messageId, answers) => onUserInputSubmit(sessionId, messageId, answers),
              (messageId, action, content) =>
                onMcpElicitationSubmit(sessionId, messageId, action, content),
              (messageId, result) => onCodexAppRequestSubmit(sessionId, messageId, result),
            )}
          </MessageSlot>
        ))}
      </>
    );
  }

  return (
    <VirtualizedConversationMessageList
      isActive={isActive}
      renderMessageCard={renderMessageCard}
      sessionId={sessionId}
      messages={messages}
      scrollContainerRef={scrollContainerRef}
      conversationSearchQuery={conversationSearchQuery}
      conversationSearchMatchedItemKeys={conversationSearchMatchedItemKeys}
      conversationSearchActiveItemKey={conversationSearchActiveItemKey}
      onConversationSearchItemMount={onConversationSearchItemMount}
      onApprovalDecision={onApprovalDecision}
      onUserInputSubmit={onUserInputSubmit}
      onMcpElicitationSubmit={onMcpElicitationSubmit}
      onCodexAppRequestSubmit={onCodexAppRequestSubmit}
    />
  );
}


const SessionComposer = memo(function SessionComposer({
  paneId,
  isPaneActive,
  session,
  committedDraft,
  draftAttachments,
  formatByteSize,
  isSending,
  isStopping,
  isSessionBusy,
  isUpdating,
  isRefreshingModelOptions,
  modelOptionsError,
  agentCommands,
  hasLoadedAgentCommands,
  isRefreshingAgentCommands,
  agentCommandsError,
  showNewResponseIndicator,
  onScrollToLatest,
  onDraftCommit,
  onDraftAttachmentRemove,
  onRefreshSessionModelOptions,
  onRefreshAgentCommands,
  onSend,
  onSessionSettingsChange,
  onStopSession,
  onPaste,
}: {
  paneId: string;
  isPaneActive: boolean;
  session: Session | null;
  committedDraft: string;
  draftAttachments: DraftImageAttachment[];
  formatByteSize: (byteSize: number) => string;
  isSending: boolean;
  isStopping: boolean;
  isSessionBusy: boolean;
  isUpdating: boolean;
  isRefreshingModelOptions: boolean;
  modelOptionsError: string | null;
  agentCommands: AgentCommand[];
  hasLoadedAgentCommands: boolean;
  isRefreshingAgentCommands: boolean;
  agentCommandsError: string | null;
  showNewResponseIndicator: boolean;
  onScrollToLatest: () => void;
  onDraftCommit: (sessionId: string, nextValue: string) => void;
  onDraftAttachmentRemove: (sessionId: string, attachmentId: string) => void;
  onRefreshSessionModelOptions: (sessionId: string) => void;
  onRefreshAgentCommands: (sessionId: string) => void;
  onSend: (sessionId: string, draftText?: string, expandedText?: string | null) => boolean;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
  onStopSession: (sessionId: string) => void;
  onPaste: (event: ReactClipboardEvent<HTMLTextAreaElement>) => void;
}) {
  const composerInputRef = useRef<HTMLTextAreaElement | null>(null);
  const localDraftsRef = useRef<Record<string, string>>({});
  const committedDraftsRef = useRef<Record<string, string>>({});
  const onDraftCommitRef = useRef(onDraftCommit);
  const requestedSlashModelOptionsRef = useRef<string | null>(null);
  const requestedSlashAgentCommandsRef = useRef<string | null>(null);
  const slashOptionsRef = useRef<HTMLDivElement | null>(null);
  const [localDraftsBySessionId, setLocalDraftsBySessionId] = useState<Record<string, string>>({});
  const [promptHistoryStateBySessionId, setPromptHistoryStateBySessionId] = useState<
    Record<string, PromptHistoryState | undefined>
  >({});
  const [slashActiveIndex, setSlashActiveIndex] = useState(0);
  const [slashNavModality, setSlashNavModality] = useState<"keyboard" | "mouse">("keyboard");

  const activeSessionId = session?.id ?? null;
  const composerDraft =
    activeSessionId === null ? "" : (localDraftsBySessionId[activeSessionId] ?? committedDraft);
  const slashPalette = useMemo(
    () =>
      buildSlashPaletteState(
        session,
        composerDraft,
        isRefreshingModelOptions,
        modelOptionsError,
        agentCommands,
        hasLoadedAgentCommands,
        isRefreshingAgentCommands,
        agentCommandsError,
      ),
    [
      agentCommands,
      agentCommandsError,
      composerDraft,
      hasLoadedAgentCommands,
      isRefreshingAgentCommands,
      isRefreshingModelOptions,
      modelOptionsError,
      session,
    ],
  );
  const slashPaletteResetKey = slashPalette.kind === "none" ? "none" : slashPalette.resetKey;
  const slashPaletteSupportsModelRefresh =
    slashPalette.kind === "choice" && slashPalette.supportsLiveRefresh;
  const slashPaletteSupportsAgentRefresh =
    slashPalette.kind === "command" && Boolean(slashPalette.supportsRefresh);
  const activeSlashItem =
    slashPalette.kind === "none" || slashPalette.items.length === 0
      ? null
      : (slashPalette.items[Math.min(slashActiveIndex, slashPalette.items.length - 1)] ?? null);
  const composerInputDisabled = !session || isStopping;
  const composerSendDisabled =
    !session ||
    isSending ||
    isStopping ||
    isUpdating ||
    (slashPalette.kind !== "none" && slashPalette.items.length === 0);

  function resizeComposerInput() {
    const textarea = composerInputRef.current;
    if (!textarea) {
      return;
    }

    const computedStyle = window.getComputedStyle(textarea);
    const minHeight = parseFloat(computedStyle.minHeight) || 0;
    const borderHeight =
      (parseFloat(computedStyle.borderTopWidth) || 0) +
      (parseFloat(computedStyle.borderBottomWidth) || 0);
    const panelElement = textarea.closest(".workspace-pane");
    const panelSlotElement =
      panelElement instanceof HTMLElement && panelElement.parentElement instanceof HTMLElement
        ? panelElement.parentElement
        : null;
    const availablePanelHeight =
      panelSlotElement?.clientHeight ??
      (panelElement instanceof HTMLElement ? panelElement.clientHeight : 0);
    const maxHeight = Math.max(
      minHeight,
      availablePanelHeight > 0 ? availablePanelHeight * 0.4 : Number.POSITIVE_INFINITY,
    );

    textarea.style.height = "0px";

    const contentHeight = textarea.scrollHeight + borderHeight;
    const nextHeight = Math.min(Math.max(contentHeight, minHeight), maxHeight);
    textarea.style.height = `${nextHeight}px`;
    textarea.style.overflowY = contentHeight > maxHeight + 1 ? "auto" : "hidden";
  }

  useLayoutEffect(() => {
    resizeComposerInput();
  }, [activeSessionId, composerDraft]);

  useEffect(() => {
    localDraftsRef.current = localDraftsBySessionId;
  }, [localDraftsBySessionId]);

  useEffect(() => {
    onDraftCommitRef.current = onDraftCommit;
  }, [onDraftCommit]);

  useEffect(() => {
    setSlashActiveIndex(slashPalette.kind === "none" ? 0 : slashPalette.defaultActiveIndex);
  }, [activeSessionId, slashPaletteResetKey]);

  useEffect(() => {
    if (
      !session ||
      slashPalette.kind !== "choice" ||
      !slashPaletteSupportsModelRefresh ||
      !supportsLiveSessionModelOptions(session)
    ) {
      return;
    }

    if (session.modelOptions?.length) {
      requestedSlashModelOptionsRef.current = session.id;
      return;
    }

    if (isRefreshingModelOptions || requestedSlashModelOptionsRef.current === session.id) {
      return;
    }

    requestSlashModelOptions();
  }, [
    isRefreshingModelOptions,
    onRefreshSessionModelOptions,
    session,
    slashPalette.kind,
    slashPaletteSupportsModelRefresh,
  ]);

  useEffect(() => {
    if (slashPalette.kind === "none") {
      return;
    }

    const container = slashOptionsRef.current;
    if (!container) {
      return;
    }

    const activeOption = container.querySelector<HTMLButtonElement>(
      '.composer-slash-option.active[role="option"]',
    );
    if (!activeOption) {
      return;
    }

    const containerRect = container.getBoundingClientRect();
    const optionRect = activeOption.getBoundingClientRect();

    if (optionRect.top < containerRect.top) {
      container.scrollTop += optionRect.top - containerRect.top;
    } else if (optionRect.bottom > containerRect.bottom) {
      container.scrollTop += optionRect.bottom - containerRect.bottom;
    }
  }, [slashPalette.kind, slashPaletteResetKey, slashActiveIndex]);

  useEffect(() => {
    if (
      !session ||
      slashPalette.kind !== "command" ||
      !slashPaletteSupportsAgentRefresh ||
      !supportsAgentSlashCommands(session)
    ) {
      return;
    }

    const requestKey = `${session.id}:${session.workdir}:${session.agentCommandsRevision ?? 0}`;
    const requestKeyBase = `${session.id}:${session.workdir}:`;
    const alreadyRequested = requestedSlashAgentCommandsRef.current === requestKey;
    const isSameSessionRequest =
      requestedSlashAgentCommandsRef.current?.startsWith(requestKeyBase) ?? false;
    if (hasLoadedAgentCommands && !alreadyRequested && !isSameSessionRequest) {
      requestedSlashAgentCommandsRef.current = requestKey;
      return;
    }
    if (
      (hasLoadedAgentCommands && alreadyRequested) ||
      isRefreshingAgentCommands ||
      (agentCommandsError && alreadyRequested)
    ) {
      return;
    }

    requestSlashAgentCommands();
  }, [
    agentCommandsError,
    hasLoadedAgentCommands,
    isRefreshingAgentCommands,
    onRefreshAgentCommands,
    session,
    slashPalette.kind,
    slashPaletteSupportsAgentRefresh,
  ]);

  useEffect(() => {
    const textarea = composerInputRef.current;
    if (!textarea || typeof ResizeObserver === "undefined") {
      return;
    }

    const panelElement = textarea.closest(".workspace-pane");
    const panelSlotElement =
      panelElement instanceof HTMLElement && panelElement.parentElement instanceof HTMLElement
        ? panelElement.parentElement
        : null;
    let previousWidth = textarea.getBoundingClientRect().width;
    let previousAvailablePanelHeight =
      panelSlotElement?.clientHeight ??
      (panelElement instanceof HTMLElement ? panelElement.clientHeight : 0);
    const resizeObserver = new ResizeObserver((entries) => {
      const nextWidth =
        entries.find((entry) => entry.target === textarea)?.contentRect.width ??
        textarea.getBoundingClientRect().width;
      const nextAvailablePanelHeight =
        panelSlotElement?.clientHeight ??
        (panelElement instanceof HTMLElement ? panelElement.clientHeight : 0);
      const widthChanged = Math.abs(nextWidth - previousWidth) >= 1;
      const panelHeightChanged =
        Math.abs(nextAvailablePanelHeight - previousAvailablePanelHeight) >= 1;

      if (!widthChanged && !panelHeightChanged) {
        return;
      }

      previousWidth = nextWidth;
      previousAvailablePanelHeight = nextAvailablePanelHeight;
      resizeComposerInput();
    });

    resizeObserver.observe(textarea);
    if (panelSlotElement instanceof HTMLElement) {
      resizeObserver.observe(panelSlotElement);
    } else if (panelElement instanceof HTMLElement) {
      resizeObserver.observe(panelElement);
    }

    return () => {
      resizeObserver.disconnect();
    };
  }, [activeSessionId]);

  useEffect(() => {
    if (!activeSessionId) {
      return;
    }

    const previousCommitted = committedDraftsRef.current[activeSessionId];
    const localDraft = localDraftsRef.current[activeSessionId];

    committedDraftsRef.current[activeSessionId] = committedDraft;

    if (localDraft !== undefined && localDraft !== previousCommitted) {
      return;
    }

    setLocalDraftsBySessionId((current) => {
      if ((current[activeSessionId] ?? "") === committedDraft) {
        return current;
      }

      return {
        ...current,
        [activeSessionId]: committedDraft,
      };
    });
  }, [activeSessionId, committedDraft]);

  useEffect(() => {
    if (!activeSessionId) {
      return;
    }

    return () => {
      const latestDraft = localDraftsRef.current[activeSessionId];
      const committed = committedDraftsRef.current[activeSessionId] ?? "";
      if (latestDraft !== undefined && latestDraft !== committed) {
        committedDraftsRef.current[activeSessionId] = latestDraft;
        onDraftCommitRef.current(activeSessionId, latestDraft);
      }
    };
  }, [activeSessionId]);

  useEffect(() => {
    if (!activeSessionId || !isPaneActive || composerInputDisabled) {
      return;
    }

    focusComposerInput();
  }, [activeSessionId, composerInputDisabled, isPaneActive]);

  function resetPromptHistory(sessionId: string) {
    setPromptHistoryStateBySessionId((current) => {
      if (!current[sessionId]) {
        return current;
      }

      const nextState = { ...current };
      delete nextState[sessionId];
      return nextState;
    });
  }

  function updateLocalDraft(sessionId: string, nextValue: string) {
    localDraftsRef.current = {
      ...localDraftsRef.current,
      [sessionId]: nextValue,
    };

    setLocalDraftsBySessionId((current) => {
      if ((current[sessionId] ?? "") === nextValue) {
        return current;
      }

      return {
        ...current,
        [sessionId]: nextValue,
      };
    });
  }

  function commitDraft(sessionId: string, nextValue: string) {
    committedDraftsRef.current[sessionId] = nextValue;
    onDraftCommit(sessionId, nextValue);
  }

  function getComposerDraftValue() {
    return composerInputRef.current?.value ?? composerDraft;
  }

  function focusComposerInput(selectionStart?: number) {
    window.requestAnimationFrame(() => {
      const textarea = composerInputRef.current;
      if (!textarea) {
        return;
      }

      const nextSelectionStart = selectionStart ?? textarea.value.length;
      textarea.focus();
      textarea.setSelectionRange(nextSelectionStart, nextSelectionStart);
    });
  }

  function requestSlashModelOptions(force = false) {
    if (!session || !supportsLiveSessionModelOptions(session)) {
      return;
    }

    if (!force && requestedSlashModelOptionsRef.current === session.id) {
      return;
    }

    requestedSlashModelOptionsRef.current = session.id;
    void onRefreshSessionModelOptions(session.id);
  }

  function requestSlashAgentCommands(force = false) {
    if (!session || !supportsAgentSlashCommands(session)) {
      return;
    }

    const requestKey = `${session.id}:${session.workdir}:${session.agentCommandsRevision ?? 0}`;
    if (!force && requestedSlashAgentCommandsRef.current === requestKey) {
      return;
    }

    requestedSlashAgentCommandsRef.current = requestKey;
    void onRefreshAgentCommands(session.id);
  }

  function handleComposerChange(nextValue: string) {
    if (!activeSessionId) {
      return;
    }

    resetPromptHistory(activeSessionId);
    updateLocalDraft(activeSessionId, nextValue);
  }

  function handleComposerBlur() {
    if (!activeSessionId) {
      return;
    }

    commitDraft(activeSessionId, getComposerDraftValue());
  }

  function applySlashPaletteItem(item: SlashPaletteItem, keepPaletteOpen = false) {
    if (!session || isSending || isStopping) {
      return;
    }

    if (item.kind === "command") {
      resetPromptHistory(session.id);
      const nextDraft = `${item.command} `;
      updateLocalDraft(session.id, nextDraft);
      focusComposerInput(nextDraft.length);
      return;
    }

    if (item.kind === "agent-command") {
      if (isUpdating) {
        focusComposerInput(getComposerDraftValue().length);
        return;
      }

      const agentCommand = item.command;
      const parsedDraft = parseAgentCommandDraft(getComposerDraftValue());
      const matchesSelectedCommand =
        parsedDraft?.commandName.toLowerCase() === item.name.toLowerCase();
      if (item.hasArguments && !matchesSelectedCommand) {
        resetPromptHistory(session.id);
        const nextDraft = `/${item.name} `;
        updateLocalDraft(session.id, nextDraft);
        focusComposerInput(nextDraft.length);
        return;
      }

      const visiblePrompt = (matchesSelectedCommand
        ? getComposerDraftValue()
        : `/${item.name}`).trim();
      const accepted =
        normalizedAgentCommandKind(agentCommand) === "nativeSlash"
          ? onSend(session.id, visiblePrompt)
          : onSend(
              session.id,
              visiblePrompt,
              agentCommand.content.split("$ARGUMENTS").join(
                matchesSelectedCommand ? (parsedDraft?.argumentsText ?? "") : "",
              ),
            );
      if (!accepted) {
        focusComposerInput();
        return;
      }

      resetPromptHistory(session.id);
      updateLocalDraft(session.id, "");
      commitDraft(session.id, "");
      focusComposerInput();
      return;
    }

    if (isUpdating) {
      focusComposerInput(getComposerDraftValue().length);
      return;
    }

    resetPromptHistory(session.id);
    void onSessionSettingsChange(session.id, item.field, item.value);
    if (keepPaletteOpen) {
      focusComposerInput(getComposerDraftValue().length);
    } else {
      updateLocalDraft(session.id, "");
      commitDraft(session.id, "");
      focusComposerInput(0);
    }
  }

  function handleComposerSend() {
    if (!session || isSending || isStopping) {
      return;
    }

    if (slashPalette.kind !== "none") {
      if (activeSlashItem) {
        if (activeSlashItem.kind === "choice" && isUpdating) {
          focusComposerInput(getComposerDraftValue().length);
          return;
        }
        applySlashPaletteItem(activeSlashItem);
      }
      return;
    }

    if (isUpdating) {
      focusComposerInput(getComposerDraftValue().length);
      return;
    }

    const draftToSend = getComposerDraftValue();
    const accepted = onSend(session.id, draftToSend);
    if (!accepted) {
      focusComposerInput();
      return;
    }

    resetPromptHistory(session.id);
    updateLocalDraft(session.id, "");
    commitDraft(session.id, "");
    focusComposerInput();
  }

  function handleComposerKeyDown(event: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (!session) {
      return;
    }

    if (slashPalette.kind !== "none") {
      if (event.key === "Escape") {
        event.preventDefault();
        resetPromptHistory(session.id);
        updateLocalDraft(session.id, "");
        commitDraft(session.id, "");
        return;
      }

      if ((event.key === "Enter" && !event.shiftKey) || event.key === "Tab") {
        event.preventDefault();
        handleComposerSend();
        return;
      }

      if (
        isSpaceKey(event) &&
        !event.altKey &&
        !event.ctrlKey &&
        !event.metaKey &&
        !event.shiftKey
      ) {
        if (activeSlashItem) {
          event.preventDefault();
          if (activeSlashItem.kind === "choice") {
            applySlashPaletteItem(activeSlashItem, true);
          } else {
            applySlashPaletteItem(activeSlashItem);
          }
        }
        return;
      }

      if (
        (event.key === "ArrowUp" || event.key === "ArrowDown") &&
        !event.altKey &&
        !event.ctrlKey &&
        !event.metaKey &&
        !event.shiftKey
      ) {
        event.preventDefault();
        setSlashNavModality("keyboard");
        if (slashPalette.items.length === 0) {
          return;
        }

        setSlashActiveIndex((current) => {
          if (event.key === "ArrowUp") {
            return current <= 0 ? slashPalette.items.length - 1 : current - 1;
          }

          return current >= slashPalette.items.length - 1 ? 0 : current + 1;
        });
        return;
      }
    }

    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      handleComposerSend();
      return;
    }

    if (event.key !== "ArrowUp" && event.key !== "ArrowDown") {
      return;
    }

    if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) {
      return;
    }

    const textarea = event.currentTarget;
    if (textarea.selectionStart !== 0 || textarea.selectionEnd !== 0) {
      return;
    }

    const promptHistory = collectUserPromptHistory(session);
    if (promptHistory.length === 0) {
      return;
    }

    const historyState = promptHistoryStateBySessionId[session.id];
    if (event.key === "ArrowDown" && !historyState) {
      return;
    }

    event.preventDefault();

    if (event.key === "ArrowUp") {
      const nextIndex = historyState
        ? Math.max(historyState.index - 1, 0)
        : promptHistory.length - 1;
      const draftSnapshot = historyState?.draft ?? getComposerDraftValue();

      setPromptHistoryStateBySessionId((current) => ({
        ...current,
        [session.id]: {
          index: nextIndex,
          draft: draftSnapshot,
        },
      }));
      updateLocalDraft(session.id, promptHistory[nextIndex]);
    } else {
      const currentHistoryState = historyState;
      if (!currentHistoryState) {
        return;
      }

      if (currentHistoryState.index >= promptHistory.length - 1) {
        resetPromptHistory(session.id);
        updateLocalDraft(session.id, currentHistoryState.draft);
      } else {
        const nextIndex = currentHistoryState.index + 1;
        setPromptHistoryStateBySessionId((current) => ({
          ...current,
          [session.id]: {
            index: nextIndex,
            draft: currentHistoryState.draft,
          },
        }));
        updateLocalDraft(session.id, promptHistory[nextIndex]);
      }
    }

    window.requestAnimationFrame(() => {
      textarea.setSelectionRange(0, 0);
    });
  }

  const slashPaletteErrorMessage =
    slashPalette.kind === "none" ? null : (slashPalette.errorMessage ?? null);
  const slashPaletteIsRefreshing =
    slashPalette.kind === "none" ? false : Boolean(slashPalette.isRefreshing);
  const slashPaletteRefreshActionLabel =
    slashPalette.kind === "none" ? null : (slashPalette.refreshActionLabel ?? null);
  const slashPaletteSupportsRefresh =
    slashPalette.kind === "choice"
      ? slashPalette.supportsLiveRefresh
      : slashPalette.kind === "command"
        ? Boolean(slashPalette.supportsRefresh)
        : false;
  const slashPaletteStatusText =
    slashPalette.kind === "command" ? (slashPalette.statusText ?? null) : null;
  const showSlashPaletteStatus =
    slashPalette.kind !== "none" &&
    (
      slashPaletteSupportsRefresh ||
      Boolean(slashPaletteErrorMessage) ||
      Boolean(slashPaletteStatusText) ||
      (slashPalette.kind === "choice" && isUpdating)
    );

  return (
    <footer className="composer">
      {showNewResponseIndicator ? (
        <button className="new-response-indicator" type="button" onClick={onScrollToLatest}>
          New response
        </button>
      ) : null}
      {draftAttachments.length > 0 ? (
        <div className="composer-attachments" aria-label="Draft image attachments">
          {draftAttachments.map((attachment) => (
            <article key={attachment.id} className="composer-attachment-card">
              <img
                className="composer-attachment-preview"
                src={attachment.previewUrl}
                alt={attachment.fileName}
              />
              <div className="composer-attachment-copy">
                <strong className="composer-attachment-name">{attachment.fileName}</strong>
                <span className="composer-attachment-meta">
                  {formatByteSize(attachment.byteSize)} | {attachment.mediaType}
                </span>
              </div>
              <button
                className="composer-attachment-remove"
                type="button"
                onClick={() => session && onDraftAttachmentRemove(session.id, attachment.id)}
                aria-label={`Remove ${attachment.fileName}`}
                disabled={composerInputDisabled}
              >
                Remove
              </button>
            </article>
          ))}
        </div>
      ) : null}
      <div className="composer-row">
        <textarea
          id={`prompt-${paneId}`}
          ref={composerInputRef}
          className="composer-input"
          aria-label={session ? `Message ${session.name}` : "Message session"}
          value={composerDraft}
          onChange={(event) => handleComposerChange(event.target.value)}
          onBlur={handleComposerBlur}
          disabled={composerInputDisabled}
          onKeyDown={handleComposerKeyDown}
          onPaste={onPaste}
          placeholder={session ? `Send a prompt to ${session.agent}...` : "Open a session..."}
          rows={1}
        />
        <div className="composer-actions">
          {session && (isSessionBusy || isStopping) ? (
            <button
              className="ghost-button composer-stop-button"
              type="button"
              onClick={() => onStopSession(session.id)}
              disabled={isStopping}
            >
              {isStopping ? "Stopping..." : "Stop"}
            </button>
          ) : null}
          <button
            className="send-button"
            type="button"
            onMouseDown={(event) => {
              event.preventDefault();
            }}
            onClick={handleComposerSend}
            disabled={composerSendDisabled}
          >
            {isSending
              ? isSessionBusy
                ? "Queueing..."
                : "Sending..."
              : isSessionBusy
                ? "Queue"
                : "Send"}
          </button>
        </div>
      </div>
      {session ? (
        <p className="composer-hint">
          Paste PNG, JPEG, GIF, or WebP images into the prompt. Drag-and-drop is not supported
          yet.
        </p>
      ) : null}
      {session && slashPalette.kind !== "none" ? (
        <div className="composer-slash-menu" role="listbox" aria-label={slashPalette.title}>
          <div className="composer-slash-header">
            <strong className="composer-slash-title">{slashPalette.title}</strong>
            <span className="composer-slash-hint">{slashPalette.hint}</span>
          </div>
          {showSlashPaletteStatus ? (
            <div className="composer-slash-status">
              {slashPaletteErrorMessage ? (
                <p className="composer-slash-error" role="alert">
                  {slashPaletteErrorMessage}
                </p>
              ) : slashPalette.kind === "choice" ? (
                <p className="composer-slash-status-text" aria-live="polite">
                  {isUpdating ? (
                    <span className="composer-slash-status-inline">
                      <span className="composer-slash-status-spinner" aria-hidden="true" />
                      Applying setting...
                    </span>
                  ) : slashPalette.isRefreshing ? (
                    "Loading live model options..."
                  ) : slashPalette.supportsLiveRefresh ? (
                    "Refresh live models to update this list from the active session."
                  ) : null}
                </p>
              ) : slashPaletteStatusText ? (
                <p className="composer-slash-status-text" aria-live="polite">
                  {slashPaletteIsRefreshing ? (
                    <span className="composer-slash-status-inline">
                      <span className="composer-slash-status-spinner" aria-hidden="true" />
                      {slashPaletteStatusText}
                    </span>
                  ) : (
                    slashPaletteStatusText
                  )}
                </p>
              ) : null}
              {slashPaletteSupportsRefresh ? (
                <button
                  className="ghost-button composer-slash-refresh-button"
                  type="button"
                  onClick={() => {
                    if (slashPalette.kind === "choice") {
                      requestSlashModelOptions(true);
                    } else {
                      requestSlashAgentCommands(true);
                    }
                  }}
                  disabled={
                    (slashPalette.kind === "choice"
                      ? isRefreshingModelOptions
                      : isRefreshingAgentCommands) || isUpdating
                  }
                >
                  {slashPaletteIsRefreshing
                    ? "Loading..."
                    : (slashPaletteRefreshActionLabel ??
                        (slashPalette.kind === "choice"
                          ? "Refresh live models"
                          : "Refresh agent commands"))}
                </button>
              ) : null}
            </div>
          ) : null}
          {slashPalette.items.length > 0 ? (
            <div
              ref={slashOptionsRef}
              className={`composer-slash-options modality-${slashNavModality}`}
            >
              {slashPalette.items.map((item, index) => {
                const isActive = activeSlashItem?.key === item.key && index === slashActiveIndex;

                return (
                  <div key={item.key} className="composer-slash-option-group">
                    {item.sectionLabel ? (
                      <div className="composer-slash-section-label">{item.sectionLabel}</div>
                    ) : null}
                    <button
                      className={`composer-slash-option${isActive ? " active" : ""}`}
                      type="button"
                      role="option"
                      aria-selected={isActive}
                      onMouseDown={(event) => {
                        event.preventDefault();
                      }}
                      onMouseMove={() => {
                        setSlashNavModality("mouse");
                        if (slashActiveIndex !== index) {
                          setSlashActiveIndex(index);
                        }
                      }}
                      onClick={() => applySlashPaletteItem(item)}
                      disabled={(item.kind === "choice" || item.kind === "agent-command") && isUpdating}
                    >
                      <span className="composer-slash-option-copy">
                        <span className="composer-slash-option-label">{item.label}</span>
                        <span className="composer-slash-option-detail">{item.detail}</span>
                      </span>
                      {item.kind === "choice" && item.isCurrent ? (
                        isUpdating ? (
                          <span className="composer-slash-option-badge pending">
                            <span className="composer-slash-option-spinner" aria-hidden="true" />
                            Applying
                          </span>
                        ) : (
                          <span className="composer-slash-option-badge">Current</span>
                        )
                      ) : item.kind === "agent-command" ? (
                        <span className="composer-slash-option-badge">Agent</span>
                      ) : null}
                    </button>
                  </div>
                );
              })}
            </div>
          ) : (
            <p className="composer-slash-empty">
              {slashPalette.emptyMessage}
              {slashPalette.kind === "choice" &&
              slashPalette.supportsLiveRefresh &&
              slashPalette.isRefreshing
                ? " Live options will appear here as soon as they load."
                : slashPalette.kind === "command" && slashPaletteIsRefreshing
                  ? " Agent commands will appear here as soon as they load."
                  : null}
            </p>
          )}
        </div>
      ) : null}
    </footer>
  );
}, (previous, next) =>
  previous.paneId === next.paneId &&
  previous.isPaneActive === next.isPaneActive &&
  previous.session === next.session &&
  previous.committedDraft === next.committedDraft &&
  previous.draftAttachments === next.draftAttachments &&
  previous.formatByteSize === next.formatByteSize &&
  previous.isSending === next.isSending &&
  previous.isStopping === next.isStopping &&
  previous.isSessionBusy === next.isSessionBusy &&
  previous.isUpdating === next.isUpdating &&
  previous.isRefreshingModelOptions === next.isRefreshingModelOptions &&
  previous.modelOptionsError === next.modelOptionsError &&
  previous.agentCommands === next.agentCommands &&
  previous.hasLoadedAgentCommands === next.hasLoadedAgentCommands &&
  previous.isRefreshingAgentCommands === next.isRefreshingAgentCommands &&
  previous.agentCommandsError === next.agentCommandsError &&
  previous.showNewResponseIndicator === next.showNewResponseIndicator
);
