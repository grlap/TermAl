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
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
  type RefObject,
} from "react";
import { ExpandedPromptPanel } from "../ExpandedPromptPanel";
import { CONVERSATION_COMPOSER_INPUT_DATA_ATTRIBUTES } from "./conversation-composer-focus";
import {
  buildSlashPaletteState,
  parseAgentCommandDraft,
  supportsAgentSlashCommands,
  supportsLiveSessionModelOptions,
  type SlashPaletteItem,
} from "./session-slash-palette";
import {
  resolveAgentCommand,
  type ResolveAgentCommandResponse,
} from "../api";
import {
  findNewPendingCreatedConversationMarker,
  isSpaceKey,
  spawnDelegationOptionsFromResolvedCommand,
  type PendingCreatedConversationMarker,
  type SpawnDelegationOptions,
} from "./agent-session-panel-helpers";
import {
  formatAgentCommandResolverError,
  prepareAgentCommandSubmission,
  sendResolvedAgentCommandSubmission,
  shouldFocusDelegateWithSlashPaletteKey,
  shouldSubmitSlashPaletteKey,
} from "./session-agent-command-submission";
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
import {
  CONVERSATION_OVERVIEW_MIN_MESSAGES,
  ConversationOverviewRail,
} from "./ConversationOverviewRail";
import { useConversationOverviewController } from "./conversation-overview-controller";
import {
  ConversationMarkerFloatingWindow,
  findActivatableConversationMarkerContextMenuTrigger,
  findConversationMarkerContextMenuTrigger,
  groupConversationMarkersByMessageId,
  shouldOpenConversationMarkerContextMenu,
  sortConversationMarkersForNavigation,
  useConversationMarkerJump,
  useConversationMarkerContextMenu,
} from "./conversation-markers";
import { useComposerAutoResize } from "./useComposerAutoResize";
import {
  MessageNavigationProvider,
  makeMessageNavigationLookup,
  useMessageNavigationTargetMaps,
  type MessageNavigationContextValue,
} from "./conversation-navigation";
import {
  renderHighlightedText,
  type SearchHighlightTone,
} from "../search-highlight";
import { resolveLiveWaitingIndicatorPrompt } from "../app-utils";
import {
  hasAgentOutputAfterLatestUserPrompt,
  hasTurnFinalizingOutputAfterLatestUserPrompt,
} from "../SessionPaneView.waiting-indicator";
import {
  useComposerSessionSnapshot,
  useSessionRecordSnapshot,
} from "../session-store";
import { useStableEvent } from "./use-stable-event";
import {
  includeUndeferredMessageTail,
  useInitialActiveTranscriptMessages,
} from "./useInitialActiveTranscriptMessages";
import { MessageMetaMarkerMenuProvider } from "../message-cards";
import { normalizeConversationMarkerColor } from "../conversation-marker-colors";
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
  CreateConversationMarkerOptions,
} from "../types";
import type { PaneViewMode } from "../workspace";

export { splitAgentCommandResolverTail } from "./session-agent-command-submission";
type WaitingIndicatorKind = "liveTurn" | "delegationWait" | "send";

type DraftImageAttachment = ImageAttachment & {
  base64Data: string;
  id: string;
  previewUrl: string;
};

type PromptHistoryState = {
  index: number;
  draft: string;
};

type AgentCommandResolverErrorState = {
  message: string;
  sessionId: string;
};

const EMPTY_PENDING_PROMPTS: readonly PendingPrompt[] = [];
const EMPTY_CONVERSATION_MARKERS: readonly ConversationMarker[] = [];
const NOOP_CREATE_CONVERSATION_MARKER = () => {};
const NOOP_DELETE_CONVERSATION_MARKER = () => {};

type CreateConversationMarkerHandlerResult =
  | boolean
  | void
  | Promise<boolean | void>;

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

type SpawnDelegationHandler = (
  sessionId: string,
  prompt: string,
  options?: SpawnDelegationOptions,
) => Promise<boolean>;

// The transcript virtualizer and overview rail intentionally share the same
// size threshold. The rail may still defer its first paint, but marker jumps
// need the virtualizer handle as soon as the transcript itself virtualizes.
const CONVERSATION_VIRTUALIZATION_MIN_MESSAGES =
  CONVERSATION_OVERVIEW_MIN_MESSAGES;
const EMPTY_COMPOSER_ATTACHMENTS: readonly {
  byteSize: number;
  fileName: string;
  id: string;
  mediaType: string;
  previewUrl: string;
}[] = [];
const EMPTY_COMPOSER_PROMPT_HISTORY: readonly string[] = [];

export function AgentSessionPanel({
  paneId,
  viewMode,
  activeSessionId,
  liveTailPinned = true,
  isLoading,
  isUpdating,
  showWaitingIndicator,
  waitingIndicatorKind = "liveTurn",
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
  onDeleteConversationMarker = NOOP_DELETE_CONVERSATION_MARKER,
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
  liveTailPinned?: boolean;
  isLoading: boolean;
  isUpdating: boolean;
  showWaitingIndicator: boolean;
  waitingIndicatorKind?: WaitingIndicatorKind;
  waitingIndicatorPrompt: string | null;
  commandMessages: CommandMessage[];
  diffMessages: DiffMessage[];
  scrollContainerRef: RefObject<HTMLElement | null>;
  onApprovalDecision: (sessionId: string, messageId: string, decision: ApprovalDecision) => void;
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
  const stableOnDeleteConversationMarker = useStableEvent(
    onDeleteConversationMarker,
  );
  const stableOnSessionSettingsChange = useStableEvent(onSessionSettingsChange);

  return (
    <SessionBody
      paneId={paneId}
      viewMode={viewMode}
      scrollContainerRef={scrollContainerRef}
      activeSessionId={activeSessionId}
      liveTailPinned={liveTailPinned}
      isLoading={isLoading}
      isUpdating={isUpdating}
      showWaitingIndicator={showWaitingIndicator}
      waitingIndicatorKind={waitingIndicatorKind}
      waitingIndicatorPrompt={waitingIndicatorPrompt}
      commandMessages={commandMessages}
      diffMessages={diffMessages}
      onApprovalDecision={stableOnApprovalDecision}
      onUserInputSubmit={stableOnUserInputSubmit}
      onMcpElicitationSubmit={stableOnMcpElicitationSubmit}
      onCodexAppRequestSubmit={stableOnCodexAppRequestSubmit}
      onCancelQueuedPrompt={stableOnCancelQueuedPrompt}
      onCreateConversationMarker={stableOnCreateConversationMarker}
      onDeleteConversationMarker={stableOnDeleteConversationMarker}
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
  newResponseIndicatorLabel,
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
  canSpawnDelegation = false,
  onSpawnDelegation,
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
  onSend: (sessionId: string, draftText?: string, expandedText?: string | null) => boolean;
  canSpawnDelegation?: boolean;
  onSpawnDelegation?: SpawnDelegationHandler;
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
        newResponseIndicatorLabel={newResponseIndicatorLabel}
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
        canSpawnDelegation={canSpawnDelegation}
        onSpawnDelegation={onSpawnDelegation}
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
  liveTailPinned,
  isLoading,
  isUpdating,
  showWaitingIndicator,
  waitingIndicatorKind,
  waitingIndicatorPrompt,
  commandMessages,
  diffMessages,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
  onCancelQueuedPrompt,
  onCreateConversationMarker,
  onDeleteConversationMarker,
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
  liveTailPinned: boolean;
  isLoading: boolean;
  isUpdating: boolean;
  showWaitingIndicator: boolean;
  waitingIndicatorKind: WaitingIndicatorKind;
  waitingIndicatorPrompt: string | null;
  commandMessages: CommandMessage[];
  diffMessages: DiffMessage[];
  onApprovalDecision: (sessionId: string, messageId: string, decision: ApprovalDecision) => void;
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
  const activeSessionMessages = activeSession?.messages;
  const activeSessionStatus = activeSession?.status;
  const shouldResolveLiveWaitingPrompt =
    showWaitingIndicator &&
    waitingIndicatorKind === "liveTurn" &&
    activeSessionStatus === "active";
  const liveWaitingIndicatorPrompt = useMemo(
    () =>
      shouldResolveLiveWaitingPrompt && activeSessionMessages
        ? resolveLiveWaitingIndicatorPrompt({
            messages: activeSessionMessages,
            status: "active",
          })
        : null,
    [activeSessionMessages, activeSessionStatus, shouldResolveLiveWaitingPrompt],
  );
  const resolvedWaitingIndicatorPrompt = shouldResolveLiveWaitingPrompt
    ? liveWaitingIndicatorPrompt
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
          liveTailPinned={liveTailPinned}
          scrollContainerRef={scrollContainerRef}
          isActive
          isLoading={isLoading}
          showWaitingIndicator={showWaitingIndicator}
          waitingIndicatorKind={waitingIndicatorKind}
          waitingIndicatorPrompt={resolvedWaitingIndicatorPrompt}
          onApprovalDecision={onApprovalDecision}
          onUserInputSubmit={onUserInputSubmit}
          onMcpElicitationSubmit={onMcpElicitationSubmit}
          onCodexAppRequestSubmit={onCodexAppRequestSubmit}
          onCancelQueuedPrompt={onCancelQueuedPrompt}
          onCreateConversationMarker={onCreateConversationMarker}
          onDeleteConversationMarker={onDeleteConversationMarker}
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
  previous.liveTailPinned === next.liveTailPinned &&
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
  previous.onDeleteConversationMarker === next.onDeleteConversationMarker &&
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
  liveTailPinned,
  scrollContainerRef,
  isActive,
  isLoading,
  showWaitingIndicator,
  waitingIndicatorKind,
  waitingIndicatorPrompt,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
  onCancelQueuedPrompt,
  onCreateConversationMarker,
  onDeleteConversationMarker,
  conversationSearchQuery,
  conversationSearchMatchedItemKeys,
  conversationSearchActiveItemKey,
  onConversationSearchItemMount,
}: {
  renderMessageCard: RenderMessageCard;
  session: Session;
  liveTailPinned: boolean;
  scrollContainerRef: RefObject<HTMLElement | null>;
  isActive: boolean;
  isLoading: boolean;
  showWaitingIndicator: boolean;
  waitingIndicatorKind: WaitingIndicatorKind;
  waitingIndicatorPrompt: string | null;
  onApprovalDecision: (sessionId: string, messageId: string, decision: ApprovalDecision) => void;
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
  onConversationSearchItemMount: (itemKey: string, node: HTMLElement | null) => void;
}) {
  const pendingPrompts = session.pendingPrompts ?? EMPTY_PENDING_PROMPTS;
  const deferredMessages = useDeferredValue(session.messages);
  const deferredPendingPrompts = useDeferredValue(pendingPrompts);
  const visibleMarkers = session.markers ?? EMPTY_CONVERSATION_MARKERS;
  const hasConversationSearch =
    conversationSearchQuery.trim().length > 0 ||
    conversationSearchMatchedItemKeys.size > 0 ||
    conversationSearchActiveItemKey !== null;
  const baseVisibleMessages = isActive
    ? includeUndeferredMessageTail(deferredMessages, session.messages)
    : session.messages;
  const {
    isWindowed: isInitialTranscriptWindowActive,
    messages: visibleMessages,
    requestFullTranscriptRender,
  } = useInitialActiveTranscriptMessages({
    hasConversationMarkers: visibleMarkers.length > 0,
    hasConversationSearch,
    isActive,
    messages: baseVisibleMessages,
    scrollContainerRef,
    sessionId: session.id,
  });
  const overviewMessages = isInitialTranscriptWindowActive
    ? baseVisibleMessages
    : visibleMessages;
  const visiblePendingPromptsBase = isActive ? deferredPendingPrompts : pendingPrompts;
  const visibleMessageIds = useMemo(
    () => new Set(visibleMessages.map((message) => message.id)),
    [visibleMessages],
  );
  const visiblePendingPrompts = useMemo(() => {
    if (visiblePendingPromptsBase.length === 0 || visibleMessages.length === 0) {
      return visiblePendingPromptsBase;
    }

    const filteredPendingPrompts = visiblePendingPromptsBase.filter(
      (prompt) => !visibleMessageIds.has(prompt.id),
    );
    return filteredPendingPrompts.length === visiblePendingPromptsBase.length
      ? visiblePendingPromptsBase
      : filteredPendingPrompts;
  }, [visibleMessages.length, visibleMessageIds, visiblePendingPromptsBase]);
  const effectiveShowWaitingIndicator =
    showWaitingIndicator &&
    (waitingIndicatorKind === "delegationWait" ||
      waitingIndicatorKind === "send" ||
      (session.status === "active" &&
        !hasTurnFinalizingOutputAfterLatestUserPrompt(visibleMessages)) ||
      !hasAgentOutputAfterLatestUserPrompt(visibleMessages));
  const conversationOverview = useConversationOverviewController({
    agent: session.agent,
    isActive,
    messageCount: overviewMessages.length,
    onFullTranscriptDemand: requestFullTranscriptRender,
    scrollContainerRef,
    sessionId: session.id,
    showWaitingIndicator: effectiveShowWaitingIndicator,
    waitingIndicatorPrompt: effectiveShowWaitingIndicator
      ? waitingIndicatorPrompt
      : null,
  });
  const markersByMessageId = useMemo(
    () => groupConversationMarkersByMessageId(visibleMarkers),
    [visibleMarkers],
  );
  const sortedMarkers = useMemo(
    () => sortConversationMarkersForNavigation(visibleMarkers, visibleMessages),
    [visibleMarkers, visibleMessages],
  );
  // activeMarkerId selects a persisted marker in the rail; activeMarkerMessageId
  // drives the message-shell highlight immediately, including the create flow
  // before the backend emits the new marker. pendingCreatedMarkers tie that
  // immediate highlight to markers that appear after create so color follows the
  // newest create instead of an older marker on the same message.
  const [activeMarkerId, setActiveMarkerId] = useState<string | null>(null);
  const [activeMarkerMessageId, setActiveMarkerMessageId] = useState<
    string | null
  >(null);
  const [pendingCreatedMarkers, setPendingCreatedMarkers] = useState<
    PendingCreatedConversationMarker[]
  >([]);
  const pendingCreatedMarkersRef = useRef<PendingCreatedConversationMarker[]>([]);
  const pendingCreatedMarkerSequenceRef = useRef(0);
  const [markerPanelVisibilityOverride, setMarkerPanelVisibilityOverride] =
    useState<boolean | null>(null);
  const conversationPageRef = useRef<HTMLDivElement | null>(null);
  const markerPanelFocusRestoreFrameRef = useRef<number | null>(null);
  // null follows the auto-show heuristic; explicit booleans come from the
  // message-header context menu.
  const isMarkerPanelVisible =
    markerPanelVisibilityOverride ?? sortedMarkers.length > 0;
  const setPendingConversationMarkerCreates = useCallback(
    (nextPendingMarkers: PendingCreatedConversationMarker[]) => {
      pendingCreatedMarkersRef.current = nextPendingMarkers;
      setPendingCreatedMarkers(nextPendingMarkers);
    },
    [],
  );
  const clearPendingConversationMarkerCreate = useCallback(
    (localId: number) => {
      const currentPendingMarkers = pendingCreatedMarkersRef.current;
      const failedMarker = currentPendingMarkers.find(
        (marker) => marker.localId === localId,
      );
      if (!failedMarker) {
        return;
      }
      const nextPendingMarkers = currentPendingMarkers.filter(
        (marker) => marker.localId !== localId,
      );
      setPendingConversationMarkerCreates(nextPendingMarkers);
      if (
        currentPendingMarkers[currentPendingMarkers.length - 1]?.localId ===
        localId
      ) {
        setActiveMarkerMessageId(
          nextPendingMarkers[nextPendingMarkers.length - 1]?.messageId ?? null,
        );
      }
    },
    [setPendingConversationMarkerCreates],
  );
  const handleCreateConversationMarker = useCallback(
    (
      targetSessionId: string,
      messageId: string,
      options?: CreateConversationMarkerOptions,
    ) => {
      let localPendingMarkerId: number | null = null;
      if (targetSessionId === session.id) {
        const messageMarkers = markersByMessageId.get(messageId) ?? [];
        pendingCreatedMarkerSequenceRef.current += 1;
        localPendingMarkerId = pendingCreatedMarkerSequenceRef.current;
        setActiveMarkerId(null);
        setActiveMarkerMessageId(messageId);
        setPendingConversationMarkerCreates([
          ...pendingCreatedMarkersRef.current,
          {
            localId: localPendingMarkerId,
            messageId,
            name: options?.name?.trim() || null,
            existingMarkerIds: new Set(
              messageMarkers.map((marker) => marker.id),
            ),
          },
        ]);
      }
      const createResult = onCreateConversationMarker(
        targetSessionId,
        messageId,
        options,
      );
      if (localPendingMarkerId !== null) {
        void Promise.resolve(createResult).then(
          (accepted) => {
            if (accepted === false) {
              clearPendingConversationMarkerCreate(localPendingMarkerId);
            }
          },
          () => clearPendingConversationMarkerCreate(localPendingMarkerId),
        );
      }
    },
    [
      clearPendingConversationMarkerCreate,
      markersByMessageId,
      onCreateConversationMarker,
      session.id,
      setPendingConversationMarkerCreates,
    ],
  );
  const {
    contextMenuNode: markerContextMenuNode,
    openContextMenu: openMarkerContextMenu,
  } = useConversationMarkerContextMenu({
    isActive,
    isMarkerPanelVisible,
    markersByMessageId,
    onCreateConversationMarker: handleCreateConversationMarker,
    onDeleteConversationMarker,
    onSetMarkerPanelVisible: setMarkerPanelVisibilityOverride,
    scrollContainerRef,
    sessionId: session.id,
    visibleMessageIds,
  });
  const {
    handleConversationItemMount,
    jumpToMarker: jumpToConversationMarker,
    jumpToMessageId,
  } = useConversationMarkerJump({
    onMissingMessageJump: requestFullTranscriptRender,
    onConversationSearchItemMount,
    scrollContainerRef,
    sessionId: session.id,
    virtualizerHandleRef: conversationOverview.virtualizerHandleRef,
  });
  // Build navigation target maps from the full transcript, not the windowed
  // tail. The initial-transcript window can be as small as
  // `SESSION_TAIL_WINDOW_MESSAGE_COUNT` (20) messages, so navigating delegations
  // / prompts based on the window would silently skip anything off-window.
  // Off-window targets trigger full-transcript hydration and a retry in
  // `useConversationMarkerJump`, so buttons remain accurate while the first
  // paint stays cheap.
  const messageNavigationTargetMaps = useMessageNavigationTargetMaps(
    session.messages,
  );
  // The lookup closure stays stable across renders that don't replace the
  // target maps, so memoized message cards consuming the context can stay
  // memo-hits as long as their own props haven't changed.
  const messageNavigationContextValue = useMemo<MessageNavigationContextValue>(
    () => ({
      getNavigationTargets: makeMessageNavigationLookup(
        messageNavigationTargetMaps,
      ),
      jumpToMessageId,
    }),
    [jumpToMessageId, messageNavigationTargetMaps],
  );

  useEffect(() => {
    if (
      activeMarkerId &&
      !visibleMarkers.some((marker) => marker.id === activeMarkerId)
    ) {
      setActiveMarkerId(null);
      setActiveMarkerMessageId(null);
      setPendingConversationMarkerCreates([]);
    }
  }, [activeMarkerId, setPendingConversationMarkerCreates, visibleMarkers]);

  useEffect(() => {
    if (pendingCreatedMarkers.length === 0 || activeMarkerId) {
      return;
    }
    const usedMarkerIds = new Set<string>();
    let changed = false;
    const nextPendingMarkers = pendingCreatedMarkers.map((pendingMarker) => {
      if (pendingMarker.resolvedMarkerId) {
        usedMarkerIds.add(pendingMarker.resolvedMarkerId);
        return pendingMarker;
      }
      const messageMarkers =
        markersByMessageId.get(pendingMarker.messageId) ?? [];
      const createdMarker = findNewPendingCreatedConversationMarker(
        messageMarkers,
        pendingMarker,
        usedMarkerIds,
      );
      if (!createdMarker) {
        return pendingMarker;
      }
      usedMarkerIds.add(createdMarker.id);
      changed = true;
      return {
        ...pendingMarker,
        resolvedMarkerId: createdMarker.id,
      };
    });
    const latestPendingMarker =
      nextPendingMarkers[nextPendingMarkers.length - 1] ?? null;
    if (latestPendingMarker?.resolvedMarkerId) {
      setActiveMarkerId(latestPendingMarker.resolvedMarkerId);
      setActiveMarkerMessageId(latestPendingMarker.messageId);
      setPendingConversationMarkerCreates([]);
      return;
    }
    if (changed) {
      setPendingConversationMarkerCreates(nextPendingMarkers);
    }
  }, [
    activeMarkerId,
    markersByMessageId,
    pendingCreatedMarkers,
    setPendingConversationMarkerCreates,
  ]);

  useEffect(() => {
    setActiveMarkerId(null);
    setActiveMarkerMessageId(null);
    setPendingConversationMarkerCreates([]);
    setMarkerPanelVisibilityOverride(null);
  }, [session.id, setPendingConversationMarkerCreates]);

  const cancelMarkerPanelFocusRestore = useCallback(() => {
    if (markerPanelFocusRestoreFrameRef.current !== null) {
      window.cancelAnimationFrame(markerPanelFocusRestoreFrameRef.current);
      markerPanelFocusRestoreFrameRef.current = null;
    }
  }, []);

  useEffect(() => {
    return cancelMarkerPanelFocusRestore;
  }, [cancelMarkerPanelFocusRestore, session.id]);

  const hideMarkerPanelAndRestoreFocus = useCallback(() => {
    setMarkerPanelVisibilityOverride(false);
    cancelMarkerPanelFocusRestore();
    markerPanelFocusRestoreFrameRef.current = window.requestAnimationFrame(() => {
      markerPanelFocusRestoreFrameRef.current = null;
      conversationPageRef.current?.focus({ preventScroll: true });
    });
  }, [cancelMarkerPanelFocusRestore]);

  const jumpToMarker = useCallback(
    (marker: ConversationMarker) => {
      setActiveMarkerId(marker.id);
      setActiveMarkerMessageId(marker.messageId);
      setPendingConversationMarkerCreates([]);
      jumpToConversationMarker(marker);
    },
    [jumpToConversationMarker, setPendingConversationMarkerCreates],
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
      if (!rendered) {
        return null;
      }
      const messageMarkers = markersByMessageId.get(message.id) ?? [];
      const latestPendingCreatedMarker =
        pendingCreatedMarkers[pendingCreatedMarkers.length - 1] ?? null;
      const pendingActiveMessageMarker =
        !activeMarkerId &&
        latestPendingCreatedMarker?.messageId === message.id &&
        latestPendingCreatedMarker.resolvedMarkerId
          ? messageMarkers.find(
              (marker) =>
                marker.id === latestPendingCreatedMarker.resolvedMarkerId,
            ) ?? null
          : null;
      const activeMessageMarker = activeMarkerId
        ? messageMarkers.find((marker) => marker.id === activeMarkerId) ?? null
        : pendingActiveMessageMarker;
      const isActiveMarkerMessage =
        activeMessageMarker !== null || activeMarkerMessageId === message.id;
      const activeMarkerColor = activeMessageMarker?.color ?? null;
      const markerShellStyle = isActiveMarkerMessage
        ? ({
            "--conversation-active-marker-color":
              normalizeConversationMarkerColor(activeMarkerColor),
          } as CSSProperties)
        : undefined;
      // Markers are message-scoped, so all rendered message authors are
      // eligible. Nested native controls still keep their own context menu.
      const handleMarkerContextMenu = (event: ReactMouseEvent<HTMLDivElement>) => {
        if (!shouldOpenConversationMarkerContextMenu(event)) {
          return;
        }
        const trigger = findConversationMarkerContextMenuTrigger(
          event.currentTarget,
          event.target,
        );
        if (!trigger) {
          return;
        }
        event.preventDefault();
        openMarkerContextMenu({
          messageId: message.id,
          clientX: event.clientX,
          clientY: event.clientY,
          trigger,
        });
      };
      const openMarkerMenuFromTrigger = (
        trigger: HTMLElement,
        clientX: number,
        clientY: number,
      ) => {
        openMarkerContextMenu({
          messageId: message.id,
          clientX,
          clientY,
          trigger,
        });
      };
      const handleMarkerTriggerClick = (event: ReactMouseEvent<HTMLDivElement>) => {
        if (event.button !== 0) {
          return;
        }
        const trigger = findActivatableConversationMarkerContextMenuTrigger(
          event.currentTarget,
          event.target,
        );
        if (!trigger) {
          return;
        }
        event.preventDefault();
        openMarkerMenuFromTrigger(trigger, event.clientX, event.clientY);
      };
      const handleMarkerTriggerKeyDown = (
        event: ReactKeyboardEvent<HTMLDivElement>,
      ) => {
        if (
          event.key !== "Enter" &&
          event.key !== " " &&
          event.key !== "ContextMenu"
        ) {
          return;
        }
        const trigger = findActivatableConversationMarkerContextMenuTrigger(
          event.currentTarget,
          event.target,
        );
        if (!trigger) {
          return;
        }
        event.preventDefault();
        const rect = trigger.getBoundingClientRect();
        openMarkerMenuFromTrigger(trigger, rect.left, rect.bottom);
      };
      return (
        <div
          className={`conversation-message-marker-shell can-open-marker-menu${isActiveMarkerMessage ? " is-active-marker" : ""}`}
          style={markerShellStyle}
          tabIndex={-1}
          onClick={handleMarkerTriggerClick}
          onContextMenu={handleMarkerContextMenu}
          onKeyDown={handleMarkerTriggerKeyDown}
        >
          <MessageMetaMarkerMenuProvider>
            {rendered}
          </MessageMetaMarkerMenuProvider>
        </div>
      );
    },
    [
      // The marker menu owns session/create/delete state internally; keep this
      // callback keyed only to the rendered card and marker lookup surfaces.
      activeMarkerId,
      activeMarkerMessageId,
      markersByMessageId,
      openMarkerContextMenu,
      pendingCreatedMarkers,
      renderMessageCard,
    ],
  );

  if (visibleMessages.length === 0 && visiblePendingPrompts.length === 0 && !effectiveShowWaitingIndicator) {
    return (
      <div
        ref={conversationPageRef}
        className={`session-conversation-page${isActive ? " is-active" : ""}`}
        hidden={!isActive}
        tabIndex={-1}
      >
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

  const isConversationVirtualized =
    isInitialTranscriptWindowActive ||
    visibleMessages.length >= CONVERSATION_VIRTUALIZATION_MIN_MESSAGES;
  const conversationMessages = (
    <ConversationMessageList
      renderMessageCard={renderMarkedMessageCard}
      sessionId={session.id}
      messages={visibleMessages}
      scrollContainerRef={scrollContainerRef}
      tailFollowIntent={liveTailPinned}
      virtualizerHandleRef={
        // Marker jumps need the virtualizer handle whenever the transcript is
        // virtualized, even while the overview rail is still deferred.
        isConversationVirtualized
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
      forceVirtualized={isInitialTranscriptWindowActive}
    />
  );
  const liveTurnCard = effectiveShowWaitingIndicator ? (
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
        onCancel={
          prompt.localOnly
            ? undefined
            : () => onCancelQueuedPrompt(session.id, prompt.id)
        }
        searchQuery={
          conversationSearchActiveItemKey === `pendingPrompt:${prompt.id}` ? conversationSearchQuery : ""
        }
        searchHighlightTone={
          conversationSearchActiveItemKey === `pendingPrompt:${prompt.id}` ? "active" : "match"
        }
      />
    </MessageSlot>
  ));
  const pendingPromptQueue =
    pendingPromptCards.length > 0 ? (
      <div className="conversation-pending-prompts">
        {/* Only the active mounted page exposes find anchors so cached hidden pages cannot hijack scroll targets. */}
        {pendingPromptCards}
      </div>
    ) : null;
  const liveTail =
    liveTurnCard ? (
      <div className={`conversation-live-tail${liveTailPinned ? " is-pinned" : ""}`}>
        {/* Keep queued follow-ups pinned with the live status card; otherwise
           they scroll away while the active turn remains sticky. */}
        {pendingPromptQueue}
        {liveTurnCard}
      </div>
    ) : null;
  const markerNavigation = isMarkerPanelVisible ? (
    <ConversationMarkerFloatingWindow
      markers={sortedMarkers}
      activeMarkerId={activeMarkerId}
      onClose={hideMarkerPanelAndRestoreFocus}
      onJump={jumpToMarker}
      onNavigatePrevious={() => navigateMarkerByOffset(-1)}
      onNavigateNext={() => navigateMarkerByOffset(1)}
    />
  ) : null;
  const conversationContent = (
    <>
      {markerNavigation}
      {conversationMessages}
      {markerContextMenuNode}
      {liveTail ? null : pendingPromptQueue}
      {liveTail}
    </>
  );
  const conversationPageClassName = `session-conversation-page${isActive ? " is-active" : ""}${conversationOverview.shouldRender ? " has-conversation-overview-scroll" : ""}`;

  return (
    <MessageNavigationProvider value={messageNavigationContextValue}>
      <div
        ref={conversationPageRef}
        className={conversationPageClassName}
        hidden={!isActive}
        tabIndex={-1}
      >
        {conversationOverview.shouldRender ? (
          <div className="conversation-with-overview">
            <div className="conversation-overview-content">
              {conversationContent}
            </div>
            {conversationOverview.shouldRenderRail ? (
              <ConversationOverviewRail
                messages={overviewMessages}
                layoutSnapshot={conversationOverview.layoutSnapshot}
                viewportSnapshot={conversationOverview.viewportSnapshot}
                markers={visibleMarkers}
                tailItems={conversationOverview.tailItems}
                maxHeightPx={conversationOverview.maxHeightPx}
                onNavigate={conversationOverview.navigate}
              />
            ) : (
              <div
                aria-hidden="true"
                className="conversation-overview-rail is-pending"
                style={{ height: `${Math.ceil(conversationOverview.maxHeightPx)}px` }}
              />
            )}
          </div>
        ) : (
          conversationContent
        )}
      </div>
    </MessageNavigationProvider>
  );
}, (previous, next) =>
  previous.renderMessageCard === next.renderMessageCard &&
  previous.session === next.session &&
  previous.liveTailPinned === next.liveTailPinned &&
  previous.scrollContainerRef === next.scrollContainerRef &&
  previous.isActive === next.isActive &&
  previous.isLoading === next.isLoading &&
  previous.showWaitingIndicator === next.showWaitingIndicator &&
  previous.waitingIndicatorPrompt === next.waitingIndicatorPrompt &&
  previous.onUserInputSubmit === next.onUserInputSubmit &&
  previous.onMcpElicitationSubmit === next.onMcpElicitationSubmit &&
  previous.onCodexAppRequestSubmit === next.onCodexAppRequestSubmit &&
  previous.onCreateConversationMarker === next.onCreateConversationMarker &&
  previous.onDeleteConversationMarker === next.onDeleteConversationMarker &&
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
  tailFollowIntent,
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
  forceVirtualized = false,
}: {
  renderMessageCard: RenderMessageCard;
  sessionId: string;
  messages: Message[];
  scrollContainerRef: RefObject<HTMLElement | null>;
  tailFollowIntent: boolean;
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
  forceVirtualized?: boolean;
}) {
  if (!forceVirtualized && messages.length < CONVERSATION_VIRTUALIZATION_MIN_MESSAGES) {
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
      tailFollowIntent={tailFollowIntent}
      preferInitialEstimatedBottomViewport
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
  newResponseIndicatorLabel,
  onScrollToLatest,
  onDraftCommit,
  onDraftAttachmentRemove,
  onRefreshSessionModelOptions,
  onRefreshAgentCommands,
  onSend,
  canSpawnDelegation = false,
  onSpawnDelegation,
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
  newResponseIndicatorLabel: string;
  onScrollToLatest: () => void;
  onDraftCommit: (sessionId: string, nextValue: string) => void;
  onDraftAttachmentRemove: (sessionId: string, attachmentId: string) => void;
  onRefreshSessionModelOptions: (sessionId: string) => void;
  onRefreshAgentCommands: (sessionId: string) => void;
  onSend: (sessionId: string, draftText?: string, expandedText?: string | null) => boolean;
  canSpawnDelegation?: boolean;
  onSpawnDelegation?: SpawnDelegationHandler;
  onSessionSettingsChange: (
    sessionId: string,
    field: SessionSettingsField,
    value: SessionSettingsValue,
  ) => void;
  onStopSession: (sessionId: string) => void;
  onPaste: (event: ReactClipboardEvent<HTMLTextAreaElement>) => void;
}) {
  const {
    composerInputRef,
    resetAndCancelScheduledComposerResize,
    resetComposerSizingState,
    cancelAndRestoreScheduledComposerTransition,
    resizeComposerInput,
    scheduleComposerResize,
  } = useComposerAutoResize(sessionId);
  const localDraftsRef = useRef<Record<string, string>>({});
  const committedDraftsRef = useRef<Record<string, string>>({});
  const onDraftCommitRef = useRef(onDraftCommit);
  const requestedSlashModelOptionsRef = useRef<string | null>(null);
  const requestedSlashAgentCommandsRef = useRef<string | null>(null);
  const slashOptionsRef = useRef<HTMLDivElement | null>(null);
  const composerDelegateButtonRef = useRef<HTMLButtonElement | null>(null);
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
  const [isAgentCommandResolving, setIsAgentCommandResolving] = useState(false);
  const isAgentCommandResolvingRef = useRef(false);
  const [isDelegationSpawning, setIsDelegationSpawning] = useState(false);
  const [agentCommandResolverError, setAgentCommandResolverError] =
    useState<AgentCommandResolverErrorState | null>(null);
  const isMountedRef = useRef(true);
  const activeSessionIdRef = useRef<string | null>(null);
  const lastComposerDraftSyncPropSessionIdRef = useRef<string | null>(null);
  const lastComposerDraftSyncSessionIdRef = useRef<string | null>(null);

  // `activeSessionId` is a best-effort identity for draft bookkeeping while
  // the store snapshot catches up. Callers that need capability/session fields
  // must still check `session`.
  const activeSessionId = session?.id ?? sessionId;
  useLayoutEffect(() => {
    activeSessionIdRef.current = activeSessionId;
  }, [activeSessionId]);
  useEffect(() => {
    // SessionComposer is memoized; explicitly drop resolver errors when the
    // active session identity changes even if the component instance is reused.
    setAgentCommandResolverError(null);
  }, [activeSessionId]);

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
  const canDelegateActiveSlashCommand =
    slashPalette.kind !== "none" && activeSlashItem?.kind === "agent-command";
  const composerInputDisabled =
    !session || isStopping || isAgentCommandResolving || isDelegationSpawning;
  const composerSendDisabled =
    !session ||
    isSending ||
    isStopping ||
    isUpdating ||
    isAgentCommandResolving ||
    (slashPalette.kind !== "none" && slashPalette.items.length === 0);
  const composerDelegateDisabled =
    !session ||
    !canSpawnDelegation ||
    !onSpawnDelegation ||
    isSending ||
    isStopping ||
    isUpdating ||
    isAgentCommandResolving ||
    isDelegationSpawning ||
    (slashPalette.kind !== "none" && !canDelegateActiveSlashCommand);

  useEffect(() => {
    isMountedRef.current = true;
    return () => {
      isMountedRef.current = false;
    };
  }, []);

  function beginAgentCommandResolution() {
    if (isAgentCommandResolvingRef.current) {
      return false;
    }
    isAgentCommandResolvingRef.current = true;
    setAgentCommandResolverError(null);
    setIsAgentCommandResolving(true);
    return true;
  }

  function finishAgentCommandResolution() {
    isAgentCommandResolvingRef.current = false;
    if (isMountedRef.current) {
      setIsAgentCommandResolving(false);
    }
  }

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

    const previousDraftSyncPropSessionId =
      lastComposerDraftSyncPropSessionIdRef.current;
    const isPropSessionSwitch = previousDraftSyncPropSessionId !== sessionId;
    lastComposerDraftSyncPropSessionIdRef.current = sessionId;
    const previousDraftSyncSessionId = lastComposerDraftSyncSessionIdRef.current;
    const isSessionSwitch = previousDraftSyncSessionId !== activeSessionId;
    lastComposerDraftSyncSessionIdRef.current = activeSessionId;
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
    if (
      didUpdateDomValue &&
      !isSessionSwitch &&
      !isPropSessionSwitch &&
      previousCommitted !== undefined
    ) {
      resizeComposerInput(true);
    }
  }, [activeSessionId, committedDraft]);

  useLayoutEffect(() => {
    resetComposerSizingState();
    resetAndCancelScheduledComposerResize();
    cancelAndRestoreScheduledComposerTransition();
    resizeComposerInput(true);

    return () => {
      resetAndCancelScheduledComposerResize();
      cancelAndRestoreScheduledComposerTransition();
    };
  }, [activeSessionId]);

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

  function updateLocalDraft(
    sessionId: string,
    nextValue: string,
    options: { animateHeight?: boolean } = {},
  ) {
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
      scheduleComposerResize(false, options.animateHeight ?? true);
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
    setAgentCommandResolverError(null);
    updateLocalDraft(activeSessionId, nextValue);
  }

  function handleComposerBlur() {
    if (!activeSessionId) {
      return;
    }

    commitDraft(activeSessionId, getComposerDraftValue());
  }

  async function applySlashPaletteItem(
    item: SlashPaletteItem,
    keepPaletteOpen = false,
  ) {
    if (
      !activeSessionId ||
      !session ||
      isSending ||
      isStopping ||
      isAgentCommandResolvingRef.current
    ) {
      return;
    }

    if (item.kind === "command") {
      resetPromptHistory(activeSessionId);
      const nextDraft = `${item.command} `;
      setAgentCommandResolverError(null);
      updateLocalDraft(activeSessionId, nextDraft);
      focusComposerInput(nextDraft.length);
      return;
    }

    if (item.kind === "agent-command") {
      if (isUpdating) {
        focusComposerInput(getComposerDraftValue().length);
        return;
      }

      const resolution = prepareAgentCommandSubmission(
        item,
        getComposerDraftValue(),
      );
      if (resolution.kind === "expand") {
        resetPromptHistory(activeSessionId);
        setAgentCommandResolverError(null);
        updateLocalDraft(activeSessionId, resolution.nextDraft);
        focusComposerInput(resolution.nextDraft.length);
        return;
      }

      const requestSessionId = activeSessionId;
      let resolved: ResolveAgentCommandResponse;
      if (!beginAgentCommandResolution()) {
        return;
      }
      try {
        resolved = await resolveAgentCommand(
          requestSessionId,
          resolution.commandName,
          {
            arguments: resolution.argumentsText,
            ...(resolution.noteText ? { note: resolution.noteText } : {}),
            intent: "send",
          },
        );
      } catch (error) {
        if (isMountedRef.current && activeSessionIdRef.current === requestSessionId) {
          setAgentCommandResolverError({
            message: formatAgentCommandResolverError(error),
            sessionId: requestSessionId,
          });
          focusComposerInput();
        }
        return;
      } finally {
        finishAgentCommandResolution();
      }

      if (!isMountedRef.current || activeSessionIdRef.current !== requestSessionId) {
        return;
      }

      const accepted = sendResolvedAgentCommandSubmission(
        onSend,
        requestSessionId,
        resolved,
      );
      if (!accepted) {
        focusComposerInput();
        return;
      }

      resetPromptHistory(requestSessionId);
      updateLocalDraft(requestSessionId, "", { animateHeight: false });
      commitDraft(requestSessionId, "");
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

  async function handleComposerSend() {
    if (
      !activeSessionId ||
      isSending ||
      isStopping ||
      isAgentCommandResolvingRef.current
    ) {
      return;
    }

    if (slashPalette.kind !== "none") {
      if (activeSlashItem) {
        if (activeSlashItem.kind === "choice" && isUpdating) {
          focusComposerInput(getComposerDraftValue().length);
          return;
        }
        await applySlashPaletteItem(activeSlashItem);
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
    updateLocalDraft(activeSessionId, "", { animateHeight: false });
    commitDraft(activeSessionId, "");
    focusComposerInput();
  }

  async function handleComposerDelegationSpawn() {
    if (composerDelegateDisabled || !activeSessionId || !onSpawnDelegation) {
      focusComposerInput();
      return;
    }

    const requestSessionId = activeSessionId;
    let prompt: string;
    let delegationOptions: SpawnDelegationOptions | undefined;
    if (slashPalette.kind !== "none") {
      if (activeSlashItem?.kind !== "agent-command") {
        focusComposerInput(getComposerDraftValue().length);
        return;
      }
      const resolution = prepareAgentCommandSubmission(
        activeSlashItem,
        getComposerDraftValue(),
      );
      if (resolution.kind === "expand") {
        resetPromptHistory(activeSessionId);
        updateLocalDraft(activeSessionId, resolution.nextDraft);
        focusComposerInput(resolution.nextDraft.length);
        return;
      }
      let resolved: ResolveAgentCommandResponse;
      if (!beginAgentCommandResolution()) {
        focusComposerInput();
        return;
      }
      try {
        resolved = await resolveAgentCommand(
          requestSessionId,
          resolution.commandName,
          {
            arguments: resolution.argumentsText,
            ...(resolution.noteText ? { note: resolution.noteText } : {}),
            intent: "delegate",
          },
        );
      } catch (error) {
        if (isMountedRef.current && activeSessionIdRef.current === requestSessionId) {
          setAgentCommandResolverError({
            message: formatAgentCommandResolverError(error),
            sessionId: requestSessionId,
          });
          focusComposerInput();
        }
        return;
      } finally {
        finishAgentCommandResolution();
      }
      if (!isMountedRef.current || activeSessionIdRef.current !== requestSessionId) {
        return;
      }
      prompt = (resolved.expandedPrompt ?? resolved.visiblePrompt).trim();
      delegationOptions = spawnDelegationOptionsFromResolvedCommand(resolved);
    } else {
      prompt = getComposerDraftValue().trim();
    }
    if (!prompt) {
      focusComposerInput();
      return;
    }

    setIsDelegationSpawning(true);
    let accepted = false;
    try {
      accepted = delegationOptions
        ? await onSpawnDelegation(requestSessionId, prompt, delegationOptions)
        : await onSpawnDelegation(requestSessionId, prompt);
    } catch {
      accepted = false;
    } finally {
      if (isMountedRef.current) {
        setIsDelegationSpawning(false);
      }
    }

    if (!isMountedRef.current) {
      return;
    }

    if (!accepted) {
      if (activeSessionIdRef.current !== requestSessionId) {
        return;
      }
      focusComposerInput();
      return;
    }

    if (activeSessionIdRef.current !== requestSessionId) {
      return;
    }

    resetPromptHistory(requestSessionId);
    updateLocalDraft(requestSessionId, "", { animateHeight: false });
    commitDraft(requestSessionId, "");
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
        setAgentCommandResolverError(null);
        updateLocalDraft(activeSessionId, "");
        commitDraft(activeSessionId, "");
        return;
      }

      if (
        shouldFocusDelegateWithSlashPaletteKey(
          event,
          canDelegateActiveSlashCommand,
          canSpawnDelegation,
          Boolean(onSpawnDelegation),
          composerDelegateDisabled,
        )
      ) {
        event.preventDefault();
        composerDelegateButtonRef.current?.focus();
        return;
      }

      if (
        shouldSubmitSlashPaletteKey(
          event,
          canDelegateActiveSlashCommand,
          canSpawnDelegation,
          Boolean(onSpawnDelegation),
          composerDelegateDisabled,
        )
      ) {
        event.preventDefault();
        void handleComposerSend();
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
          if (activeSlashItem.kind === "choice") {
            event.preventDefault();
            void applySlashPaletteItem(activeSlashItem, true);
          } else if (activeSlashItem.kind === "command") {
            event.preventDefault();
            void applySlashPaletteItem(activeSlashItem);
          } else {
            const parsedDraft = parseAgentCommandDraft(getComposerDraftValue());
            const matchesSelectedCommand =
              parsedDraft?.commandName.toLowerCase() ===
              activeSlashItem.name.toLowerCase();
            if (!matchesSelectedCommand) {
              event.preventDefault();
              resetPromptHistory(activeSessionId);
              const nextDraft = `/${activeSlashItem.name} `;
              setAgentCommandResolverError(null);
              updateLocalDraft(activeSessionId, nextDraft);
              focusComposerInput(nextDraft.length);
            }
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
      void handleComposerSend();
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
    slashPalette.kind === "none"
      ? null
      : (agentCommandResolverError?.sessionId === activeSessionId
          ? agentCommandResolverError.message
          : (slashPalette.errorMessage ?? null));
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
  const slashPaletteHintId = `composer-slash-hint-${paneId}`;
  const keyboardDelegationHint = "Tab moves focus to Delegate.";
  const slashPaletteHint =
    slashPalette.kind !== "none" &&
    canDelegateActiveSlashCommand &&
    !composerDelegateDisabled
      ? [slashPalette.hint, keyboardDelegationHint].filter(Boolean).join(" ")
      : slashPalette.kind !== "none"
        ? slashPalette.hint
        : null;
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
          {newResponseIndicatorLabel}
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
          {...CONVERSATION_COMPOSER_INPUT_DATA_ATTRIBUTES}
          aria-label={session ? `Message ${session.name}` : "Message session"}
          aria-describedby={slashPaletteHint ? slashPaletteHintId : undefined}
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
          {session && onSpawnDelegation && canSpawnDelegation ? (
            <button
              ref={composerDelegateButtonRef}
              className="ghost-button composer-delegate-button"
              type="button"
              onMouseDown={(event) => {
                event.preventDefault();
              }}
              onClick={() => void handleComposerDelegationSpawn()}
              disabled={composerDelegateDisabled}
              title="Spawn read-only delegation from current draft"
            >
              {isDelegationSpawning ? "Delegating..." : "Delegate"}
            </button>
          ) : null}
          <button
            className="send-button"
            type="button"
            onMouseDown={(event) => {
              event.preventDefault();
            }}
            onClick={() => void handleComposerSend()}
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
            <span id={slashPaletteHintId} className="composer-slash-hint">
              {slashPaletteHint}
            </span>
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
                      onClick={() => void applySlashPaletteItem(item)}
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
  previous.newResponseIndicatorLabel === next.newResponseIndicatorLabel &&
  previous.onScrollToLatest === next.onScrollToLatest &&
  previous.onDraftCommit === next.onDraftCommit &&
  previous.onDraftAttachmentRemove === next.onDraftAttachmentRemove &&
  previous.onRefreshSessionModelOptions === next.onRefreshSessionModelOptions &&
  previous.onRefreshAgentCommands === next.onRefreshAgentCommands &&
  previous.onSend === next.onSend &&
  previous.canSpawnDelegation === next.canSpawnDelegation &&
  previous.onSpawnDelegation === next.onSpawnDelegation &&
  previous.onSessionSettingsChange === next.onSessionSettingsChange &&
  previous.onStopSession === next.onStopSession &&
  previous.onPaste === next.onPaste
);
