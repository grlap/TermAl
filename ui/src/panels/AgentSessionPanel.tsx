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
  renderHighlightedText,
  type SearchHighlightTone,
} from "../search-highlight";
import { resolvePaneScrollCommand } from "../pane-keyboard";
import { findLastUserPrompt } from "../app-utils";
import {
  useComposerSessionSnapshot,
  useSessionRecordSnapshot,
} from "../session-store";
import { useStableEvent } from "./use-stable-event";
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
  DelegationWritePolicy,
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
const NOOP_DELETE_CONVERSATION_MARKER = () => {};

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

type AgentCommandSlashPaletteItem = Extract<
  SlashPaletteItem,
  { kind: "agent-command" }
>;

type AgentCommandSubmissionResolution =
  | { kind: "expand"; nextDraft: string }
  | {
      expandedPrompt: string | null;
      kind: "submit";
      visiblePrompt: string;
    };

type SpawnDelegationOptions = {
  writePolicy?: DelegationWritePolicy;
};

type SpawnDelegationHandler = (
  sessionId: string,
  prompt: string,
  options?: SpawnDelegationOptions,
) => Promise<boolean>;

function agentCommandDelegationOptions(
  item: AgentCommandSlashPaletteItem,
): SpawnDelegationOptions | undefined {
  const commandName = item.name.trim().toLowerCase();
  const commandSource = item.command.source.replace(/\\/g, "/").toLowerCase();
  if (
    commandName === "review-local" ||
    commandSource.endsWith("/.claude/commands/review-local.md")
  ) {
    return {
      writePolicy: { kind: "isolatedWorktree", ownedPaths: [] },
    };
  }
  return undefined;
}

function resolveAgentCommandSubmission(
  item: AgentCommandSlashPaletteItem,
  draft: string,
): AgentCommandSubmissionResolution {
  const agentCommand = item.command;
  const parsedDraft = parseAgentCommandDraft(draft);
  const matchesSelectedCommand =
    parsedDraft?.commandName.toLowerCase() === item.name.toLowerCase();
  if (item.hasArguments && !matchesSelectedCommand) {
    return { kind: "expand", nextDraft: `/${item.name} ` };
  }

  const visiblePrompt = (matchesSelectedCommand
    ? draft
    : `/${item.name}`).trim();
  if (normalizedAgentCommandKind(agentCommand) === "nativeSlash") {
    return { expandedPrompt: null, kind: "submit", visiblePrompt };
  }

  return {
    expandedPrompt: agentCommand.content.split("$ARGUMENTS").join(
      matchesSelectedCommand ? (parsedDraft?.argumentsText ?? "") : "",
    ),
    kind: "submit",
    visiblePrompt,
  };
}

function sendResolvedAgentCommandSubmission(
  onSend: (
    sessionId: string,
    draftText?: string,
    expandedText?: string | null,
  ) => boolean,
  sessionId: string,
  resolution: Extract<AgentCommandSubmissionResolution, { kind: "submit" }>,
) {
  return resolution.expandedPrompt == null
    ? onSend(sessionId, resolution.visiblePrompt)
    : onSend(sessionId, resolution.visiblePrompt, resolution.expandedPrompt);
}

// The transcript virtualizer and overview rail intentionally share the same
// size threshold. The rail may still defer its first paint, but marker jumps
// need the virtualizer handle as soon as the transcript itself virtualizes.
const CONVERSATION_VIRTUALIZATION_MIN_MESSAGES =
  CONVERSATION_OVERVIEW_MIN_MESSAGES;
const INITIAL_ACTIVE_TRANSCRIPT_TAIL_MIN_MESSAGES = 512;
const INITIAL_ACTIVE_TRANSCRIPT_TAIL_MESSAGE_COUNT = 20;
const INITIAL_ACTIVE_TRANSCRIPT_TOP_DEMAND_THRESHOLD_PX = 160;
const INITIAL_ACTIVE_TRANSCRIPT_WHEEL_DEMAND_THRESHOLD_PX = 8;
const INITIAL_ACTIVE_TRANSCRIPT_TOUCH_PULL_DEMAND_THRESHOLD_PX = 8;
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

function shouldUseInitialActiveTranscriptTailWindow({
  hasConversationMarkers,
  hasConversationSearch,
  isActive,
  messageCount,
}: {
  hasConversationMarkers: boolean;
  hasConversationSearch: boolean;
  isActive: boolean;
  messageCount: number;
}) {
  return (
    isActive &&
    messageCount > INITIAL_ACTIVE_TRANSCRIPT_TAIL_MIN_MESSAGES &&
    !hasConversationMarkers &&
    !hasConversationSearch
  );
}

function isTranscriptTopBoundaryDemandKey(event: KeyboardEvent) {
  const command = resolvePaneScrollCommand(
    {
      altKey: event.altKey,
      ctrlKey: event.ctrlKey,
      key: event.key,
      metaKey: event.metaKey,
      shiftKey: event.shiftKey,
    },
    event.target,
  );
  return command?.kind === "boundary" && command.direction === "up";
}

function shouldIgnoreTranscriptDemandKeyTarget(event: KeyboardEvent) {
  const target = event.target;
  if (isTranscriptTopBoundaryDemandKey(event)) {
    return false;
  }
  if (!(target instanceof Element)) {
    return false;
  }
  return Boolean(
    target.closest(
      "input, textarea, select, option, [contenteditable]",
    ),
  );
}

function isTranscriptDemandKeyEventInScope(
  event: KeyboardEvent,
  scrollNode: HTMLElement,
) {
  const path =
    typeof event.composedPath === "function" ? event.composedPath() : [];
  if (path.length > 0) {
    return path.includes(scrollNode);
  }
  return event.target instanceof Node && scrollNode.contains(event.target);
}

function useInitialActiveTranscriptMessages({
  hasConversationMarkers,
  hasConversationSearch,
  isActive,
  messages,
  scrollContainerRef,
  sessionId,
}: {
  hasConversationMarkers: boolean;
  hasConversationSearch: boolean;
  isActive: boolean;
  messages: Message[];
  scrollContainerRef: RefObject<HTMLElement | null>;
  sessionId: string;
}) {
  const [, forceHydratedRender] = useState(0);
  const hydrationRef = useRef({
    hydrated: false,
    sessionId,
  });
  if (hydrationRef.current.sessionId !== sessionId) {
    hydrationRef.current = {
      hydrated: false,
      sessionId,
    };
  }

  const isTailEligible = shouldUseInitialActiveTranscriptTailWindow({
    hasConversationMarkers,
    hasConversationSearch,
    isActive,
    messageCount: messages.length,
  });
  if (!isTailEligible && messages.length > INITIAL_ACTIVE_TRANSCRIPT_TAIL_MIN_MESSAGES) {
    hydrationRef.current.hydrated = true;
  }
  const isWindowed = isTailEligible && !hydrationRef.current.hydrated;
  const hasMessages = messages.length > 0;
  const requestFullTranscriptRender = useCallback(() => {
    if (
      hydrationRef.current.sessionId !== sessionId ||
      hydrationRef.current.hydrated
    ) {
      return;
    }

    hydrationRef.current.hydrated = true;
    forceHydratedRender((current) => current + 1);
  }, [sessionId]);

  useEffect(() => {
    if (
      hydrationRef.current.sessionId !== sessionId ||
      hydrationRef.current.hydrated
    ) {
      return undefined;
    }
    if (
      !isActive ||
      hasConversationMarkers ||
      hasConversationSearch ||
      !hasMessages
    ) {
      return undefined;
    }

    const node = scrollContainerRef.current;
    if (!node) {
      return undefined;
    }

    let lastTouchClientY: number | null = null;
    let hasDemandInteraction = false;
    let hasQueuedWheelDemandRender = false;
    let disposed = false;
    const requestFullTranscriptRenderAfterWheel = () => {
      if (hasQueuedWheelDemandRender) {
        return;
      }
      hasQueuedWheelDemandRender = true;
      queueMicrotask(() => {
        hasQueuedWheelDemandRender = false;
        if (!disposed) {
          requestFullTranscriptRender();
        }
      });
    };
    const hydrateIfNearTop = () => {
      if (
        hasDemandInteraction &&
        node.scrollTop <= INITIAL_ACTIVE_TRANSCRIPT_TOP_DEMAND_THRESHOLD_PX
      ) {
        requestFullTranscriptRender();
      }
    };
    const handleWheel = (event: WheelEvent) => {
      if (
        event.ctrlKey ||
        event.deltaY >= -INITIAL_ACTIVE_TRANSCRIPT_WHEEL_DEMAND_THRESHOLD_PX
      ) {
        return;
      }
      hasDemandInteraction = true;
      if (node.scrollTop <= INITIAL_ACTIVE_TRANSCRIPT_TOP_DEMAND_THRESHOLD_PX) {
        requestFullTranscriptRenderAfterWheel();
      }
    };
    const handleMouseDown = (event: MouseEvent) => {
      if (event.target === node) {
        hasDemandInteraction = true;
      }
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (!isTranscriptDemandKeyEventInScope(event, node)) {
        return;
      }
      if (shouldIgnoreTranscriptDemandKeyTarget(event)) {
        return;
      }
      if (
        event.key === "ArrowUp" ||
        event.key === "Home" ||
        event.key === "PageUp"
      ) {
        hasDemandInteraction = true;
        requestFullTranscriptRender();
      }
    };
    const handleTouchStart = (event: TouchEvent) => {
      hasDemandInteraction = true;
      lastTouchClientY = event.touches[0]?.clientY ?? null;
    };
    const handleTouchMove = (event: TouchEvent) => {
      const touch = event.touches[0] ?? event.changedTouches[0] ?? null;
      if (!touch) {
        return;
      }
      if (
        lastTouchClientY !== null &&
        touch.clientY - lastTouchClientY >
          INITIAL_ACTIVE_TRANSCRIPT_TOUCH_PULL_DEMAND_THRESHOLD_PX &&
        node.scrollTop <= INITIAL_ACTIVE_TRANSCRIPT_TOP_DEMAND_THRESHOLD_PX
      ) {
        requestFullTranscriptRender();
      }
      lastTouchClientY = touch.clientY;
    };
    const handleTouchEnd = () => {
      lastTouchClientY = null;
    };

    node.addEventListener("scroll", hydrateIfNearTop, { passive: true });
    node.addEventListener("wheel", handleWheel, { passive: true });
    node.addEventListener("mousedown", handleMouseDown, { passive: true });
    document.addEventListener("keydown", handleKeyDown, { capture: true });
    node.addEventListener("touchstart", handleTouchStart, { passive: true });
    node.addEventListener("touchmove", handleTouchMove, { passive: true });
    node.addEventListener("touchend", handleTouchEnd, { passive: true });
    node.addEventListener("touchcancel", handleTouchEnd, { passive: true });

    return () => {
      disposed = true;
      node.removeEventListener("scroll", hydrateIfNearTop);
      node.removeEventListener("wheel", handleWheel);
      node.removeEventListener("mousedown", handleMouseDown);
      document.removeEventListener("keydown", handleKeyDown, {
        capture: true,
      });
      node.removeEventListener("touchstart", handleTouchStart);
      node.removeEventListener("touchmove", handleTouchMove);
      node.removeEventListener("touchend", handleTouchEnd);
      node.removeEventListener("touchcancel", handleTouchEnd);
    };
  }, [
    hasConversationMarkers,
    hasConversationSearch,
    hasMessages,
    isActive,
    requestFullTranscriptRender,
    scrollContainerRef,
    sessionId,
  ]);

  const windowedMessages = useMemo(
    () =>
      isWindowed
        ? messages.slice(-INITIAL_ACTIVE_TRANSCRIPT_TAIL_MESSAGE_COUNT)
        : messages,
    [isWindowed, messages],
  );

  return {
    isWindowed,
    messages: windowedMessages,
    requestFullTranscriptRender,
  };
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
  liveTailPinned = true,
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
  ) => void;
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
  ) => void;
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
          liveTailPinned={liveTailPinned}
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
  ) => void;
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
  const conversationOverview = useConversationOverviewController({
    agent: session.agent,
    isActive,
    messageCount: overviewMessages.length,
    onFullTranscriptDemand: requestFullTranscriptRender,
    scrollContainerRef,
    sessionId: session.id,
    showWaitingIndicator,
    waitingIndicatorPrompt,
  });
  const markersByMessageId = useMemo(
    () => groupConversationMarkersByMessageId(visibleMarkers),
    [visibleMarkers],
  );
  const sortedMarkers = useMemo(
    () => sortConversationMarkersForNavigation(visibleMarkers, visibleMessages),
    [visibleMarkers, visibleMessages],
  );
  const [activeMarkerId, setActiveMarkerId] = useState<string | null>(null);
  const [markerPanelVisibilityOverride, setMarkerPanelVisibilityOverride] =
    useState<boolean | null>(null);
  const conversationPageRef = useRef<HTMLDivElement | null>(null);
  const markerPanelFocusRestoreFrameRef = useRef<number | null>(null);
  // null follows the auto-show heuristic; explicit booleans come from the
  // message-header context menu.
  const isMarkerPanelVisible =
    markerPanelVisibilityOverride ?? sortedMarkers.length > 0;
  const {
    contextMenuNode: markerContextMenuNode,
    openContextMenu: openMarkerContextMenu,
  } = useConversationMarkerContextMenu({
    isActive,
    isMarkerPanelVisible,
    markersByMessageId,
    onCreateConversationMarker,
    onDeleteConversationMarker,
    onSetMarkerPanelVisible: setMarkerPanelVisibilityOverride,
    scrollContainerRef,
    sessionId: session.id,
    visibleMessageIds,
  });
  const {
    handleConversationItemMount,
    jumpToMarker: jumpToConversationMarker,
  } = useConversationMarkerJump({
    onConversationSearchItemMount,
    scrollContainerRef,
    sessionId: session.id,
    virtualizerHandleRef: conversationOverview.virtualizerHandleRef,
  });

  useEffect(() => {
    if (
      activeMarkerId &&
      !visibleMarkers.some((marker) => marker.id === activeMarkerId)
    ) {
      setActiveMarkerId(null);
    }
  }, [activeMarkerId, visibleMarkers]);

  useEffect(() => {
    setMarkerPanelVisibilityOverride(null);
  }, [session.id]);

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
      jumpToConversationMarker(marker);
    },
    [jumpToConversationMarker],
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
      const activeMessageMarker = activeMarkerId
        ? markersByMessageId
            .get(message.id)
            ?.find((marker) => marker.id === activeMarkerId) ?? null
        : null;
      const markerShellStyle = activeMessageMarker
        ? ({
            "--conversation-active-marker-color":
              normalizeConversationMarkerColor(activeMessageMarker.color),
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
          className={`conversation-message-marker-shell can-open-marker-menu${activeMessageMarker ? " is-active-marker" : ""}`}
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
      markersByMessageId,
      openMarkerContextMenu,
      renderMessageCard,
    ],
  );

  if (visibleMessages.length === 0 && visiblePendingPrompts.length === 0 && !showWaitingIndicator) {
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
  const liveTail =
    liveTurnCard || pendingPromptCards.length > 0 ? (
      <div className={`conversation-live-tail${liveTailPinned ? " is-pinned" : ""}`}>
        {liveTurnCard}
        {/* DOM keeps the live turn before queued prompts for screen readers; pinned CSS places it nearest the composer. */}
        {/* Only the active mounted page exposes find anchors so cached hidden pages cannot hijack scroll targets. */}
        {pendingPromptCards}
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
      {liveTail}
    </>
  );
  const conversationPageClassName = `session-conversation-page${isActive ? " is-active" : ""}${conversationOverview.shouldRender ? " has-conversation-overview-scroll" : ""}`;

  return (
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
  const composerInputRef = useRef<HTMLTextAreaElement | null>(null);
  const composerResizeAnimationFrameRef = useRef<number | null>(null);
  const composerTransitionRestoreRef = useRef<{
    frameId: number;
    previousInlineTransition: string;
  } | null>(null);
  const composerResizeNeedsMetricRefreshRef = useRef(false);
  const composerResizeShouldAnimateHeightRef = useRef(true);
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
  const [isDelegationSpawning, setIsDelegationSpawning] = useState(false);
  const isMountedRef = useRef(true);
  const activeSessionIdRef = useRef<string | null>(null);

  // `activeSessionId` is a best-effort identity for draft bookkeeping while
  // the store snapshot catches up. Callers that need capability/session fields
  // must still check `session`.
  const activeSessionId = session?.id ?? sessionId;
  useLayoutEffect(() => {
    activeSessionIdRef.current = activeSessionId;
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
  const composerInputDisabled = !session || isStopping;
  const composerSendDisabled =
    !session ||
    isSending ||
    isStopping ||
    isUpdating ||
    (slashPalette.kind !== "none" && slashPalette.items.length === 0);
  const composerDelegateDisabled =
    !session ||
    !canSpawnDelegation ||
    !onSpawnDelegation ||
    isSending ||
    isStopping ||
    isUpdating ||
    isDelegationSpawning ||
    (slashPalette.kind !== "none" && !canDelegateActiveSlashCommand);

  useEffect(() => {
    isMountedRef.current = true;
    return () => {
      isMountedRef.current = false;
    };
  }, []);

  function cancelScheduledComposerResize() {
    composerResizeNeedsMetricRefreshRef.current = false;
    composerResizeShouldAnimateHeightRef.current = true;
    if (composerResizeAnimationFrameRef.current == null) {
      return;
    }

    window.cancelAnimationFrame(composerResizeAnimationFrameRef.current);
    composerResizeAnimationFrameRef.current = null;
  }

  function restoreComposerInputTransition(
    textarea: HTMLTextAreaElement,
    previousInlineTransition: string,
  ) {
    if (previousInlineTransition) {
      textarea.style.transition = previousInlineTransition;
    } else {
      textarea.style.removeProperty("transition");
    }
  }

  function cancelScheduledComposerTransitionRestore() {
    const pendingRestore = composerTransitionRestoreRef.current;
    if (!pendingRestore) {
      return null;
    }

    window.cancelAnimationFrame(pendingRestore.frameId);
    composerTransitionRestoreRef.current = null;
    return pendingRestore.previousInlineTransition;
  }

  function cancelAndRestoreScheduledComposerTransition() {
    const previousInlineTransition = cancelScheduledComposerTransitionRestore();
    const textarea = composerInputRef.current;
    if (
      previousInlineTransition !== null &&
      textarea &&
      textarea.style.transition === "none"
    ) {
      restoreComposerInputTransition(textarea, previousInlineTransition);
    }
  }

  function scheduleComposerTransitionRestore(
    textarea: HTMLTextAreaElement,
    previousInlineTransition: string,
  ) {
    cancelScheduledComposerTransitionRestore();
    const frameId = window.requestAnimationFrame(() => {
      const pendingRestore = composerTransitionRestoreRef.current;
      if (!pendingRestore || pendingRestore.frameId !== frameId) {
        return;
      }

      composerTransitionRestoreRef.current = null;
      restoreComposerInputTransition(
        textarea,
        pendingRestore.previousInlineTransition,
      );
    });
    composerTransitionRestoreRef.current = {
      frameId,
      previousInlineTransition,
    };
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

  function resizeComposerInput(forceRefreshMetrics = false, animateHeight = true) {
    const textarea = composerInputRef.current;
    if (!textarea) {
      return;
    }

    const pendingPreviousInlineTransition =
      cancelScheduledComposerTransitionRestore();
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
    const previousInlineTransition =
      pendingPreviousInlineTransition !== null &&
      textarea.style.transition === "none"
        ? pendingPreviousInlineTransition
        : textarea.style.transition;
    const previousMeasuredHeight =
      composerLastAppliedHeightRef.current ??
      (parseFloat(textarea.style.height) ||
        textarea.getBoundingClientRect().height ||
        null);
    if (shouldAllowShrink) {
      textarea.style.transition = "none";
      textarea.style.height = `${Math.max(sizingState.minHeight, 1)}px`;
      composerLastAppliedHeightRef.current = null;
    }

    const contentHeight = textarea.scrollHeight + sizingState.borderHeight;
    const nextHeight = Math.min(Math.max(contentHeight, sizingState.minHeight), maxHeight);
    const nextOverflowY: "auto" | "hidden" =
      contentHeight > maxHeight + 1 ? "auto" : "hidden";

    if (shouldAllowShrink) {
      const hasPreviousMeasuredHeight = previousMeasuredHeight != null;
      const heightChanged =
        !hasPreviousMeasuredHeight ||
        Math.abs(previousMeasuredHeight - nextHeight) > 0.5;
      if (hasPreviousMeasuredHeight) {
        textarea.style.height = `${previousMeasuredHeight}px`;
        void textarea.offsetHeight;
      }
      if (animateHeight) {
        restoreComposerInputTransition(textarea, previousInlineTransition);
      } else {
        scheduleComposerTransitionRestore(textarea, previousInlineTransition);
      }
      if (!heightChanged) {
        composerLastAppliedHeightRef.current = nextHeight;
      }
    }

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

  function scheduleComposerResize(forceRefreshMetrics = false, animateHeight = true) {
    if (!activeSessionId) {
      return;
    }

    composerResizeNeedsMetricRefreshRef.current =
      composerResizeNeedsMetricRefreshRef.current || forceRefreshMetrics;
    composerResizeShouldAnimateHeightRef.current =
      composerResizeShouldAnimateHeightRef.current && animateHeight;
    if (composerResizeAnimationFrameRef.current != null) {
      return;
    }

    composerResizeAnimationFrameRef.current = window.requestAnimationFrame(() => {
      composerResizeAnimationFrameRef.current = null;
      const shouldRefreshMetrics = composerResizeNeedsMetricRefreshRef.current;
      const shouldAnimateHeight = composerResizeShouldAnimateHeightRef.current;
      composerResizeNeedsMetricRefreshRef.current = false;
      composerResizeShouldAnimateHeightRef.current = true;
      resizeComposerInput(shouldRefreshMetrics, shouldAnimateHeight);
    });
  }

  useLayoutEffect(() => {
    composerSizingStateRef.current = null;
    composerResizeNeedsMetricRefreshRef.current = false;
    composerResizeShouldAnimateHeightRef.current = true;
    composerLastMeasuredDraftLengthRef.current = 0;
    composerLastAppliedHeightRef.current = null;
    composerLastAppliedOverflowYRef.current = null;
    cancelScheduledComposerResize();
    cancelAndRestoreScheduledComposerTransition();
    resizeComposerInput(true);

    return () => {
      cancelScheduledComposerResize();
      cancelAndRestoreScheduledComposerTransition();
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
      composerResizeShouldAnimateHeightRef.current = true;
      composerLastMeasuredDraftLengthRef.current = 0;
      composerLastAppliedHeightRef.current = null;
      composerLastAppliedOverflowYRef.current = null;
      cancelScheduledComposerResize();
      cancelAndRestoreScheduledComposerTransition();
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

      const resolution = resolveAgentCommandSubmission(
        item,
        getComposerDraftValue(),
      );
      if (resolution.kind === "expand") {
        resetPromptHistory(activeSessionId);
        updateLocalDraft(activeSessionId, resolution.nextDraft);
        focusComposerInput(resolution.nextDraft.length);
        return;
      }

      const accepted = sendResolvedAgentCommandSubmission(
        onSend,
        activeSessionId,
        resolution,
      );
      if (!accepted) {
        focusComposerInput();
        return;
      }

      resetPromptHistory(activeSessionId);
      updateLocalDraft(activeSessionId, "", { animateHeight: false });
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
    updateLocalDraft(activeSessionId, "", { animateHeight: false });
    commitDraft(activeSessionId, "");
    focusComposerInput();
  }

  async function handleComposerDelegationSpawn() {
    if (composerDelegateDisabled || !activeSessionId || !onSpawnDelegation) {
      focusComposerInput();
      return;
    }

    let prompt: string;
    let delegationOptions: SpawnDelegationOptions | undefined;
    if (slashPalette.kind !== "none") {
      if (activeSlashItem?.kind !== "agent-command") {
        focusComposerInput(getComposerDraftValue().length);
        return;
      }
      delegationOptions = agentCommandDelegationOptions(activeSlashItem);
      const resolution = resolveAgentCommandSubmission(
        activeSlashItem,
        getComposerDraftValue(),
      );
      if (resolution.kind === "expand") {
        resetPromptHistory(activeSessionId);
        updateLocalDraft(activeSessionId, resolution.nextDraft);
        focusComposerInput(resolution.nextDraft.length);
        return;
      }
      prompt = (resolution.expandedPrompt ?? resolution.visiblePrompt).trim();
    } else {
      prompt = getComposerDraftValue().trim();
    }
    if (!prompt) {
      focusComposerInput();
      return;
    }

    const requestSessionId = activeSessionId;
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
          if (activeSlashItem.kind === "choice") {
            event.preventDefault();
            applySlashPaletteItem(activeSlashItem, true);
          } else if (activeSlashItem.kind === "command") {
            event.preventDefault();
            applySlashPaletteItem(activeSlashItem);
          } else {
            const parsedDraft = parseAgentCommandDraft(getComposerDraftValue());
            const matchesSelectedCommand =
              parsedDraft?.commandName.toLowerCase() ===
              activeSlashItem.name.toLowerCase();
            if (!matchesSelectedCommand) {
              event.preventDefault();
              resetPromptHistory(activeSessionId);
              const nextDraft = `/${activeSlashItem.name} `;
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
          data-conversation-composer-input="true"
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
          {session && onSpawnDelegation && canSpawnDelegation ? (
            <button
              className="ghost-button composer-delegate-button"
              type="button"
              onMouseDown={(event) => {
                event.preventDefault();
              }}
              onClick={() => void handleComposerDelegationSpawn()}
              disabled={composerDelegateDisabled}
              aria-busy={isDelegationSpawning}
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
