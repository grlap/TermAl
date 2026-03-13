import {
  memo,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ClipboardEvent as ReactClipboardEvent,
  type ReactNode,
  type RefObject,
} from "react";
import {
  renderHighlightedText,
  type SearchHighlightTone,
} from "../search-highlight";
import type {
  ApprovalDecision,
  ApprovalPolicy,
  ClaudeApprovalMode,
  CommandMessage,
  CursorMode,
  DiffMessage,
  GeminiApprovalMode,
  ImageAttachment,
  Message,
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
  | "claudeApprovalMode"
  | "cursorMode"
  | "geminiApprovalMode";
type SessionSettingsValue =
  | string
  | SandboxMode
  | ApprovalPolicy
  | ClaudeApprovalMode
  | CursorMode
  | GeminiApprovalMode;

const CONVERSATION_VIRTUALIZATION_MIN_MESSAGES = 80;
const VIRTUALIZED_MESSAGE_OVERSCAN_PX = 960;
const VIRTUALIZED_MESSAGE_GAP_PX = 12;
const DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT = 720;
const EMPTY_MATCHED_ITEM_KEYS = new Set<string>();

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
  renderMessageCard: (
    message: Message,
    preferImmediateHeavyRender: boolean,
    onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void,
  ) => JSX.Element | null;
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
  activeSession,
  committedDraft,
  draftAttachments,
  formatByteSize,
  isSending,
  isStopping,
  isSessionBusy,
  showNewResponseIndicator,
  footerModeLabel,
  onScrollToLatest,
  onDraftCommit,
  onDraftAttachmentRemove,
  onSend,
  onStopSession,
  onPaste,
}: {
  paneId: string;
  viewMode: PaneViewMode;
  activeSession: Session | null;
  committedDraft: string;
  draftAttachments: DraftImageAttachment[];
  formatByteSize: (byteSize: number) => string;
  isSending: boolean;
  isStopping: boolean;
  isSessionBusy: boolean;
  showNewResponseIndicator: boolean;
  footerModeLabel: string;
  onScrollToLatest: () => void;
  onDraftCommit: (sessionId: string, nextValue: string) => void;
  onDraftAttachmentRemove: (sessionId: string, attachmentId: string) => void;
  onSend: (sessionId: string, draftText?: string) => void;
  onStopSession: (sessionId: string) => void;
  onPaste: (event: ReactClipboardEvent<HTMLTextAreaElement>) => void;
}): JSX.Element {
  if (viewMode === "session") {
    return (
      <SessionComposer
        paneId={paneId}
        session={activeSession}
        committedDraft={committedDraft}
        draftAttachments={draftAttachments}
        formatByteSize={formatByteSize}
        isSending={isSending}
        isStopping={isStopping}
        isSessionBusy={isSessionBusy}
        showNewResponseIndicator={showNewResponseIndicator}
        onScrollToLatest={onScrollToLatest}
        onDraftCommit={onDraftCommit}
        onDraftAttachmentRemove={onDraftAttachmentRemove}
        onSend={onSend}
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
  renderMessageCard: (
    message: Message,
    preferImmediateHeavyRender: boolean,
    onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void,
  ) => JSX.Element | null;
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
            renderMessageCard={renderMessageCard}
            session={session}
            scrollContainerRef={scrollContainerRef}
            isActive={session.id === activeSession.id}
            isLoading={isLoading && session.id === activeSession.id}
            showWaitingIndicator={showWaitingIndicator && session.id === activeSession.id}
            waitingIndicatorPrompt={session.id === activeSession.id ? waitingIndicatorPrompt : null}
            onApprovalDecision={onApprovalDecision}
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
    return renderPromptSettings(paneId, activeSession, isUpdating, onSessionSettingsChange) ?? (
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
          <MessageSlot key={message.id}>{renderCommandCard(message)}</MessageSlot>
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
          <MessageSlot key={message.id}>{renderDiffCard(message)}</MessageSlot>
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
  previous.conversationSearchQuery === next.conversationSearchQuery &&
  previous.conversationSearchMatchedItemKeys === next.conversationSearchMatchedItemKeys &&
  previous.conversationSearchActiveItemKey === next.conversationSearchActiveItemKey &&
  previous.onConversationSearchItemMount === next.onConversationSearchItemMount &&
  previous.renderCommandCard === next.renderCommandCard &&
  previous.renderDiffCard === next.renderDiffCard &&
  previous.renderMessageCard === next.renderMessageCard &&
  previous.renderPromptSettings === next.renderPromptSettings
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
  onCancelQueuedPrompt,
  conversationSearchQuery,
  conversationSearchMatchedItemKeys,
  conversationSearchActiveItemKey,
  onConversationSearchItemMount,
}: {
  renderMessageCard: (
    message: Message,
    preferImmediateHeavyRender: boolean,
    onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void,
  ) => JSX.Element | null;
  session: Session;
  scrollContainerRef: RefObject<HTMLElement | null>;
  isActive: boolean;
  isLoading: boolean;
  showWaitingIndicator: boolean;
  waitingIndicatorPrompt: string | null;
  onApprovalDecision: (sessionId: string, messageId: string, decision: ApprovalDecision) => void;
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
  conversationSearchQuery,
  conversationSearchMatchedItemKeys,
  conversationSearchActiveItemKey,
  onConversationSearchItemMount,
}: {
  renderMessageCard: (
    message: Message,
    preferImmediateHeavyRender: boolean,
    onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void,
  ) => JSX.Element | null;
  sessionId: string;
  messages: Message[];
  scrollContainerRef: RefObject<HTMLElement | null>;
  isActive: boolean;
  onApprovalDecision: (sessionId: string, messageId: string, decision: ApprovalDecision) => void;
  conversationSearchQuery: string;
  conversationSearchMatchedItemKeys: ReadonlySet<string>;
  conversationSearchActiveItemKey: string | null;
  onConversationSearchItemMount: (itemKey: string, node: HTMLElement | null) => void;
}) {
  const hasConversationSearch = conversationSearchQuery.trim().length > 0;

  if (hasConversationSearch || !isActive || messages.length < CONVERSATION_VIRTUALIZATION_MIN_MESSAGES) {
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
            )}
          </MessageSlot>
        ))}
      </>
    );
  }

  return (
    <VirtualizedConversationMessageList
      renderMessageCard={renderMessageCard}
      sessionId={sessionId}
      messages={messages}
      scrollContainerRef={scrollContainerRef}
      onApprovalDecision={onApprovalDecision}
    />
  );
}

function VirtualizedConversationMessageList({
  renderMessageCard,
  sessionId,
  messages,
  scrollContainerRef,
  onApprovalDecision,
}: {
  renderMessageCard: (
    message: Message,
    preferImmediateHeavyRender: boolean,
    onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void,
  ) => JSX.Element | null;
  sessionId: string;
  messages: Message[];
  scrollContainerRef: RefObject<HTMLElement | null>;
  onApprovalDecision: (sessionId: string, messageId: string, decision: ApprovalDecision) => void;
}) {
  const messageHeightsRef = useRef<Record<string, number>>({});
  const visibleRangeRef = useRef({
    startIndex: 0,
    endIndex: messages.length,
  });
  const [viewport, setViewport] = useState({
    height: DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
    scrollTop: 0,
  });
  const [layoutVersion, setLayoutVersion] = useState(0);

  const messageIndexById = useMemo(
    () => new Map(messages.map((message, index) => [message.id, index])),
    [messages],
  );
  const messageHeights = useMemo(
    () =>
      messages.map(
        (message) => messageHeightsRef.current[message.id] ?? estimateConversationMessageHeight(message),
      ),
    [layoutVersion, messages],
  );
  const layout = useMemo(() => buildVirtualizedMessageLayout(messageHeights), [messageHeights]);
  const activeViewport = scrollContainerRef.current;
  const viewportHeight =
    activeViewport?.clientHeight && activeViewport.clientHeight > 0
      ? activeViewport.clientHeight
      : viewport.height;
  const viewportScrollTop = activeViewport ? activeViewport.scrollTop : viewport.scrollTop;
  const visibleRange = useMemo(
    () =>
      findVirtualizedMessageRange(
        layout.tops,
        messageHeights,
        viewportScrollTop,
        viewportHeight,
        VIRTUALIZED_MESSAGE_OVERSCAN_PX,
      ),
    [layout.tops, messageHeights, viewportHeight, viewportScrollTop],
  );

  useEffect(() => {
    visibleRangeRef.current = visibleRange;
  }, [visibleRange]);

  useEffect(() => {
    messageHeightsRef.current = Object.fromEntries(
      messages
        .filter((message) => messageHeightsRef.current[message.id] !== undefined)
        .map((message) => [message.id, messageHeightsRef.current[message.id] as number]),
    );
  }, [messages]);

  useLayoutEffect(() => {
    const node = scrollContainerRef.current;
    if (!node) {
      return;
    }

    const syncViewport = () => {
      const nextState = {
        height: node.clientHeight > 0 ? node.clientHeight : DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
        scrollTop: node.scrollTop,
      };

      setViewport((current) =>
        current.height === nextState.height && current.scrollTop === nextState.scrollTop
          ? current
          : nextState,
      );
    };

    syncViewport();
    node.addEventListener("scroll", syncViewport, { passive: true });
    const resizeObserver = new ResizeObserver(syncViewport);
    resizeObserver.observe(node);

    return () => {
      node.removeEventListener("scroll", syncViewport);
      resizeObserver.disconnect();
    };
  }, [scrollContainerRef, sessionId]);

  function handleHeightChange(messageId: string, nextHeight: number) {
    if (!Number.isFinite(nextHeight) || nextHeight <= 0) {
      return;
    }

    const previousHeight =
      messageHeightsRef.current[messageId] ??
      estimateConversationMessageHeight(messages[messageIndexById.get(messageId) ?? 0]);
    if (Math.abs(previousHeight - nextHeight) < 1) {
      return;
    }

    messageHeightsRef.current[messageId] = nextHeight;

    const messageIndex = messageIndexById.get(messageId);
    const node = scrollContainerRef.current;
    if (node && messageIndex !== undefined && messageIndex < visibleRangeRef.current.startIndex) {
      node.scrollTop += nextHeight - previousHeight;
    }

    setLayoutVersion((current) => current + 1);
  }

  return (
    <div className="virtualized-message-list" style={{ height: layout.totalHeight }}>
      {messages
        .slice(visibleRange.startIndex, visibleRange.endIndex)
        .map((message, visibleIndex) => {
          const messageIndex = visibleRange.startIndex + visibleIndex;
          return (
            <MeasuredMessageCard
              key={message.id}
              renderMessageCard={renderMessageCard}
              message={message}
              preferImmediateHeavyRender={messageIndex >= messages.length - 2}
              top={layout.tops[messageIndex] ?? 0}
              onApprovalDecision={(messageId, decision) => onApprovalDecision(sessionId, messageId, decision)}
              onHeightChange={handleHeightChange}
            />
          );
        })}
    </div>
  );
}

function MeasuredMessageCard({
  renderMessageCard,
  message,
  preferImmediateHeavyRender,
  onApprovalDecision,
  onHeightChange,
  top,
}: {
  renderMessageCard: (
    message: Message,
    preferImmediateHeavyRender: boolean,
    onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void,
  ) => ReactNode;
  message: Message;
  preferImmediateHeavyRender: boolean;
  onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void;
  onHeightChange: (messageId: string, nextHeight: number) => void;
  top: number;
}) {
  const slotRef = useRef<HTMLDivElement | null>(null);

  useLayoutEffect(() => {
    const node = slotRef.current;
    if (!node) {
      return;
    }

    let frameId = 0;
    const measure = () => {
      frameId = 0;
      onHeightChange(message.id, node.getBoundingClientRect().height);
    };

    measure();
    const resizeObserver = new ResizeObserver(() => {
      if (frameId !== 0) {
        return;
      }

      frameId = window.requestAnimationFrame(measure);
    });
    resizeObserver.observe(node);

    return () => {
      resizeObserver.disconnect();
      if (frameId !== 0) {
        window.cancelAnimationFrame(frameId);
      }
    };
  }, [message, onHeightChange]);

  return (
    <div ref={slotRef} className="virtualized-message-slot" style={{ top }}>
      {renderMessageCard(message, preferImmediateHeavyRender, onApprovalDecision)}
    </div>
  );
}

const SessionComposer = memo(function SessionComposer({
  paneId,
  session,
  committedDraft,
  draftAttachments,
  formatByteSize,
  isSending,
  isStopping,
  isSessionBusy,
  showNewResponseIndicator,
  onScrollToLatest,
  onDraftCommit,
  onDraftAttachmentRemove,
  onSend,
  onStopSession,
  onPaste,
}: {
  paneId: string;
  session: Session | null;
  committedDraft: string;
  draftAttachments: DraftImageAttachment[];
  formatByteSize: (byteSize: number) => string;
  isSending: boolean;
  isStopping: boolean;
  isSessionBusy: boolean;
  showNewResponseIndicator: boolean;
  onScrollToLatest: () => void;
  onDraftCommit: (sessionId: string, nextValue: string) => void;
  onDraftAttachmentRemove: (sessionId: string, attachmentId: string) => void;
  onSend: (sessionId: string, draftText?: string) => void;
  onStopSession: (sessionId: string) => void;
  onPaste: (event: ReactClipboardEvent<HTMLTextAreaElement>) => void;
}) {
  const composerInputRef = useRef<HTMLTextAreaElement | null>(null);
  const localDraftsRef = useRef<Record<string, string>>({});
  const committedDraftsRef = useRef<Record<string, string>>({});
  const onDraftCommitRef = useRef(onDraftCommit);
  const [localDraftsBySessionId, setLocalDraftsBySessionId] = useState<Record<string, string>>({});
  const [promptHistoryStateBySessionId, setPromptHistoryStateBySessionId] = useState<
    Record<string, PromptHistoryState | undefined>
  >({});

  const activeSessionId = session?.id ?? null;
  const composerDraft =
    activeSessionId === null ? "" : (localDraftsBySessionId[activeSessionId] ?? committedDraft);
  const composerInputDisabled = !session || isStopping;
  const composerSendDisabled = !session || isSending || isStopping;

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

  function handleComposerSend() {
    if (!session) {
      return;
    }

    const draftToSend = getComposerDraftValue();
    resetPromptHistory(session.id);
    updateLocalDraft(session.id, "");
    commitDraft(session.id, "");
    onSend(session.id, draftToSend);
    window.requestAnimationFrame(() => {
      composerInputRef.current?.focus();
    });
  }

  function handleComposerKeyDown(event: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (!session) {
      return;
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
                  {formatByteSize(attachment.byteSize)} · {attachment.mediaType}
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
    </footer>
  );
}, (previous, next) =>
  previous.paneId === next.paneId &&
  previous.session === next.session &&
  previous.committedDraft === next.committedDraft &&
  previous.draftAttachments === next.draftAttachments &&
  previous.formatByteSize === next.formatByteSize &&
  previous.isSending === next.isSending &&
  previous.isStopping === next.isStopping &&
  previous.isSessionBusy === next.isSessionBusy &&
  previous.showNewResponseIndicator === next.showNewResponseIndicator
);

function RunningIndicator({
  agent,
  lastPrompt,
}: {
  agent: Session["agent"];
  lastPrompt: string | null;
}) {
  return (
    <article
      className={`activity-card activity-card-live ${lastPrompt ? "has-tooltip" : ""}`}
      role="status"
      aria-live="polite"
    >
      <div className="activity-spinner" aria-hidden="true" />
      <div>
        <div className="card-label">Live turn</div>
        <h3>{agent} is working</h3>
        <p>Waiting for the next chunk of output...</p>
      </div>
      {lastPrompt ? (
        <div className="activity-tooltip" role="tooltip">
          <div className="activity-tooltip-label">Last prompt</div>
          <p>{lastPrompt}</p>
        </div>
      ) : null}
    </article>
  );
}

const PendingPromptCard = memo(function PendingPromptCard({
  prompt,
  onCancel,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  prompt: PendingPrompt;
  onCancel: () => void;
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  return (
    <article className="message-card bubble bubble-you pending-prompt-card">
      <div className="pending-prompt-header">
        <MessageMeta author="you" timestamp={prompt.timestamp} />
        <button
          className="pending-prompt-dismiss"
          type="button"
          onClick={onCancel}
          aria-label="Cancel queued prompt"
          title="Cancel queued prompt"
        >
          x
        </button>
      </div>
      {prompt.attachments && prompt.attachments.length > 0 ? (
        <MessageAttachmentList
          attachments={prompt.attachments}
          searchQuery={searchQuery}
          searchHighlightTone={searchHighlightTone}
        />
      ) : null}
      {prompt.text ? (
        <p className="plain-text-copy">
          {renderHighlightedText(prompt.text, searchQuery, searchHighlightTone)}
        </p>
      ) : (
        <p className="support-copy">{imageAttachmentSummaryLabel(prompt.attachments?.length ?? 0)}</p>
      )}
    </article>
  );
}, (previous, next) =>
  previous.prompt === next.prompt &&
  previous.searchQuery === next.searchQuery &&
  previous.searchHighlightTone === next.searchHighlightTone
);

function MessageMeta({ author, timestamp }: { author: string; timestamp: string }) {
  return (
    <div className="message-meta">
      <span>{author === "you" ? "You" : "Agent"}</span>
      <span>{timestamp}</span>
    </div>
  );
}

function MessageAttachmentList({
  attachments,
  searchQuery = "",
  searchHighlightTone = "match",
}: {
  attachments: ImageAttachment[];
  searchQuery?: string;
  searchHighlightTone?: SearchHighlightTone;
}) {
  return (
    <div className="message-attachment-list">
      {attachments.map((attachment, index) => (
        <div
          key={`${attachment.fileName}-${attachment.byteSize}-${index}`}
          className="message-attachment-chip"
        >
          <strong className="message-attachment-name">
            {renderHighlightedText(attachment.fileName, searchQuery, searchHighlightTone)}
          </strong>
          <span className="message-attachment-meta">
            {formatByteSize(attachment.byteSize)} ·{" "}
            {renderHighlightedText(attachment.mediaType, searchQuery, searchHighlightTone)}
          </span>
        </div>
      ))}
    </div>
  );
}

function MessageSlot({
  children,
  itemKey,
  isSearchMatch = false,
  isSearchActive = false,
  onSearchItemMount,
}: {
  children: ReactNode;
  itemKey?: string;
  isSearchMatch?: boolean;
  isSearchActive?: boolean;
  onSearchItemMount?: (itemKey: string, node: HTMLElement | null) => void;
}) {
  if (!itemKey) {
    return <>{children}</>;
  }

  return (
    <div
      className={`message-slot${isSearchMatch ? " session-search-hit" : ""}${isSearchActive ? " session-search-hit-active" : ""}`}
      data-session-search-item-key={itemKey}
      ref={(node) => {
        onSearchItemMount?.(itemKey, node);
      }}
    >
      {children}
    </div>
  );
}

function PanelEmptyState({ title, body }: { title: string; body: string }) {
  return (
    <article className="empty-state">
      <div className="card-label">Live State</div>
      <h3>{title}</h3>
      <p>{body}</p>
    </article>
  );
}

function buildVirtualizedMessageLayout(itemHeights: number[]) {
  const tops = new Array<number>(itemHeights.length);
  let offset = 0;

  for (let index = 0; index < itemHeights.length; index += 1) {
    tops[index] = offset;
    offset += itemHeights[index] + VIRTUALIZED_MESSAGE_GAP_PX;
  }

  return {
    tops,
    totalHeight: Math.max(offset - VIRTUALIZED_MESSAGE_GAP_PX, 0),
  };
}

function findVirtualizedMessageRange(
  tops: number[],
  itemHeights: number[],
  scrollTop: number,
  viewportHeight: number,
  overscan: number,
) {
  if (itemHeights.length === 0) {
    return {
      startIndex: 0,
      endIndex: 0,
    };
  }

  const startBoundary = Math.max(scrollTop - overscan, 0);
  const endBoundary =
    scrollTop + Math.max(viewportHeight, DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT) + overscan;

  let startIndex = 0;
  while (
    startIndex < itemHeights.length - 1 &&
    tops[startIndex] + itemHeights[startIndex] < startBoundary
  ) {
    startIndex += 1;
  }

  let endIndex = startIndex;
  while (endIndex < itemHeights.length && tops[endIndex] < endBoundary) {
    endIndex += 1;
  }

  return {
    startIndex,
    endIndex: Math.max(startIndex + 1, endIndex),
  };
}

function estimateConversationMessageHeight(message: Message) {
  switch (message.type) {
    case "text": {
      const lineCount = message.text.length === 0 ? 1 : message.text.split("\n").length;
      const attachmentHeight = (message.attachments?.length ?? 0) * 54;
      return Math.min(1800, Math.max(92, 78 + lineCount * 24 + attachmentHeight));
    }
    case "thinking":
      return Math.min(900, Math.max(140, 112 + message.lines.length * 28));
    case "command": {
      const commandLineCount = message.command.length === 0 ? 1 : message.command.split("\n").length;
      const outputLineCount = message.output ? message.output.split("\n").length : 3;
      return Math.min(
        1400,
        Math.max(180, 152 + commandLineCount * 22 + Math.min(outputLineCount, 14) * 20),
      );
    }
    case "diff": {
      const diffLineCount = message.diff.length === 0 ? 1 : message.diff.split("\n").length;
      return Math.min(1500, Math.max(180, 156 + Math.min(diffLineCount, 20) * 20));
    }
    case "markdown": {
      const markdownLineCount =
        message.markdown.length === 0 ? 1 : message.markdown.split("\n").length;
      return Math.min(1600, Math.max(140, 124 + markdownLineCount * 24));
    }
    case "approval":
      return Math.max(220, 188 + (message.detail.length === 0 ? 1 : message.detail.split("\n").length) * 22);
  }
}

function collectUserPromptHistory(session: Session) {
  return session.messages.flatMap((message) => {
    if (message.type !== "text" || message.author !== "you") {
      return [];
    }

    const prompt = message.text.trim();
    return prompt ? [prompt] : [];
  });
}

function imageAttachmentSummaryLabel(count: number) {
  return count === 1 ? "1 image attached" : `${count} images attached`;
}

function formatByteSize(byteSize: number) {
  if (byteSize < 1024) {
    return `${byteSize} B`;
  }

  if (byteSize < 1024 * 1024) {
    return `${(byteSize / 1024).toFixed(1)} KB`;
  }

  return `${(byteSize / (1024 * 1024)).toFixed(1)} MB`;
}
