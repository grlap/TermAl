import {
  memo,
  useCallback,
  useDeferredValue,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
} from "react";
import {
  findNewPendingCreatedConversationMarker,
  type PendingCreatedConversationMarker,
} from "./agent-session-panel-helpers";
import { SessionComposer } from "./AgentSessionPanel.composer";
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
  type RenderMessageCard,
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
  shouldShowAgentSessionWaitingIndicator,
} from "./AgentSessionPanel.waiting-indicator";
import { useSessionRecordSnapshot } from "../session-store";
import { useStableEvent } from "./use-stable-event";
import {
  includeUndeferredMessageTail,
  useInitialActiveTranscriptMessages,
} from "./useInitialActiveTranscriptMessages";
import { MessageMetaMarkerMenuProvider } from "../message-cards";
import { normalizeConversationMarkerColor } from "../conversation-marker-colors";
import type {
  PendingPrompt,
  ConversationMarker,
  CreateConversationMarkerOptions,
} from "../types";
import type {
  AgentSessionPanelFooterProps,
  AgentSessionPanelProps,
  ConversationMessageListProps,
  SessionBodyProps,
  SessionConversationPageProps,
} from "./AgentSessionPanel.types";

export { splitAgentCommandResolverTail } from "./session-agent-command-submission";

const EMPTY_PENDING_PROMPTS: readonly PendingPrompt[] = [];
const EMPTY_CONVERSATION_MARKERS: readonly ConversationMarker[] = [];
const NOOP_CREATE_CONVERSATION_MARKER = () => {};
const NOOP_DELETE_CONVERSATION_MARKER = () => {};

// The transcript virtualizer and overview rail intentionally share the same
// size threshold. The rail may still defer its first paint, but marker jumps
// need the virtualizer handle as soon as the transcript itself virtualizes.
const CONVERSATION_VIRTUALIZATION_MIN_MESSAGES =
  CONVERSATION_OVERVIEW_MIN_MESSAGES;
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
}: AgentSessionPanelProps): JSX.Element {
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
}: AgentSessionPanelFooterProps): JSX.Element {
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
}: SessionBodyProps): JSX.Element | null {
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
}: SessionConversationPageProps) {
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
  const effectiveShowWaitingIndicator = shouldShowAgentSessionWaitingIndicator({
    showWaitingIndicator,
    waitingIndicatorKind,
    sessionStatus: session.status,
    visibleMessages,
  });
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
}: ConversationMessageListProps) {
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
