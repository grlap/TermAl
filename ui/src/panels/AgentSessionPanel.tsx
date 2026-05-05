import {
  memo,
  useCallback,
  useDeferredValue,
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
} from "./session-message-leaves";
import {
  PendingPromptCard,
  RunningIndicator,
} from "./session-activity-cards";
import {
  VirtualizedConversationMessageList,
  type CodexAppRequestSubmitHandler,
  type McpElicitationSubmitHandler,
  type RenderMessageCard,
  type UserInputSubmitHandler,
  type VirtualizedConversationMessageListHandle,
} from "./VirtualizedConversationMessageList";
import { ConversationOverviewRail } from "./ConversationOverviewRail";
import { useConversationOverviewController } from "./conversation-overview-controller";
import {
  ConversationMarkerNavigator,
  ConversationMessageMarkers,
  MarkerPlusIcon,
  findMountedConversationMessageSlot,
  groupConversationMarkersByMessageId,
  sortConversationMarkersForNavigation,
} from "./conversation-markers";
import {
  renderHighlightedText,
  type SearchHighlightTone,
} from "../search-highlight";
import { findLastUserPrompt } from "../app-utils";
import {
  useComposerSessionSnapshot,
  useSessionRecordSnapshot,
} from "../session-store";
import { useStableEvent } from "./use-stable-event";
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
  ConversationMarker,
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

const EMPTY_PENDING_PROMPTS: readonly PendingPrompt[] = [];
const EMPTY_CONVERSATION_MARKERS: readonly ConversationMarker[] = [];
const NOOP_CREATE_CONVERSATION_MARKER = () => {};

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
const EMPTY_COMPOSER_ATTACHMENTS: readonly {
  byteSize: number;
  fileName: string;
  id: string;
  mediaType: string;
  previewUrl: string;
}[] = [];
const EMPTY_COMPOSER_PROMPT_HISTORY: readonly string[] = [];

/** @internal Exported for focused regression tests; not a cross-panel API. */
export function includeUndeferredMessageTail(
  deferredMessages: Message[],
  currentMessages: Message[],
) {
  if (deferredMessages === currentMessages) {
    return deferredMessages;
  }
  if (currentMessages.length === 0) {
    return currentMessages;
  }
  if (deferredMessages.length === 0) {
    return currentMessages;
  }

  const sharedLength = Math.min(deferredMessages.length, currentMessages.length);
  for (let index = 0; index < sharedLength; index += 1) {
    if (currentMessages[index]?.id !== deferredMessages[index]?.id) {
      return currentMessages;
    }
    if (currentMessages[index] !== deferredMessages[index]) {
      return [
        ...deferredMessages.slice(0, index),
        ...currentMessages.slice(index),
      ];
    }
  }

  if (currentMessages.length > deferredMessages.length) {
    return [
      ...deferredMessages,
      ...currentMessages.slice(deferredMessages.length),
    ];
  }

  if (deferredMessages.length > currentMessages.length) {
    return currentMessages;
  }

  return deferredMessages;
}

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
  activeSessionId,
  isLoading,
  isUpdating,
  showWaitingIndicator,
  waitingIndicatorPrompt,
  commandMessages,
  diffMessages,
  scrollContainerRef,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
  onCancelQueuedPrompt,
  onCreateConversationMarker = NOOP_CREATE_CONVERSATION_MARKER,
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
  activeSessionId: string | null;
  isLoading: boolean;
  isUpdating: boolean;
  showWaitingIndicator: boolean;
  waitingIndicatorPrompt: string | null;
  commandMessages: CommandMessage[];
  diffMessages: DiffMessage[];
  scrollContainerRef: RefObject<HTMLElement | null>;
  onApprovalDecision: (sessionId: string, messageId: string, decision: ApprovalDecision) => void;
  onUserInputSubmit: UserInputSubmitHandler;
  onMcpElicitationSubmit: McpElicitationSubmitHandler;
  onCodexAppRequestSubmit: CodexAppRequestSubmitHandler;
  onCancelQueuedPrompt: (sessionId: string, promptId: string) => void;
  onCreateConversationMarker?: (sessionId: string, messageId: string) => void;
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
  const stableOnApprovalDecision = useStableEvent(onApprovalDecision);
  const stableOnUserInputSubmit = useStableEvent(onUserInputSubmit);
  const stableOnMcpElicitationSubmit = useStableEvent(onMcpElicitationSubmit);
  const stableOnCodexAppRequestSubmit = useStableEvent(
    onCodexAppRequestSubmit,
  );
  const stableOnCancelQueuedPrompt = useStableEvent(onCancelQueuedPrompt);
  const stableOnCreateConversationMarker = useStableEvent(
    onCreateConversationMarker,
  );
  const stableOnSessionSettingsChange = useStableEvent(onSessionSettingsChange);

  return (
    <SessionBody
      paneId={paneId}
      viewMode={viewMode}
      scrollContainerRef={scrollContainerRef}
      activeSessionId={activeSessionId}
      isLoading={isLoading}
      isUpdating={isUpdating}
      showWaitingIndicator={showWaitingIndicator}
      waitingIndicatorPrompt={waitingIndicatorPrompt}
      commandMessages={commandMessages}
      diffMessages={diffMessages}
      onApprovalDecision={stableOnApprovalDecision}
      onUserInputSubmit={stableOnUserInputSubmit}
      onMcpElicitationSubmit={stableOnMcpElicitationSubmit}
      onCodexAppRequestSubmit={stableOnCodexAppRequestSubmit}
      onCancelQueuedPrompt={stableOnCancelQueuedPrompt}
      onCreateConversationMarker={stableOnCreateConversationMarker}
      onSessionSettingsChange={stableOnSessionSettingsChange}
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

export const AgentSessionPanelFooter = memo(function AgentSessionPanelFooter({
  paneId,
  viewMode,
  isPaneActive,
  activeSessionId,
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
  activeSessionId: string | null;
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
        sessionId={activeSessionId}
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
});

const SessionBody = memo(function SessionBody({
  paneId,
  viewMode,
  scrollContainerRef,
  activeSessionId,
  isLoading,
  isUpdating,
  showWaitingIndicator,
  waitingIndicatorPrompt,
  commandMessages,
  diffMessages,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
  onCancelQueuedPrompt,
  onCreateConversationMarker,
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
  activeSessionId: string | null;
  isLoading: boolean;
  isUpdating: boolean;
  showWaitingIndicator: boolean;
  waitingIndicatorPrompt: string | null;
  commandMessages: CommandMessage[];
  diffMessages: DiffMessage[];
  onApprovalDecision: (sessionId: string, messageId: string, decision: ApprovalDecision) => void;
  onUserInputSubmit: UserInputSubmitHandler;
  onMcpElicitationSubmit: McpElicitationSubmitHandler;
  onCodexAppRequestSubmit: CodexAppRequestSubmitHandler;
  onCancelQueuedPrompt: (sessionId: string, promptId: string) => void;
  onCreateConversationMarker: (sessionId: string, messageId: string) => void;
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
  const activeSession = useSessionRecordSnapshot(activeSessionId);
  const resolvedWaitingIndicatorPrompt =
    showWaitingIndicator &&
    activeSession &&
    (activeSession.status === "active" || activeSession.status === "approval")
      ? findLastUserPrompt(activeSession)
      : waitingIndicatorPrompt;

  if (!activeSession) {
    return (
      <PanelEmptyState
        title="Ready for a session"
        body="Click a session on the left to open it in the active tile."
      />
    );
  }

  if (viewMode === "session") {
    const activePendingPrompts =
      activeSession.pendingPrompts ?? EMPTY_PENDING_PROMPTS;
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
        <SessionConversationPage
          key={activeSession.id}
          renderMessageCard={renderMessageCard}
          session={activeSession}
          scrollContainerRef={scrollContainerRef}
          isActive
          isLoading={isLoading}
          showWaitingIndicator={showWaitingIndicator}
          waitingIndicatorPrompt={resolvedWaitingIndicatorPrompt}
          onApprovalDecision={onApprovalDecision}
          onUserInputSubmit={onUserInputSubmit}
          onMcpElicitationSubmit={onMcpElicitationSubmit}
          onCodexAppRequestSubmit={onCodexAppRequestSubmit}
          onCancelQueuedPrompt={onCancelQueuedPrompt}
          onCreateConversationMarker={onCreateConversationMarker}
          conversationSearchQuery={conversationSearchQuery}
          conversationSearchMatchedItemKeys={conversationSearchMatchedItemKeys}
          conversationSearchActiveItemKey={conversationSearchActiveItemKey}
          onConversationSearchItemMount={onConversationSearchItemMount}
        />
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
  previous.activeSessionId === next.activeSessionId &&
  previous.isLoading === next.isLoading &&
  previous.isUpdating === next.isUpdating &&
  previous.showWaitingIndicator === next.showWaitingIndicator &&
  previous.waitingIndicatorPrompt === next.waitingIndicatorPrompt &&
  previous.commandMessages === next.commandMessages &&
  previous.diffMessages === next.diffMessages &&
  previous.onApprovalDecision === next.onApprovalDecision &&
  previous.onUserInputSubmit === next.onUserInputSubmit &&
  previous.onMcpElicitationSubmit === next.onMcpElicitationSubmit &&
  previous.onCodexAppRequestSubmit === next.onCodexAppRequestSubmit &&
  previous.onCancelQueuedPrompt === next.onCancelQueuedPrompt &&
  previous.onCreateConversationMarker === next.onCreateConversationMarker &&
  previous.onSessionSettingsChange === next.onSessionSettingsChange &&
  previous.conversationSearchQuery === next.conversationSearchQuery &&
  previous.conversationSearchMatchedItemKeys === next.conversationSearchMatchedItemKeys &&
  previous.conversationSearchActiveItemKey === next.conversationSearchActiveItemKey &&
  previous.onConversationSearchItemMount === next.onConversationSearchItemMount &&
  (previous.viewMode !== "commands" ||
    previous.renderCommandCard === next.renderCommandCard) &&
  (previous.viewMode !== "diffs" ||
    previous.renderDiffCard === next.renderDiffCard) &&
  (previous.viewMode !== "session" ||
    previous.renderMessageCard === next.renderMessageCard) &&
  (previous.viewMode !== "prompt" ||
    previous.renderPromptSettings === next.renderPromptSettings)
  // Render callbacks are invoked during render, so they stay in normal React
  // dataflow. Compare only the renderer that can affect the active view mode;
  // event handlers above are committed stable callbacks. In session view the
  // message renderer intentionally tracks streaming flags, so active streaming
  // chunks can re-render SessionBody; the conversation page still defers the
  // visible message list before rendering heavy message content.
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
  onCreateConversationMarker,
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
  onCreateConversationMarker: (sessionId: string, messageId: string) => void;
  conversationSearchQuery: string;
  conversationSearchMatchedItemKeys: ReadonlySet<string>;
  conversationSearchActiveItemKey: string | null;
  onConversationSearchItemMount: (itemKey: string, node: HTMLElement | null) => void;
}) {
  const pendingPrompts = session.pendingPrompts ?? EMPTY_PENDING_PROMPTS;
  const deferredMessages = useDeferredValue(session.messages);
  const deferredPendingPrompts = useDeferredValue(pendingPrompts);
  const visibleMessages = isActive
    ? includeUndeferredMessageTail(deferredMessages, session.messages)
    : session.messages;
  const visiblePendingPromptsBase = isActive ? deferredPendingPrompts : pendingPrompts;
  const visiblePendingPrompts = useMemo(() => {
    if (visiblePendingPromptsBase.length === 0 || visibleMessages.length === 0) {
      return visiblePendingPromptsBase;
    }

    const visibleMessageIds = new Set(
      visibleMessages.map((message) => message.id),
    );
    const filteredPendingPrompts = visiblePendingPromptsBase.filter(
      (prompt) => !visibleMessageIds.has(prompt.id),
    );
    return filteredPendingPrompts.length === visiblePendingPromptsBase.length
      ? visiblePendingPromptsBase
      : filteredPendingPrompts;
  }, [visibleMessages, visiblePendingPromptsBase]);
  const conversationOverview = useConversationOverviewController({
    agent: session.agent,
    isActive,
    messageCount: visibleMessages.length,
    scrollContainerRef,
    sessionId: session.id,
    showWaitingIndicator,
    waitingIndicatorPrompt,
  });
  const visibleMarkers = session.markers ?? EMPTY_CONVERSATION_MARKERS;
  const markersByMessageId = useMemo(
    () => groupConversationMarkersByMessageId(visibleMarkers),
    [visibleMarkers],
  );
  const sortedMarkers = useMemo(
    () => sortConversationMarkersForNavigation(visibleMarkers, visibleMessages),
    [visibleMarkers, visibleMessages],
  );
  const [activeMarkerId, setActiveMarkerId] = useState<string | null>(null);
  const messageSlotNodesRef = useRef<Map<string, HTMLElement>>(new Map());
  const messageSlotNodesSessionIdRef = useRef(session.id);

  const ensureMessageSlotCacheForCurrentSession = useCallback(() => {
    if (messageSlotNodesSessionIdRef.current !== session.id) {
      messageSlotNodesRef.current = new Map();
      messageSlotNodesSessionIdRef.current = session.id;
    }
    return messageSlotNodesRef.current;
  }, [session.id]);

  useEffect(() => {
    if (
      activeMarkerId &&
      !visibleMarkers.some((marker) => marker.id === activeMarkerId)
    ) {
      setActiveMarkerId(null);
    }
  }, [activeMarkerId, visibleMarkers]);

  // Re-creating this ref callback on session changes is intentional: React
  // detaches and re-attaches mounted message slots, repopulating the per-session
  // marker jump cache after the layout-effect reset.
  const handleConversationItemMount = useCallback(
    (itemKey: string, node: HTMLElement | null) => {
      const messageId = itemKey.startsWith("message:")
        ? itemKey.slice("message:".length)
        : null;
      if (messageId) {
        const messageSlotNodes = ensureMessageSlotCacheForCurrentSession();
        if (node) {
          messageSlotNodes.set(messageId, node);
        } else {
          messageSlotNodes.delete(messageId);
        }
      }
      onConversationSearchItemMount(itemKey, node);
    },
    [ensureMessageSlotCacheForCurrentSession, onConversationSearchItemMount],
  );

  const jumpToMarker = useCallback(
    (marker: ConversationMarker) => {
      setActiveMarkerId(marker.id);
      const jumpedWithVirtualizer =
        conversationOverview.virtualizerHandleRef.current?.jumpToMessageId(
          marker.messageId,
          { align: "center" },
        ) ?? false;
      if (jumpedWithVirtualizer) {
        return;
      }
      const messageSlotNodes = ensureMessageSlotCacheForCurrentSession();
      (
        messageSlotNodes.get(marker.messageId) ??
        findMountedConversationMessageSlot(
          marker.messageId,
          scrollContainerRef.current ?? document,
        )
      )
        ?.scrollIntoView?.({ block: "center", behavior: "smooth" });
    },
    [
      conversationOverview.virtualizerHandleRef,
      ensureMessageSlotCacheForCurrentSession,
      scrollContainerRef,
    ],
  );

  const navigateMarkerByOffset = useCallback(
    (offset: -1 | 1) => {
      if (sortedMarkers.length === 0) {
        return;
      }
      const currentIndex =
        activeMarkerId === null
          ? -1
          : sortedMarkers.findIndex((marker) => marker.id === activeMarkerId);
      const fallbackIndex = offset > 0 ? 0 : sortedMarkers.length - 1;
      const nextIndex =
        currentIndex === -1
          ? fallbackIndex
          : (currentIndex + offset + sortedMarkers.length) %
            sortedMarkers.length;
      jumpToMarker(sortedMarkers[nextIndex]);
    },
    [activeMarkerId, jumpToMarker, sortedMarkers],
  );

  const renderMarkedMessageCard = useCallback<RenderMessageCard>(
    (
      message,
      preferImmediateHeavyRender,
      onMessageApprovalDecision,
      onMessageUserInputSubmit,
      onMessageMcpElicitationSubmit,
      onMessageCodexAppRequestSubmit,
    ) => {
      const rendered = renderMessageCard(
        message,
        preferImmediateHeavyRender,
        onMessageApprovalDecision,
        onMessageUserInputSubmit,
        onMessageMcpElicitationSubmit,
        onMessageCodexAppRequestSubmit,
      );
      const messageMarkers = markersByMessageId.get(message.id) ?? [];
      if (!rendered) {
        return null;
      }
      return (
        <div className="conversation-message-marker-shell">
          <div className="conversation-message-marker-toolbar">
            <button
              type="button"
              className="ghost-button conversation-message-marker-add-button"
              title="Add checkpoint marker"
              aria-label="Add checkpoint marker"
              onClick={() => onCreateConversationMarker(session.id, message.id)}
            >
              <MarkerPlusIcon />
            </button>
          </div>
          {messageMarkers.length > 0 ? (
            <ConversationMessageMarkers
              markers={messageMarkers}
              activeMarkerId={activeMarkerId}
              onMarkerClick={jumpToMarker}
            />
          ) : null}
          {rendered}
        </div>
      );
    },
    [
      activeMarkerId,
      jumpToMarker,
      markersByMessageId,
      onCreateConversationMarker,
      renderMessageCard,
      session.id,
    ],
  );

  if (visibleMessages.length === 0 && visiblePendingPrompts.length === 0 && !showWaitingIndicator) {
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

  const conversationMessages = (
    <ConversationMessageList
      renderMessageCard={renderMarkedMessageCard}
      sessionId={session.id}
      messages={visibleMessages}
      scrollContainerRef={scrollContainerRef}
      virtualizerHandleRef={
        conversationOverview.shouldRender
          ? conversationOverview.virtualizerHandleRef
          : undefined
      }
      isActive={isActive}
      onApprovalDecision={onApprovalDecision}
      onUserInputSubmit={onUserInputSubmit}
      onMcpElicitationSubmit={onMcpElicitationSubmit}
      onCodexAppRequestSubmit={onCodexAppRequestSubmit}
      conversationSearchQuery={conversationSearchQuery}
      conversationSearchMatchedItemKeys={conversationSearchMatchedItemKeys}
      conversationSearchActiveItemKey={conversationSearchActiveItemKey}
      onConversationSearchItemMount={handleConversationItemMount}
      />
  );
  const liveTurnCard = showWaitingIndicator ? (
    <RunningIndicator agent={session.agent} lastPrompt={waitingIndicatorPrompt} />
  ) : null;
  const pendingPromptCards = visiblePendingPrompts.map((prompt) => (
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
  ));
  const markerNavigation = sortedMarkers.length > 0 ? (
    <ConversationMarkerNavigator
      markers={sortedMarkers}
      activeMarkerId={activeMarkerId}
      onJump={jumpToMarker}
      onNavigatePrevious={() => navigateMarkerByOffset(-1)}
      onNavigateNext={() => navigateMarkerByOffset(1)}
    />
  ) : null;

  const conversationContent = (
    <>
      {markerNavigation}
      {conversationMessages}
      {liveTurnCard}
      {/* Only the active mounted page exposes find anchors so cached hidden pages cannot hijack scroll targets. */}
      {pendingPromptCards}
    </>
  );

  return (
    <div className={`session-conversation-page${isActive ? " is-active" : ""}`} hidden={!isActive}>
      {conversationOverview.shouldRender ? (
        <div className="conversation-with-overview">
          <div className="conversation-overview-content">
            {conversationContent}
          </div>
          <ConversationOverviewRail
            messages={visibleMessages}
            layoutSnapshot={conversationOverview.layoutSnapshot}
            viewportSnapshot={conversationOverview.viewportSnapshot}
            markers={visibleMarkers}
            tailItems={conversationOverview.tailItems}
            maxHeightPx={conversationOverview.maxHeightPx}
            onNavigate={conversationOverview.navigate}
          />
        </div>
      ) : (
        conversationContent
      )}
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
  virtualizerHandleRef,
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
  virtualizerHandleRef?: { current: VirtualizedConversationMessageListHandle | null };
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
      // Keying by sessionId remounts the virtualizer on every session switch
      // so all per-session scroll-intent refs and pending timers/RAFs reset
      // cleanly via the existing unmount cleanup. Without the key, refs like
      // `pendingMountedPrependRestoreRef`, `pendingDeferredLayoutAnchorRef`,
      // `pendingProgrammaticBottomFollowUntilRef`, the idle-compaction timer,
      // and the bottom-stick state survive across sessions in the same pane -
      // letting session A's queued restore corrupt session B's scroll, or an
      // A-armed timer mutate B's mounted range with stale flags.
      key={sessionId}
      isActive={isActive}
      renderMessageCard={renderMessageCard}
      sessionId={sessionId}
      messages={messages}
      scrollContainerRef={scrollContainerRef}
      virtualizerHandleRef={virtualizerHandleRef}
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
  sessionId,
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
  sessionId: string | null;
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
  const composerResizeAnimationFrameRef = useRef<number | null>(null);
  const composerResizeNeedsMetricRefreshRef = useRef(false);
  const composerLastMeasuredDraftLengthRef = useRef(0);
  const composerLastAppliedHeightRef = useRef<number | null>(null);
  const composerLastAppliedOverflowYRef = useRef<"auto" | "hidden" | null>(null);
  const composerSizingStateRef = useRef<{
    borderHeight: number;
    minHeight: number;
    panelElement: HTMLElement | null;
    panelSlotElement: HTMLElement | null;
  } | null>(null);
  const localDraftsRef = useRef<Record<string, string>>({});
  const committedDraftsRef = useRef<Record<string, string>>({});
  const onDraftCommitRef = useRef(onDraftCommit);
  const requestedSlashModelOptionsRef = useRef<string | null>(null);
  const requestedSlashAgentCommandsRef = useRef<string | null>(null);
  const slashOptionsRef = useRef<HTMLDivElement | null>(null);
  const session = useComposerSessionSnapshot(sessionId);
  // This state is intentionally narrow: it exists so slash-palette rendering
  // has a reactive draft. Plain prompt text lives in the uncontrolled textarea;
  // read the current prompt through `getComposerDraftValue()`.
  const [currentLocalDraftState, setCurrentLocalDraftState] = useState<{
    draft: string;
    sessionId: string | null;
  }>(() => {
    const initialSessionId = session?.id ?? sessionId;
    if (!initialSessionId) {
      return { draft: "", sessionId: null };
    }

    const initialCommittedDraft = session?.committedDraft ?? "";
    const initialLocalDraft = localDraftsRef.current[initialSessionId];
    const initialDraft =
      initialLocalDraft !== undefined ? initialLocalDraft : initialCommittedDraft;
    return {
      draft: initialDraft,
      sessionId: initialSessionId,
    };
  });
  const [promptHistoryStateBySessionId, setPromptHistoryStateBySessionId] = useState<
    Record<string, PromptHistoryState | undefined>
  >({});
  const [slashActiveIndex, setSlashActiveIndex] = useState(0);
  const [slashNavModality, setSlashNavModality] = useState<"keyboard" | "mouse">("keyboard");

  // `activeSessionId` is a best-effort identity for draft bookkeeping while
  // the store snapshot catches up. Callers that need capability/session fields
  // must still check `session`.
  const activeSessionId = session?.id ?? sessionId;
  const committedDraft = session?.committedDraft ?? "";
  const draftAttachments = session?.draftAttachments ?? EMPTY_COMPOSER_ATTACHMENTS;
  const promptHistory = session?.promptHistory ?? EMPTY_COMPOSER_PROMPT_HISTORY;
  const composerDraft =
    currentLocalDraftState.sessionId === activeSessionId
      ? currentLocalDraftState.draft
      : "";
  const initialComposerDraft = activeSessionId
    ? (localDraftsRef.current[activeSessionId] ?? committedDraft)
    : "";
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

  function cancelScheduledComposerResize() {
    if (composerResizeAnimationFrameRef.current == null) {
      return;
    }

    window.cancelAnimationFrame(composerResizeAnimationFrameRef.current);
    composerResizeAnimationFrameRef.current = null;
  }

  function getComposerSizingState(
    textarea: HTMLTextAreaElement,
    forceRefreshMetrics = false,
  ) {
    if (!composerSizingStateRef.current || forceRefreshMetrics) {
      const computedStyle = window.getComputedStyle(textarea);
      const panelElement = textarea.closest(".workspace-pane");
      const resolvedPanelElement =
        panelElement instanceof HTMLElement ? panelElement : null;
      const panelSlotElement =
        resolvedPanelElement?.parentElement instanceof HTMLElement
          ? resolvedPanelElement.parentElement
          : null;

      composerSizingStateRef.current = {
        minHeight: parseFloat(computedStyle.minHeight) || 0,
        borderHeight:
          (parseFloat(computedStyle.borderTopWidth) || 0) +
          (parseFloat(computedStyle.borderBottomWidth) || 0),
        panelElement: resolvedPanelElement,
        panelSlotElement,
      };
    }

    return composerSizingStateRef.current;
  }

  function resizeComposerInput(forceRefreshMetrics = false) {
    const textarea = composerInputRef.current;
    if (!textarea) {
      return;
    }

    const sizingState = getComposerSizingState(textarea, forceRefreshMetrics);
    const availablePanelHeight =
      sizingState.panelSlotElement?.clientHeight ??
      (sizingState.panelElement instanceof HTMLElement
        ? sizingState.panelElement.clientHeight
        : 0);
    const maxHeight = Math.max(
      sizingState.minHeight,
      availablePanelHeight > 0 ? availablePanelHeight * 0.4 : Number.POSITIVE_INFINITY,
    );
    const currentDraftLength = textarea.value.length;
    const shouldAllowShrink =
      forceRefreshMetrics ||
      currentDraftLength < composerLastMeasuredDraftLengthRef.current;
    if (shouldAllowShrink) {
      textarea.style.height = "0px";
      composerLastAppliedHeightRef.current = null;
    }

    const contentHeight = textarea.scrollHeight + sizingState.borderHeight;
    const nextHeight = Math.min(Math.max(contentHeight, sizingState.minHeight), maxHeight);
    const nextOverflowY: "auto" | "hidden" =
      contentHeight > maxHeight + 1 ? "auto" : "hidden";

    if (composerLastAppliedHeightRef.current !== nextHeight) {
      textarea.style.height = `${nextHeight}px`;
      composerLastAppliedHeightRef.current = nextHeight;
    }
    if (composerLastAppliedOverflowYRef.current !== nextOverflowY) {
      textarea.style.overflowY = nextOverflowY;
      composerLastAppliedOverflowYRef.current = nextOverflowY;
    }
    composerLastMeasuredDraftLengthRef.current = currentDraftLength;
  }

  function scheduleComposerResize(forceRefreshMetrics = false) {
    if (!activeSessionId) {
      return;
    }

    composerResizeNeedsMetricRefreshRef.current =
      composerResizeNeedsMetricRefreshRef.current || forceRefreshMetrics;
    if (composerResizeAnimationFrameRef.current != null) {
      return;
    }

    composerResizeAnimationFrameRef.current = window.requestAnimationFrame(() => {
      composerResizeAnimationFrameRef.current = null;
      const shouldRefreshMetrics = composerResizeNeedsMetricRefreshRef.current;
      composerResizeNeedsMetricRefreshRef.current = false;
      resizeComposerInput(shouldRefreshMetrics);
    });
  }

  useLayoutEffect(() => {
    composerSizingStateRef.current = null;
    composerResizeNeedsMetricRefreshRef.current = false;
    composerLastMeasuredDraftLengthRef.current = 0;
    composerLastAppliedHeightRef.current = null;
    composerLastAppliedOverflowYRef.current = null;
    cancelScheduledComposerResize();
    resizeComposerInput(true);

    return () => {
      cancelScheduledComposerResize();
    };
  }, [activeSessionId]);

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
      scheduleComposerResize(widthChanged || panelHeightChanged);
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
    return () => {
      composerSizingStateRef.current = null;
      composerResizeNeedsMetricRefreshRef.current = false;
      composerLastMeasuredDraftLengthRef.current = 0;
      composerLastAppliedHeightRef.current = null;
      composerLastAppliedOverflowYRef.current = null;
      cancelScheduledComposerResize();
    };
  }, []);

  useLayoutEffect(() => {
    if (!activeSessionId) {
      if (composerInputRef.current && composerInputRef.current.value !== "") {
        composerInputRef.current.value = "";
      }
      setCurrentLocalDraftState((current) =>
        current.sessionId === null && current.draft === ""
          ? current
          : { draft: "", sessionId: null },
      );
      scheduleComposerResize(true);
      return;
    }

    const previousCommitted = committedDraftsRef.current[activeSessionId];
    const localDraft = localDraftsRef.current[activeSessionId];

    committedDraftsRef.current[activeSessionId] = committedDraft;

    const nextDraft =
      localDraft !== undefined && localDraft !== previousCommitted
        ? localDraft
        : committedDraft;
    const textarea = composerInputRef.current;
    const didUpdateDomValue = Boolean(textarea && textarea.value !== nextDraft);
    if (didUpdateDomValue && textarea) {
      textarea.value = nextDraft;
    }
    setCurrentLocalDraftState((current) =>
      (!nextDraft.startsWith("/") &&
        current.sessionId === null &&
        current.draft === "") ||
      (current.sessionId === activeSessionId && current.draft === nextDraft)
        ? current
        : nextDraft.startsWith("/")
          ? {
              draft: nextDraft,
              sessionId: activeSessionId,
            }
          : { draft: "", sessionId: null },
    );
    if (didUpdateDomValue) {
      scheduleComposerResize(true);
    }
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
    localDraftsRef.current[sessionId] = nextValue;
    if (sessionId === activeSessionId) {
      if (composerInputRef.current && composerInputRef.current.value !== nextValue) {
        composerInputRef.current.value = nextValue;
      }
      setCurrentLocalDraftState((current) =>
        (!nextValue.startsWith("/") &&
          current.sessionId === null &&
          current.draft === "") ||
        (current.sessionId === sessionId && current.draft === nextValue)
          ? current
          : nextValue.startsWith("/")
            ? {
                draft: nextValue,
                sessionId,
              }
            : { draft: "", sessionId: null },
      );
      scheduleComposerResize();
    }
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
    if (!activeSessionId || !session || isSending || isStopping) {
      return;
    }

    if (item.kind === "command") {
      resetPromptHistory(activeSessionId);
      const nextDraft = `${item.command} `;
      updateLocalDraft(activeSessionId, nextDraft);
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
        resetPromptHistory(activeSessionId);
        const nextDraft = `/${item.name} `;
        updateLocalDraft(activeSessionId, nextDraft);
        focusComposerInput(nextDraft.length);
        return;
      }

      const visiblePrompt = (matchesSelectedCommand
        ? getComposerDraftValue()
        : `/${item.name}`).trim();
      const accepted =
        normalizedAgentCommandKind(agentCommand) === "nativeSlash"
          ? onSend(activeSessionId, visiblePrompt)
          : onSend(
              activeSessionId,
              visiblePrompt,
              agentCommand.content.split("$ARGUMENTS").join(
                matchesSelectedCommand ? (parsedDraft?.argumentsText ?? "") : "",
              ),
            );
      if (!accepted) {
        focusComposerInput();
        return;
      }

      resetPromptHistory(activeSessionId);
      updateLocalDraft(activeSessionId, "");
      commitDraft(activeSessionId, "");
      focusComposerInput();
      return;
    }

    if (isUpdating) {
      focusComposerInput(getComposerDraftValue().length);
      return;
    }

    resetPromptHistory(activeSessionId);
    void onSessionSettingsChange(activeSessionId, item.field, item.value);
    if (keepPaletteOpen) {
      focusComposerInput(getComposerDraftValue().length);
    } else {
      updateLocalDraft(activeSessionId, "");
      commitDraft(activeSessionId, "");
      focusComposerInput(0);
    }
  }

  function handleComposerSend() {
    if (!activeSessionId || isSending || isStopping) {
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
    const accepted = onSend(activeSessionId, draftToSend);
    if (!accepted) {
      focusComposerInput();
      return;
    }

    resetPromptHistory(activeSessionId);
    updateLocalDraft(activeSessionId, "");
    commitDraft(activeSessionId, "");
    focusComposerInput();
  }

  function handleComposerKeyDown(event: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (!activeSessionId) {
      return;
    }

    if (slashPalette.kind !== "none") {
      if (event.key === "Escape") {
        event.preventDefault();
        resetPromptHistory(activeSessionId);
        updateLocalDraft(activeSessionId, "");
        commitDraft(activeSessionId, "");
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

    if (promptHistory.length === 0) {
      return;
    }

    const historyState = promptHistoryStateBySessionId[activeSessionId];
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
        [activeSessionId]: {
          index: nextIndex,
          draft: draftSnapshot,
        },
      }));
      updateLocalDraft(activeSessionId, promptHistory[nextIndex]);
    } else {
      const currentHistoryState = historyState;
      if (!currentHistoryState) {
        return;
      }

      if (currentHistoryState.index >= promptHistory.length - 1) {
        resetPromptHistory(activeSessionId);
        updateLocalDraft(activeSessionId, currentHistoryState.draft);
      } else {
        const nextIndex = currentHistoryState.index + 1;
        setPromptHistoryStateBySessionId((current) => ({
          ...current,
          [activeSessionId]: {
            index: nextIndex,
            draft: currentHistoryState.draft,
          },
        }));
        updateLocalDraft(activeSessionId, promptHistory[nextIndex]);
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
                onClick={() => activeSessionId && onDraftAttachmentRemove(activeSessionId, attachment.id)}
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
          defaultValue={initialComposerDraft}
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
              onClick={() => activeSessionId && onStopSession(activeSessionId)}
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
  previous.sessionId === next.sessionId &&
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
  previous.showNewResponseIndicator === next.showNewResponseIndicator &&
  previous.onScrollToLatest === next.onScrollToLatest &&
  previous.onDraftCommit === next.onDraftCommit &&
  previous.onDraftAttachmentRemove === next.onDraftAttachmentRemove &&
  previous.onRefreshSessionModelOptions === next.onRefreshSessionModelOptions &&
  previous.onRefreshAgentCommands === next.onRefreshAgentCommands &&
  previous.onSend === next.onSend &&
  previous.onSessionSettingsChange === next.onSessionSettingsChange &&
  previous.onStopSession === next.onStopSession &&
  previous.onPaste === next.onPaste
);
