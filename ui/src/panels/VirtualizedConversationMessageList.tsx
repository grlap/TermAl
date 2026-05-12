// Virtualized conversation rendering for an agent session.
//
// The model here is intentionally simple:
// - mounted page bands are real DOM and own the live scroll experience
// - only unseen pages above/below the mounted band are virtual space
// - page measurements may refine unseen spacers, but anchor preservation keeps
//   the currently visible DOM band stable while that virtual space catches up

import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type RefObject,
} from "react";
import { flushSync } from "react-dom";
import { isExpandedPromptOpen } from "../ExpandedPromptPanel";
import {
  canNestedScrollableConsumeWheel,
  normalizeWheelDelta,
} from "../app-utils";
import {
  DEFERRED_RENDER_RESUME_EVENT,
  DEFERRED_RENDER_SUSPENDED_ATTRIBUTE,
} from "../deferred-render";
import { DeferredHeavyContentActivationProvider } from "../message-cards";
import {
  MESSAGE_STACK_BOTTOM_FOLLOW_SCROLL_MS,
  MESSAGE_STACK_SCROLL_WRITE_EVENT,
  type MessageStackScrollWriteDetail,
} from "../message-stack-scroll-sync";
import { MessageSlot } from "./session-message-leaves";
import {
  DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
  VIRTUALIZED_MESSAGE_GAP_PX,
  clampVirtualizedViewportScrollTop,
  estimateConversationMessageHeight,
  findVirtualizedMessageRange,
  getScrollContainerBottomGap,
  isScrollContainerNearBottom,
} from "./conversation-virtualization";
import type {
  ApprovalDecision,
  JsonValue,
  McpElicitationAction,
  Message,
} from "../types";

export type UserInputSubmitHandler = (
  sessionId: string,
  messageId: string,
  answers: Record<string, string[]>,
) => void;

export type BoundUserInputSubmitHandler = (
  messageId: string,
  answers: Record<string, string[]>,
) => void;

export type McpElicitationSubmitHandler = (
  sessionId: string,
  messageId: string,
  action: McpElicitationAction,
  content?: JsonValue,
) => void;

export type BoundMcpElicitationSubmitHandler = (
  messageId: string,
  action: McpElicitationAction,
  content?: JsonValue,
) => void;

export type CodexAppRequestSubmitHandler = (
  sessionId: string,
  messageId: string,
  result: JsonValue,
) => void;

export type BoundCodexAppRequestSubmitHandler = (
  messageId: string,
  result: JsonValue,
) => void;

export type RenderMessageCard = (
  message: Message,
  preferImmediateHeavyRender: boolean,
  onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void,
  onUserInputSubmit: BoundUserInputSubmitHandler,
  onMcpElicitationSubmit: BoundMcpElicitationSubmitHandler,
  onCodexAppRequestSubmit: BoundCodexAppRequestSubmitHandler,
) => JSX.Element | null;

export type VirtualizedConversationJumpOptions = {
  // `center` is the default. `flush` is for callers that need the target DOM
  // mounted before the next browser paint.
  align?: "start" | "center" | "end";
  flush?: boolean;
};

export type VirtualizedConversationLayoutMessage = {
  messageId: string;
  messageIndex: number;
  pageIndex: number;
  type: Message["type"];
  author: Message["author"];
  // Estimated document-space top; per-message geometry is not measured.
  estimatedTopPx: number;
  estimatedHeightPx: number;
  // Page-level measured height copied onto each message in the page; null until
  // that page has reported its real DOM height.
  measuredPageHeightPx: number | null;
};

export type VirtualizedConversationLayoutSnapshot = {
  // A point-in-time snapshot for overview/marker navigation. Identity changes
  // as layout and scroll state changes; it is not a subscription object.
  sessionId: string;
  messageCount: number;
  estimatedTotalHeightPx: number;
  viewportTopPx: number;
  viewportHeightPx: number;
  viewportWidthPx: number;
  isActive: boolean;
  visiblePageRange: VirtualizedRange;
  mountedPageRange: VirtualizedRange;
  messages: VirtualizedConversationLayoutMessage[];
};

export type VirtualizedConversationViewportSnapshot = Omit<
  VirtualizedConversationLayoutSnapshot,
  "messages"
> & {
  // Identifies the loaded message window behind a viewport-only snapshot. This
  // prevents overview projections from reusing tail-window translations after
  // the virtualizer moves to a different same-size window.
  windowStartMessageId?: string | null;
  windowEndMessageId?: string | null;
};

export type VirtualizedConversationMessageListHandle = {
  // Stable for the lifetime of the mount. Methods read the latest layout
  // state internally, so consumers can keep the handle as an effect dependency
  // without retriggering on every virtualized layout update.
  getLayoutSnapshot: () => VirtualizedConversationLayoutSnapshot;
  getViewportSnapshot: () => VirtualizedConversationViewportSnapshot;
  // Returns false when the list is inactive, the scroll node is missing, or the
  // target cannot be resolved from the currently loaded transcript.
  jumpToMessageId: (
    messageId: string,
    options?: VirtualizedConversationJumpOptions,
  ) => boolean;
  jumpToMessageIndex: (
    messageIndex: number,
    options?: VirtualizedConversationJumpOptions,
  ) => boolean;
};

export type VirtualizedConversationMessageListHandleRef = {
  // Set while the list is mounted; cleared on unmount.
  current: VirtualizedConversationMessageListHandle | null;
};

const VIRTUALIZED_MESSAGES_PER_PAGE = 8;
const ACTIVE_MOUNTED_RESERVE_ABOVE_VIEWPORTS = 3;
const ACTIVE_MOUNTED_RESERVE_BELOW_VIEWPORTS = 3;
const BOUNDARY_SEEK_MOUNTED_RESERVE_ABOVE_VIEWPORTS = 1;
const BOUNDARY_SEEK_MOUNTED_RESERVE_BELOW_VIEWPORTS = 0;
const ACTIVE_MOUNTED_EXTRA_PAGES_BELOW = 2;
const IDLE_MOUNTED_COMPACTION_PAGE_HYSTERESIS = 2;
const ACTIVE_VIEWPORT_STARTUP_RESYNC_FRAMES = 12;
const BOTTOM_BOUNDARY_REVEAL_SETTLE_FRAMES = 12;
const BOTTOM_BOUNDARY_REVEAL_DELAY_MS = 220;
const POST_ACTIVATION_ESTIMATED_BOTTOM_MIN_PAGES = 20;
const ACTIVE_SCROLL_MOUNTED_RANGE_COLLAPSE_EXTRA_PAGES = 12;
const ACTIVE_SCROLL_MOUNTED_RANGE_COLLAPSE_MULTIPLIER = 2;
export const VIRTUALIZED_USER_SCROLL_ADJUSTMENT_COOLDOWN_MS = 200;
// Separate, much shorter cooldown for the deferred-heavy-content (Markdown,
// tool blocks) activation gate. Heavy content paint should resume almost
// immediately after the user stops scrolling, while the broader scroll-state
// machine keeps its 200ms quiet window for range / mount adjustments.
const DEFERRED_HEAVY_ACTIVATION_COOLDOWN_MS = 10;
const MIN_PAGE_COVERAGE_HEIGHT_PX = 64;

const EMPTY_MATCHED_ITEM_KEYS = new Set<string>();

export type VirtualizedRange = { startIndex: number; endIndex: number };
export type UserScrollKind = "incremental" | "page_jump" | "seek" | null;
type MessageLocation = {
  message: Message;
  messageIndex: number;
  pageIndex: number;
  pageLocalIndex: number;
};
type VisibleMessageAnchor = {
  messageId: string;
  viewportOffsetPx: number;
};
type PendingVisibleMessageAnchor = VisibleMessageAnchor & {
  remainingAttempts: number;
};
type MessagePage = {
  key: string;
  pageIndex: number;
  startIndex: number;
  endIndex: number;
  hasTrailingGap: boolean;
  messages: Message[];
};

type EstimatedPageHeightEntry = {
  cacheKey: string;
  height: number;
};
export type MessageWindowSnapshot = {
  ids: string[];
  sessionId: string;
};

function rangesEqual(first: VirtualizedRange, second: VirtualizedRange) {
  return first.startIndex === second.startIndex && first.endIndex === second.endIndex;
}

function buildMessagePages(messages: Message[]) {
  const pages: MessagePage[] = [];
  for (
    let startIndex = 0;
    startIndex < messages.length;
    startIndex += VIRTUALIZED_MESSAGES_PER_PAGE
  ) {
    const endIndex = Math.min(startIndex + VIRTUALIZED_MESSAGES_PER_PAGE, messages.length);
    const pageMessages = messages.slice(startIndex, endIndex);
    const firstMessageId = pageMessages[0]?.id ?? `page-${startIndex}`;
    const lastMessageId = pageMessages[pageMessages.length - 1]?.id ?? firstMessageId;
    pages.push({
      key: `${startIndex}:${endIndex}:${firstMessageId}:${lastMessageId}`,
      pageIndex: pages.length,
      startIndex,
      endIndex,
      hasTrailingGap: endIndex < messages.length,
      messages: pageMessages,
    });
  }
  return pages;
}

function buildPageLayout(pageHeights: number[]) {
  const tops = new Array<number>(pageHeights.length);
  let totalHeight = 0;
  for (let index = 0; index < pageHeights.length; index += 1) {
    tops[index] = totalHeight;
    totalHeight += pageHeights[index] ?? 0;
  }
  return { tops, totalHeight };
}

function classifyScrollKind(
  scrollDelta: number,
  clientHeight: number,
): Exclude<UserScrollKind, null> {
  return Math.abs(scrollDelta) >=
    Math.max(clientHeight * 1.5, DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT)
    ? "seek"
    : "incremental";
}

export function resolveBottomReentryScrollKind(): UserScrollKind {
  return null;
}

export function resolveNativeScrollKind(
  cachedScrollKind: UserScrollKind,
  scrollDelta: number,
  clientHeight: number,
): Exclude<UserScrollKind, null> {
  return cachedScrollKind ?? classifyScrollKind(scrollDelta, clientHeight);
}

function resolveRenderedPageCoverageHeight(pageNodes: HTMLElement[]) {
  let minHeight = Number.POSITIVE_INFINITY;
  for (const pageNode of pageNodes) {
    const height = pageNode.getBoundingClientRect().height;
    if (Number.isFinite(height) && height > 0) {
      minHeight = Math.min(minHeight, height);
    }
  }

  return Number.isFinite(minHeight)
    ? Math.max(minHeight, MIN_PAGE_COVERAGE_HEIGHT_PX)
    : null;
}

function resolvePageCoverageHeight(
  pageHeights: number[],
  pageIndex: number,
  renderedCoverageHeight: number | null,
) {
  const measuredOrEstimatedHeight = pageHeights[pageIndex];
  const fallbackHeight =
    Number.isFinite(measuredOrEstimatedHeight) && measuredOrEstimatedHeight > 0
      ? measuredOrEstimatedHeight
      : DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT;
  const coverageHeight =
    renderedCoverageHeight !== null
      ? Math.min(fallbackHeight, renderedCoverageHeight)
      : fallbackHeight;
  return Math.max(coverageHeight, MIN_PAGE_COVERAGE_HEIGHT_PX);
}

function rangeContainsRange(container: VirtualizedRange, target: VirtualizedRange) {
  return (
    container.startIndex <= target.startIndex &&
    container.endIndex >= target.endIndex
  );
}

function getRangePageCount(range: VirtualizedRange) {
  return Math.max(range.endIndex - range.startIndex, 0);
}

function shouldCollapseIncrementalMountedRange(
  currentRange: VirtualizedRange,
  targetRange: VirtualizedRange,
) {
  const targetPageCount = Math.max(getRangePageCount(targetRange), 1);
  const combinedPageCount =
    Math.max(currentRange.endIndex, targetRange.endIndex) -
    Math.min(currentRange.startIndex, targetRange.startIndex);
  const maxMountedPageCount = Math.max(
    targetPageCount * ACTIVE_SCROLL_MOUNTED_RANGE_COLLAPSE_MULTIPLIER,
    targetPageCount + ACTIVE_SCROLL_MOUNTED_RANGE_COLLAPSE_EXTRA_PAGES,
  );
  return combinedPageCount > maxMountedPageCount;
}

function getMountedMessageSlots(virtualizedListRoot: ParentNode | null) {
  if (!virtualizedListRoot) {
    return [];
  }
  return Array.from(
    virtualizedListRoot.querySelectorAll<HTMLElement>(".virtualized-message-slot[data-message-id]"),
  );
}

function findMountedMessageSlotById(
  virtualizedListRoot: ParentNode | null,
  messageId: string,
) {
  return (
    getMountedMessageSlots(virtualizedListRoot).find(
      (slot) => slot.dataset.messageId === messageId,
    ) ?? null
  );
}

function captureFirstVisibleMountedMessageAnchor(
  virtualizedListRoot: ParentNode | null,
  scrollContainerNode: HTMLElement,
): VisibleMessageAnchor | null {
  const containerRect = scrollContainerNode.getBoundingClientRect();
  const visibleSlot = getMountedMessageSlots(virtualizedListRoot).find((slot) => {
    const rect = slot.getBoundingClientRect();
    return rect.bottom > containerRect.top && rect.top < containerRect.bottom;
  });
  if (!visibleSlot?.dataset.messageId) {
    return null;
  }
  return {
    messageId: visibleSlot.dataset.messageId,
    viewportOffsetPx: visibleSlot.getBoundingClientRect().top - containerRect.top,
  };
}

function estimatePageHeight(
  page: MessagePage,
  estimateMessageHeight: (message: Message) => number,
) {
  if (page.messages.length === 0) {
    return 0;
  }

  let total = 0;
  for (let index = 0; index < page.messages.length; index += 1) {
    total += estimateMessageHeight(page.messages[index]!);
    if (index < page.messages.length - 1) {
      total += VIRTUALIZED_MESSAGE_GAP_PX;
    }
  }
  if (page.hasTrailingGap) {
    total += VIRTUALIZED_MESSAGE_GAP_PX;
  }
  return total;
}

function buildPageEstimateCacheKey(
  page: MessagePage,
  availableWidthPx: number,
) {
  const widthBucket =
    Number.isFinite(availableWidthPx) && availableWidthPx > 0
      ? Math.round(availableWidthPx)
      : 0;
  const expandedPromptKey = page.messages
    .flatMap((message) =>
      message.type === "text" &&
      message.author === "you" &&
      message.expandedText &&
      isExpandedPromptOpen(message.id)
        ? [message.id]
        : [],
    )
    .join(",");
  return `${page.key}:${widthBucket}:${expandedPromptKey}`;
}

function estimateMessageOffsetWithinPage(
  page: MessagePage,
  messageLocalIndex: number,
  estimateMessageHeight: (message: Message) => number,
) {
  let offset = 0;
  for (let index = 0; index < messageLocalIndex; index += 1) {
    offset += estimateMessageHeight(page.messages[index]!);
    offset += VIRTUALIZED_MESSAGE_GAP_PX;
  }
  return offset;
}

export function resolvePrependedMessageCount(
  previous: MessageWindowSnapshot,
  currentMessages: readonly Message[],
  sessionId: string,
) {
  if (
    previous.sessionId !== sessionId ||
    previous.ids.length === 0 ||
    previous.ids.length >= currentMessages.length
  ) {
    return null;
  }

  const firstPreviousId = previous.ids[0];
  const maxStartIndex = currentMessages.length - previous.ids.length;
  for (let startIndex = 0; startIndex <= maxStartIndex; startIndex += 1) {
    if (currentMessages[startIndex]?.id !== firstPreviousId) {
      continue;
    }
    const matchesPreviousWindow = previous.ids.every(
      (messageId, index) => currentMessages[startIndex + index]?.id === messageId,
    );
    if (matchesPreviousWindow) {
      return startIndex > 0 ? startIndex : null;
    }
  }

  return null;
}

export function VirtualizedConversationMessageList({
  isActive,
  renderMessageCard,
  sessionId,
  messages,
  scrollContainerRef,
  tailFollowIntent = false,
  conversationSearchQuery = "",
  conversationSearchMatchedItemKeys = EMPTY_MATCHED_ITEM_KEYS,
  conversationSearchActiveItemKey = null,
  onConversationSearchItemMount = () => {},
  preferInitialEstimatedBottomViewport = false,
  virtualizerHandleRef,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
}: {
  isActive: boolean;
  renderMessageCard: RenderMessageCard;
  sessionId: string;
  messages: Message[];
  scrollContainerRef: RefObject<HTMLElement | null>;
  tailFollowIntent?: boolean;
  conversationSearchQuery?: string;
  conversationSearchMatchedItemKeys?: ReadonlySet<string>;
  conversationSearchActiveItemKey?: string | null;
  onConversationSearchItemMount?: (itemKey: string, node: HTMLElement | null) => void;
  preferInitialEstimatedBottomViewport?: boolean;
  virtualizerHandleRef?: VirtualizedConversationMessageListHandleRef;
  onApprovalDecision: (sessionId: string, messageId: string, decision: ApprovalDecision) => void;
  onUserInputSubmit: UserInputSubmitHandler;
  onMcpElicitationSubmit: McpElicitationSubmitHandler;
  onCodexAppRequestSubmit: CodexAppRequestSubmitHandler;
}) {
  const hasConversationSearch = conversationSearchQuery.trim().length > 0;
  const activeConversationSearchMessageId =
    conversationSearchActiveItemKey?.startsWith("message:")
      ? conversationSearchActiveItemKey.slice("message:".length)
      : null;
  const activeConversationSearchPinKey = useMemo(() => {
    const trimmedQuery = conversationSearchQuery.trim();
    if (trimmedQuery.length === 0 || activeConversationSearchMessageId === null) {
      return null;
    }

    return JSON.stringify([sessionId, activeConversationSearchMessageId, trimmedQuery]);
  }, [activeConversationSearchMessageId, conversationSearchQuery, sessionId]);
  const activeConversationSearchPositionKey =
    activeConversationSearchMessageId === null
      ? null
      : JSON.stringify([sessionId, activeConversationSearchMessageId]);

  const pageHeightsRef = useRef<Record<string, number>>({});
  const estimatedPageHeightsRef = useRef<Record<string, EstimatedPageHeightEntry>>(
    {},
  );
  const shouldKeepBottomAfterLayoutRef = useRef(false);
  const isDetachedFromBottomRef = useRef(false);
  const skipNextMountedPrependRestoreRef = useRef(false);
  const lastPinnedConversationSearchPositionKeyRef = useRef<string | null>(null);
  const activeConversationSearchPositionKeyRef = useRef<string | null>(null);
  const lastUserScrollInputTimeRef = useRef(Number.NEGATIVE_INFINITY);
  const lastUserScrollKindRef = useRef<UserScrollKind>(null);
  const lastTouchClientYRef = useRef<number | null>(null);
  const pendingAggressiveIdleCompactionRef = useRef(false);
  const lastNativeScrollTopRef = useRef(0);
  const pendingProgrammaticScrollTopRef = useRef<number | null>(null);
  const pendingMountedPrependRestoreRef = useRef<{
    anchor: VisibleMessageAnchor | null;
    scrollHeight: number;
    scrollTop: number;
  } | null>(null);
  const pendingDeferredLayoutAnchorRef = useRef<{
    messageId: string;
    viewportOffsetPx: number;
  } | null>(null);
  const pendingDeferredLayoutTimerRef = useRef<number | null>(null);
  const pendingIdleCompactionTimerRef = useRef<number | null>(null);
  const pendingBottomBoundaryRevealFrameRef = useRef<number | null>(null);
  const pendingBottomBoundaryRevealNodeRef = useRef<HTMLElement | null>(null);
  const pendingDeferredRenderResumeTimerRef = useRef<number | null>(null);
  const pendingDeferredRenderSuspendedNodeRef = useRef<HTMLElement | null>(null);
  const pendingProgrammaticViewportSyncRef = useRef(false);
  const previousMessageWindowRef = useRef<MessageWindowSnapshot>({
    ids: messages.map((message) => message.id),
    sessionId,
  });
  const latestVisibleMessageAnchorRef = useRef<VisibleMessageAnchor | null>(null);
  const pendingPrependedMessageAnchorRef =
    useRef<PendingVisibleMessageAnchor | null>(null);
  const pendingPrependedTopBoundaryRef = useRef(false);
  const pendingPrependedBottomGapRef = useRef<number | null>(null);
  // Deadline until which a programmatic `bottom_follow` smooth-scroll can
  // claim native scroll ticks. User gestures reset it to negative infinity.
  const pendingProgrammaticBottomFollowUntilRef = useRef(
    Number.NEGATIVE_INFINITY,
  );
  const pendingBottomBoundarySeekRef = useRef(false);
  const renderedListRef = useRef<HTMLDivElement | null>(null);
  const hasUserScrollInteractionRef = useRef(false);
  activeConversationSearchPositionKeyRef.current = activeConversationSearchPositionKey;

  const [viewport, setViewport] = useState({
    height: DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
    scrollTop: 0,
    width: 0,
  });
  const [layoutVersion, setLayoutVersion] = useState(0);
  const [scrollIdleVersion, setScrollIdleVersion] = useState(0);
  const [bottomBoundarySeekVersion, setBottomBoundarySeekVersion] = useState(0);
  const [
    releasedConversationSearchPositionKey,
    setReleasedConversationSearchPositionKey,
  ] = useState<
    string | null
  >(null);
  const [hasUserScrollInteraction, setHasUserScrollInteractionState] = useState(false);
  const [isMeasuringPostActivation, setIsMeasuringPostActivation] = useState(
    () => isActive && messages.length > 0,
  );
  const [
    isBottomBoundaryRevealPending,
    setIsBottomBoundaryRevealPending,
  ] = useState(false);
  const [bottomBoundaryRevealToken, setBottomBoundaryRevealToken] = useState(0);
  const previousIsActiveRef = useRef(isActive);

  const setHasUserScrollInteraction = useCallback((nextValue: boolean) => {
    hasUserScrollInteractionRef.current = nextValue;
    setHasUserScrollInteractionState((current) =>
      current === nextValue ? current : nextValue,
    );
  }, []);

  useLayoutEffect(() => {
    if (!isActive || !tailFollowIntent) {
      return;
    }

    shouldKeepBottomAfterLayoutRef.current = true;
    isDetachedFromBottomRef.current = false;
  }, [isActive, tailFollowIntent]);
  // Search navigation keeps the active result's page band mounted until the
  // reader takes control. `activeConversationSearchPinKey` includes the query
  // text so a live, unreleased selection can re-arm its mounted page band as
  // matching changes. `activeConversationSearchPositionKey` is only
  // session+message; it gates user-scroll release and scroll repositioning so
  // refining the same match does not yank the viewport or remount an offscreen
  // search band after the reader has scrolled away. Both keys are JSON encoded
  // to avoid separator assumptions across tuple fields.
  const releaseConversationSearchPinForUserScroll = useCallback(() => {
    const positionKey = activeConversationSearchPositionKeyRef.current;
    if (positionKey === null) {
      return;
    }

    setReleasedConversationSearchPositionKey((current) =>
      current === positionKey ? current : positionKey,
    );
  }, []);

  useEffect(() => {
    setReleasedConversationSearchPositionKey((current) =>
      current !== null && current !== activeConversationSearchPositionKey ? null : current,
    );
  }, [activeConversationSearchPositionKey]);

  const syncViewportFromScrollNode = useCallback((node: HTMLElement) => {
    const nextState = {
      height: node.clientHeight > 0 ? node.clientHeight : DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
      scrollTop: node.scrollTop,
      width: node.clientWidth > 0 ? node.clientWidth : 0,
    };
    setViewport((current) =>
      current.height === nextState.height &&
      current.scrollTop === nextState.scrollTop &&
      current.width === nextState.width
        ? current
        : nextState,
    );
  }, []);

  const writeScrollTopAndSyncViewport = useCallback(
    (node: HTMLElement, nextScrollTop: number) => {
      const targetScrollTop = Number.isFinite(nextScrollTop) ? Math.max(nextScrollTop, 0) : 0;
      if (Math.abs(node.scrollTop - targetScrollTop) >= 1) {
        pendingProgrammaticScrollTopRef.current = targetScrollTop;
        node.scrollTop = targetScrollTop;
      }
      syncViewportFromScrollNode(node);
    },
    [syncViewportFromScrollNode],
  );

  const bumpLayoutVersion = useCallback(() => {
    setLayoutVersion((current) => current + 1);
  }, []);

  const clearPendingDeferredLayoutTimer = useCallback(() => {
    if (pendingDeferredLayoutTimerRef.current !== null) {
      window.clearTimeout(pendingDeferredLayoutTimerRef.current);
      pendingDeferredLayoutTimerRef.current = null;
    }
  }, []);
  const clearPendingIdleCompactionTimer = useCallback(() => {
    if (pendingIdleCompactionTimerRef.current !== null) {
      window.clearTimeout(pendingIdleCompactionTimerRef.current);
      pendingIdleCompactionTimerRef.current = null;
    }
  }, []);
  const clearPendingDeferredRenderResumeTimer = useCallback(() => {
    if (pendingDeferredRenderResumeTimerRef.current !== null) {
      window.clearTimeout(pendingDeferredRenderResumeTimerRef.current);
      pendingDeferredRenderResumeTimerRef.current = null;
    }
  }, []);
  const resumeDeferredRenderActivation = useCallback(() => {
    clearPendingDeferredRenderResumeTimer();
    const node = pendingDeferredRenderSuspendedNodeRef.current;
    if (!node) {
      return;
    }
    pendingDeferredRenderSuspendedNodeRef.current = null;
    node.removeAttribute(DEFERRED_RENDER_SUSPENDED_ATTRIBUTE);
    node.dispatchEvent(new Event(DEFERRED_RENDER_RESUME_EVENT));
  }, [clearPendingDeferredRenderResumeTimer]);
  const suspendDeferredRenderActivation = useCallback(
    (node: HTMLElement) => {
      if (scrollContainerRef.current !== node) {
        return;
      }
      clearPendingDeferredRenderResumeTimer();
      pendingDeferredRenderSuspendedNodeRef.current = node;
      node.setAttribute(DEFERRED_RENDER_SUSPENDED_ATTRIBUTE, "true");
      pendingDeferredRenderResumeTimerRef.current = window.setTimeout(() => {
        resumeDeferredRenderActivation();
      }, DEFERRED_HEAVY_ACTIVATION_COOLDOWN_MS);
    },
    [
      clearPendingDeferredRenderResumeTimer,
      resumeDeferredRenderActivation,
      scrollContainerRef,
    ],
  );
  const clearPendingBottomBoundaryRevealFrame = useCallback(() => {
    if (pendingBottomBoundaryRevealFrameRef.current !== null) {
      window.cancelAnimationFrame(pendingBottomBoundaryRevealFrameRef.current);
      pendingBottomBoundaryRevealFrameRef.current = null;
    }
    if (pendingBottomBoundaryRevealNodeRef.current) {
      delete pendingBottomBoundaryRevealNodeRef.current.dataset
        .virtualizedBottomBoundaryReveal;
      pendingBottomBoundaryRevealNodeRef.current = null;
    }
  }, []);
  const finishPostActivationMeasuring = useCallback(() => {
    renderedListRef.current?.classList.remove("is-measuring-post-activation");
    if (pendingBottomBoundaryRevealNodeRef.current) {
      delete pendingBottomBoundaryRevealNodeRef.current.dataset
        .virtualizedBottomBoundaryReveal;
      pendingBottomBoundaryRevealNodeRef.current = null;
    }
    setIsMeasuringPostActivation(false);
    setIsBottomBoundaryRevealPending(false);
  }, []);
  const scheduleIdleMountedRangeCompaction = useCallback(
    (delayMs: number) => {
      clearPendingIdleCompactionTimer();
      pendingIdleCompactionTimerRef.current = window.setTimeout(() => {
        pendingIdleCompactionTimerRef.current = null;
        lastUserScrollKindRef.current = null;
        setScrollIdleVersion((current) => current + 1);
      }, Math.max(Math.ceil(delayMs), 0));
    },
    [clearPendingIdleCompactionTimer],
  );

  const scheduleDeferredLayoutVersion = useCallback(
    (delayMs: number) => {
      clearPendingDeferredLayoutTimer();
      pendingDeferredLayoutTimerRef.current = window.setTimeout(() => {
        pendingDeferredLayoutTimerRef.current = null;
        bumpLayoutVersion();
      }, Math.max(Math.ceil(delayMs), 0));
    },
    [bumpLayoutVersion, clearPendingDeferredLayoutTimer],
  );

  const scheduleProgrammaticViewportSync = useCallback(
    (node: HTMLElement) => {
      if (pendingProgrammaticViewportSyncRef.current) {
        return;
      }

      pendingProgrammaticViewportSyncRef.current = true;
      queueMicrotask(() => {
        pendingProgrammaticViewportSyncRef.current = false;
        if (scrollContainerRef.current !== node) {
          return;
        }
        syncViewportFromScrollNode(node);
      });
    },
    [scrollContainerRef, syncViewportFromScrollNode],
  );
  const cancelPostActivationBottomRestore = useCallback(() => {
    clearPendingBottomBoundaryRevealFrame();
    shouldKeepBottomAfterLayoutRef.current = false;
    finishPostActivationMeasuring();
  }, [
    clearPendingBottomBoundaryRevealFrame,
    finishPostActivationMeasuring,
  ]);
  const scheduleBottomBoundaryReveal = useCallback(
    (node: HTMLElement) => {
      clearPendingBottomBoundaryRevealFrame();
      pendingBottomBoundaryRevealNodeRef.current = node;
      node.dataset.virtualizedBottomBoundaryReveal = "true";
      renderedListRef.current?.classList.add("is-measuring-post-activation");
      setIsMeasuringPostActivation(true);
      setIsBottomBoundaryRevealPending(true);
      setBottomBoundaryRevealToken((current) => current + 1);

      const step = (attempts: number) => {
        pendingBottomBoundaryRevealFrameRef.current = window.requestAnimationFrame(() => {
          pendingBottomBoundaryRevealFrameRef.current = null;
          if (scrollContainerRef.current !== node) {
            return;
          }

          shouldKeepBottomAfterLayoutRef.current = true;
          const maxScrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
          writeScrollTopAndSyncViewport(node, maxScrollTop);

          if (attempts + 1 < BOTTOM_BOUNDARY_REVEAL_SETTLE_FRAMES) {
            step(attempts + 1);
          }
        });
      };

      step(0);
    },
    [
      clearPendingBottomBoundaryRevealFrame,
      scrollContainerRef,
      writeScrollTopAndSyncViewport,
    ],
  );

  useEffect(() => {
    const previousIsActive = previousIsActiveRef.current;
    previousIsActiveRef.current = isActive;
    if (!previousIsActive && isActive && messages.length > 0) {
      setIsMeasuringPostActivation(true);
    }
  }, [isActive, messages.length]);
  useEffect(
    () => () => {
      clearPendingBottomBoundaryRevealFrame();
    },
    [clearPendingBottomBoundaryRevealFrame],
  );
  useEffect(() => {
    if (!isBottomBoundaryRevealPending) {
      return undefined;
    }

    const timeoutId = window.setTimeout(() => {
      const node = scrollContainerRef.current;
      if (node) {
        const maxScrollTop = Math.max(node.scrollHeight - node.clientHeight, 0);
        writeScrollTopAndSyncViewport(node, maxScrollTop);
      }
      finishPostActivationMeasuring();
    }, BOTTOM_BOUNDARY_REVEAL_DELAY_MS);

    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [
    bottomBoundaryRevealToken,
    finishPostActivationMeasuring,
    isBottomBoundaryRevealPending,
    scrollContainerRef,
    writeScrollTopAndSyncViewport,
  ]);
  const isActivatingWithMessages =
    !previousIsActiveRef.current && isActive && messages.length > 0;

  const activeViewport = isActive ? scrollContainerRef.current : null;
  const viewportHeight =
    activeViewport?.clientHeight && activeViewport.clientHeight > 0
      ? activeViewport.clientHeight
      : viewport.height;
  const viewportWidth =
    activeViewport?.clientWidth && activeViewport.clientWidth > 0
      ? activeViewport.clientWidth
      : viewport.width;

  const estimateMessageHeight = useCallback(
    (message: Message) =>
      estimateConversationMessageHeight(message, {
        availableWidthPx: viewportWidth,
        expandedPromptOpen:
          message.type === "text" &&
          message.author === "you" &&
          Boolean(message.expandedText) &&
          isExpandedPromptOpen(message.id),
      }),
    [viewportWidth],
  );

  const pages = useMemo(() => buildMessagePages(messages), [messages]);
  const pageKeys = useMemo(() => new Set(pages.map((page) => page.key)), [pages]);
  const messageLocationById = useMemo(() => {
    const locations = new Map<string, MessageLocation>();
    pages.forEach((page) => {
      page.messages.forEach((message, pageLocalIndex) => {
        locations.set(message.id, {
          message,
          messageIndex: page.startIndex + pageLocalIndex,
          pageIndex: page.pageIndex,
          pageLocalIndex,
        });
      });
    });
    return locations;
  }, [pages]);

  const pageHeights = useMemo(
    () =>
      pages.map((page) => {
        const measuredHeight = pageHeightsRef.current[page.key];
        if (measuredHeight !== undefined) {
          return measuredHeight;
        }

        const cacheKey = buildPageEstimateCacheKey(page, viewportWidth);
        const cachedEstimate = estimatedPageHeightsRef.current[page.key];
        if (cachedEstimate?.cacheKey === cacheKey) {
          return cachedEstimate.height;
        }

        const estimatedHeight = estimatePageHeight(page, estimateMessageHeight);
        estimatedPageHeightsRef.current[page.key] = {
          cacheKey,
          height: estimatedHeight,
        };
        return estimatedHeight;
      }),
    [estimateMessageHeight, layoutVersion, pages, viewportWidth],
  );
  const pageLayout = useMemo(() => buildPageLayout(pageHeights), [pageHeights]);
  const estimatedBottomScrollTop = clampVirtualizedViewportScrollTop({
    scrollTop: pageLayout.totalHeight - viewportHeight,
    viewportHeight,
    totalHeight: pageLayout.totalHeight,
  });
  const shouldUseEstimatedBottomViewport =
    preferInitialEstimatedBottomViewport &&
    isActive &&
    isMeasuringPostActivation &&
    pages.length >= POST_ACTIVATION_ESTIMATED_BOTTOM_MIN_PAGES &&
    !hasConversationSearch &&
    !hasUserScrollInteraction &&
    pendingProgrammaticScrollTopRef.current === null &&
    !isDetachedFromBottomRef.current;
  const rawViewportScrollTop = shouldUseEstimatedBottomViewport
    ? estimatedBottomScrollTop
    : activeViewport
      ? activeViewport.scrollTop
      : viewport.scrollTop;
  const viewportScrollTop = clampVirtualizedViewportScrollTop({
    scrollTop: rawViewportScrollTop,
    viewportHeight,
    totalHeight: pageLayout.totalHeight,
  });
  const resolveEstimatedBottomScrollTop = useCallback(
    () => estimatedBottomScrollTop,
    [estimatedBottomScrollTop],
  );
  const writeEstimatedScrollTopAndSyncViewport = useCallback(
    (node: HTMLElement, nextScrollTop: number) => {
      const targetScrollTop = Number.isFinite(nextScrollTop)
        ? Math.max(nextScrollTop, 0)
        : 0;
      pendingProgrammaticScrollTopRef.current = targetScrollTop;
      node.scrollTop = targetScrollTop;
      setViewport((current) => {
        const nextState = {
          height: viewportHeight > 0 ? viewportHeight : current.height,
          scrollTop: targetScrollTop,
          width: viewportWidth > 0 ? viewportWidth : current.width,
        };
        return current.height === nextState.height &&
          current.scrollTop === nextState.scrollTop &&
          current.width === nextState.width
          ? current
          : nextState;
      });
    },
    [viewportHeight, viewportWidth],
  );

  const visiblePageRange = useMemo(() => {
    if (pages.length === 0) {
      return { startIndex: 0, endIndex: 0 };
    }
    return findVirtualizedMessageRange(
      pageLayout.tops,
      pageHeights,
      viewportScrollTop,
      viewportHeight,
      0,
      0,
    );
  }, [pageHeights, pageLayout.tops, pages.length, viewportHeight, viewportScrollTop]);

  const activeConversationSearchLocation =
    activeConversationSearchMessageId !== null
      ? messageLocationById.get(activeConversationSearchMessageId)
      : undefined;
  const activeConversationSearchScrollTop = useMemo(() => {
    if (!hasConversationSearch || !activeConversationSearchLocation) {
      return null;
    }

    const page = pages[activeConversationSearchLocation.pageIndex];
    if (!page) {
      return null;
    }

    const messageTop =
      (pageLayout.tops[activeConversationSearchLocation.pageIndex] ?? 0) +
      estimateMessageOffsetWithinPage(
        page,
        activeConversationSearchLocation.pageLocalIndex,
        estimateMessageHeight,
      );
    const messageHeight = estimateMessageHeight(activeConversationSearchLocation.message);
    return Math.max(messageTop - Math.max((viewportHeight - messageHeight) / 2, 0), 0);
  }, [
    activeConversationSearchLocation,
    estimateMessageHeight,
    hasConversationSearch,
    pageLayout.tops,
    pages,
    viewportHeight,
  ]);

  const activeMountedBufferAbovePx = shouldUseEstimatedBottomViewport
    ? 0
    : Math.max(
        viewportHeight * ACTIVE_MOUNTED_RESERVE_ABOVE_VIEWPORTS,
        DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
      );
  const activeMountedBufferBelowPx = shouldUseEstimatedBottomViewport
    ? 0
    : Math.max(
        viewportHeight * ACTIVE_MOUNTED_RESERVE_BELOW_VIEWPORTS,
        DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
      );
  const activeMountedExtraPagesBelow = shouldUseEstimatedBottomViewport
    ? 0
    : ACTIVE_MOUNTED_EXTRA_PAGES_BELOW;
  // Active scroll keeps a working set around the viewport instead of waiting
  // until the reader is close to a band edge. The reserve is intentionally
  // wider below the viewport because height overestimates show up there as
  // visible blank space before the next page is mounted and measured.
  const workingMountedPageRange = useMemo(() => {
    if (pages.length === 0) {
      return { startIndex: 0, endIndex: 0 };
    }

    const baseRange = findVirtualizedMessageRange(
      pageLayout.tops,
      pageHeights,
      viewportScrollTop,
      viewportHeight,
      activeMountedBufferAbovePx,
      activeMountedBufferBelowPx,
    );

    // Keep extra whole pages mounted below the computed working range. The
    // bottom edge is where minor page-height drift shows up most clearly as a
    // deterministic "messages disappear at this exact offset" gap. Holding
    // extra pages below turns that hard boundary into hysteresis instead of a
    // visible blank slab.
    return {
      startIndex: baseRange.startIndex,
      endIndex: Math.min(baseRange.endIndex + activeMountedExtraPagesBelow, pages.length),
    };
  }, [
    activeMountedExtraPagesBelow,
    activeMountedBufferAbovePx,
    activeMountedBufferBelowPx,
    pageHeights,
    pageLayout.tops,
    pages.length,
    viewportHeight,
    viewportScrollTop,
  ]);
  const [mountedPageRange, setMountedPageRange] = useState<VirtualizedRange>(
    workingMountedPageRange,
  );
  const mountedPageRangeRef = useRef(mountedPageRange);
  mountedPageRangeRef.current = mountedPageRange;
  const applyMountedPageRange = useCallback(
    (nextRange: VirtualizedRange, options: { flush?: boolean } = {}) => {
      mountedPageRangeRef.current = nextRange;
      if (options.flush) {
        flushSync(() => {
          setMountedPageRange(nextRange);
        });
        return;
      }
      setMountedPageRange(nextRange);
    },
    [],
  );
  const captureMountedPrependRestore = useCallback(
    (node: HTMLElement) => ({
      anchor: captureFirstVisibleMountedMessageAnchor(
        renderedListRef.current,
        node,
      ),
      scrollHeight: node.scrollHeight,
      scrollTop: node.scrollTop,
    }),
    [],
  );
  const captureLatestVisibleMessageAnchor = useCallback((node: HTMLElement) => {
    const anchor = captureFirstVisibleMountedMessageAnchor(
      renderedListRef.current,
      node,
    );
    if (anchor) {
      latestVisibleMessageAnchorRef.current = anchor;
    }
    return anchor;
  }, []);
  const pageLayoutTopsRef = useRef(pageLayout.tops);
  pageLayoutTopsRef.current = pageLayout.tops;
  const pageHeightsRefForScroll = useRef(pageHeights);
  pageHeightsRefForScroll.current = pageHeights;
  const pagesLengthRef = useRef(pages.length);
  pagesLengthRef.current = pages.length;
  const activeMountedBufferAbovePxRef = useRef(activeMountedBufferAbovePx);
  activeMountedBufferAbovePxRef.current = activeMountedBufferAbovePx;
  const activeMountedBufferBelowPxRef = useRef(activeMountedBufferBelowPx);
  activeMountedBufferBelowPxRef.current = activeMountedBufferBelowPx;

  // See the search pin state-machine comment above
  // `releaseConversationSearchPinForUserScroll`.
  const searchPinnedMountedPageRange = useMemo(() => {
    if (
      !isActive ||
      !hasConversationSearch ||
      releasedConversationSearchPositionKey === activeConversationSearchPositionKey ||
      activeConversationSearchScrollTop === null ||
      pages.length === 0
    ) {
      return null;
    }

    const baseRange = findVirtualizedMessageRange(
      pageLayout.tops,
      pageHeights,
      activeConversationSearchScrollTop,
      viewportHeight,
      activeMountedBufferAbovePx,
      activeMountedBufferBelowPx,
    );
    return {
      startIndex: baseRange.startIndex,
      endIndex: Math.min(baseRange.endIndex + ACTIVE_MOUNTED_EXTRA_PAGES_BELOW, pages.length),
    };
  }, [
    activeConversationSearchPinKey,
    activeConversationSearchPositionKey,
    activeConversationSearchScrollTop,
    activeMountedBufferAbovePx,
    activeMountedBufferBelowPx,
    hasConversationSearch,
    isActive,
    pageHeights,
    pageLayout.tops,
    pages.length,
    releasedConversationSearchPositionKey,
    viewportHeight,
  ]);
  const renderedMountedPageRange = useMemo(() => {
    return searchPinnedMountedPageRange ?? mountedPageRange;
  }, [mountedPageRange, searchPinnedMountedPageRange]);

  const mountedPages = useMemo(
    () => pages.slice(renderedMountedPageRange.startIndex, renderedMountedPageRange.endIndex),
    [pages, renderedMountedPageRange.endIndex, renderedMountedPageRange.startIndex],
  );
  const topSpacerHeight =
    renderedMountedPageRange.startIndex > 0
      ? (pageLayout.tops[renderedMountedPageRange.startIndex] ?? 0)
      : 0;
  const mountedPageEndOffset =
    renderedMountedPageRange.endIndex <= renderedMountedPageRange.startIndex
      ? topSpacerHeight
      : (pageLayout.tops[renderedMountedPageRange.endIndex - 1] ?? topSpacerHeight) +
        (pageHeights[renderedMountedPageRange.endIndex - 1] ?? 0);
  const bottomSpacerHeight = Math.max(pageLayout.totalHeight - mountedPageEndOffset, 0);
  const preferImmediateHeavyRender =
    !isMeasuringPostActivation &&
    !isActivatingWithMessages &&
    !hasUserScrollInteraction;
  // Always allow deferred-heavy activation in the React context. The actual
  // suspension during fast scroll is driven by the DOM-attribute mechanism
  // (`data-deferred-render-suspended` + DEFERRED_RENDER_RESUME_EVENT) inside
  // `DeferredHeavyContent`, which resumes after
  // DEFERRED_HEAVY_ACTIVATION_COOLDOWN_MS (10ms). Gating here on a ref-derived
  // boolean would not actually re-render at the 10ms mark — the next re-render
  // is typically the 200ms idle compaction pass, which is far too late for
  // heavy markdown to come back after the user stops scrolling.
  const allowDeferredHeavyActivation = true;

  const buildWorkingMountedRangeForScrollTop = useCallback(
    (scrollTop: number, clientHeight: number) => {
      const baseRange = findVirtualizedMessageRange(
        pageLayoutTopsRef.current,
        pageHeightsRefForScroll.current,
        scrollTop,
        clientHeight > 0 ? clientHeight : DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
        activeMountedBufferAbovePxRef.current,
        activeMountedBufferBelowPxRef.current,
      );
      return {
        startIndex: baseRange.startIndex,
        endIndex: Math.min(
          baseRange.endIndex + ACTIVE_MOUNTED_EXTRA_PAGES_BELOW,
          pagesLengthRef.current,
        ),
      };
    },
    [],
  );
  const buildBottomMountedRange = useCallback(
    (clientHeight: number) => {
      const pageCount = pagesLengthRef.current;
      if (pageCount === 0) {
        return { startIndex: 0, endIndex: 0 };
      }

      const viewportHeight =
        clientHeight > 0 ? clientHeight : DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT;
      const lastPageIndex = pageCount - 1;
      const totalHeight =
        (pageLayoutTopsRef.current[lastPageIndex] ?? 0) +
        (pageHeightsRefForScroll.current[lastPageIndex] ?? 0);
      return findVirtualizedMessageRange(
        pageLayoutTopsRef.current,
        pageHeightsRefForScroll.current,
        Math.max(totalHeight - viewportHeight, 0),
        viewportHeight,
        viewportHeight * BOUNDARY_SEEK_MOUNTED_RESERVE_ABOVE_VIEWPORTS,
        viewportHeight * BOUNDARY_SEEK_MOUNTED_RESERVE_BELOW_VIEWPORTS,
      );
    },
    [],
  );
  const mountBottomBoundary = useCallback(
    (node: HTMLElement) => {
      if (
        scrollContainerRef.current !== node ||
        !pendingBottomBoundarySeekRef.current
      ) {
        return;
      }

      applyMountedPageRange(buildBottomMountedRange(node.clientHeight));
      setBottomBoundarySeekVersion((version) => version + 1);
    },
    [applyMountedPageRange, buildBottomMountedRange, scrollContainerRef],
  );
  const expandRangeToRenderedPageEdges = useCallback(
    (
      node: HTMLElement,
      nextRange: VirtualizedRange,
      scrollDelta: number,
    ): VirtualizedRange => {
      const pageNodes = renderedListRef.current
        ? Array.from(
            renderedListRef.current.querySelectorAll<HTMLElement>(
              ".virtualized-message-page[data-page-key]",
            ),
          )
        : [];
      if (pageNodes.length === 0) {
        return nextRange;
      }

      const renderedCoverageHeight = resolveRenderedPageCoverageHeight(pageNodes);
      const containerRect = node.getBoundingClientRect();
      let nextStartIndex = nextRange.startIndex;
      let nextEndIndex = nextRange.endIndex;

      if (scrollDelta < 0) {
        const firstPageRect = pageNodes[0]!.getBoundingClientRect();
        const renderedMountedTop =
          node.scrollTop + (firstPageRect.top - containerRect.top);
        const desiredMountedTop = Math.max(
          node.scrollTop - activeMountedBufferAbovePxRef.current,
          0,
        );
        let missingAbovePx = renderedMountedTop - desiredMountedTop;
        while (missingAbovePx > 0 && nextStartIndex > 0) {
          nextStartIndex -= 1;
          missingAbovePx -= resolvePageCoverageHeight(
            pageHeightsRefForScroll.current,
            nextStartIndex,
            renderedCoverageHeight,
          );
        }
      } else if (scrollDelta > 0) {
        const lastPageRect = pageNodes[pageNodes.length - 1]!.getBoundingClientRect();
        const renderedMountedBottom =
          node.scrollTop + (lastPageRect.bottom - containerRect.top);
        const desiredMountedBottom =
          node.scrollTop + node.clientHeight + activeMountedBufferBelowPxRef.current;
        let missingBelowPx = desiredMountedBottom - renderedMountedBottom;
        while (missingBelowPx > 0 && nextEndIndex < pagesLengthRef.current) {
          missingBelowPx -= resolvePageCoverageHeight(
            pageHeightsRefForScroll.current,
            nextEndIndex,
            renderedCoverageHeight,
          );
          nextEndIndex += 1;
        }
      }

      return {
        startIndex: nextStartIndex,
        endIndex: nextEndIndex,
      };
    },
    [],
  );

  const reconcileMountedRangeForNativeScroll = useCallback(
    (
      node: HTMLElement,
      scrollDelta: number,
      scrollKind: UserScrollKind,
      options: { allowSeekFlush?: boolean; flush?: boolean } = {},
    ) => {
      const nextWorkingRange = buildWorkingMountedRangeForScrollTop(
        node.scrollTop,
        node.clientHeight,
      );

      const currentRange = mountedPageRangeRef.current;
      const resolvedScrollKind =
        scrollKind === "incremental" &&
        shouldCollapseIncrementalMountedRange(currentRange, nextWorkingRange)
          ? "seek"
          : scrollKind;
      let nextRange: VirtualizedRange = currentRange;

      if (resolvedScrollKind === "seek") {
        nextRange = nextWorkingRange;
      } else if (resolvedScrollKind === "page_jump") {
        nextRange = expandRangeToRenderedPageEdges(node, {
          startIndex: Math.min(currentRange.startIndex, nextWorkingRange.startIndex),
          endIndex: Math.max(currentRange.endIndex, nextWorkingRange.endIndex),
        }, scrollDelta);
      } else if (scrollDelta !== 0) {
        nextRange = expandRangeToRenderedPageEdges(node, {
          startIndex: Math.min(currentRange.startIndex, nextWorkingRange.startIndex),
          endIndex: Math.max(currentRange.endIndex, nextWorkingRange.endIndex),
        }, scrollDelta);
      } else {
        nextRange = {
          startIndex: Math.min(currentRange.startIndex, nextWorkingRange.startIndex),
          endIndex: Math.max(currentRange.endIndex, nextWorkingRange.endIndex),
        };
      }

      if (rangesEqual(nextRange, currentRange)) {
        return;
      }

      if (resolvedScrollKind === "seek" || resolvedScrollKind === "page_jump") {
        lastUserScrollKindRef.current = resolvedScrollKind;
        pendingAggressiveIdleCompactionRef.current = true;
      }

      if (
        resolvedScrollKind === "incremental" &&
        nextRange.startIndex < currentRange.startIndex &&
        !isScrollContainerNearBottom(node)
      ) {
        if (skipNextMountedPrependRestoreRef.current) {
          skipNextMountedPrependRestoreRef.current = false;
        } else {
          pendingMountedPrependRestoreRef.current =
            captureMountedPrependRestore(node);
        }
      } else if (resolvedScrollKind === "seek" || resolvedScrollKind === "page_jump") {
        pendingMountedPrependRestoreRef.current = null;
        skipNextMountedPrependRestoreRef.current = false;
      }

      applyMountedPageRange(nextRange, {
        flush:
          options.flush ||
          (options.allowSeekFlush !== false &&
            (resolvedScrollKind === "seek" ||
              resolvedScrollKind === "page_jump")),
      });
    },
    [
      applyMountedPageRange,
      buildWorkingMountedRangeForScrollTop,
      captureMountedPrependRestore,
      expandRangeToRenderedPageEdges,
    ],
  );
  const prewarmMountedRangeForUpwardWheel = useCallback(
    (node: HTMLElement, wheelDeltaY: number) => {
      if (wheelDeltaY >= 0) {
        return;
      }

      const projectedScrollTop = Math.max(node.scrollTop + wheelDeltaY, 0);
      if (Math.abs(projectedScrollTop - node.scrollTop) < 1) {
        return;
      }

      const currentRange = mountedPageRangeRef.current;
      const projectedWorkingRange = buildWorkingMountedRangeForScrollTop(
        projectedScrollTop,
        node.clientHeight,
      );
      const nextRange = expandRangeToRenderedPageEdges(
        node,
        {
          startIndex: Math.min(currentRange.startIndex, projectedWorkingRange.startIndex),
          endIndex: Math.max(currentRange.endIndex, projectedWorkingRange.endIndex),
        },
        wheelDeltaY,
      );
      if (rangesEqual(nextRange, currentRange)) {
        return;
      }

      if (nextRange.startIndex < currentRange.startIndex && !isScrollContainerNearBottom(node)) {
        if (skipNextMountedPrependRestoreRef.current) {
          skipNextMountedPrependRestoreRef.current = false;
        } else {
          pendingMountedPrependRestoreRef.current =
            captureMountedPrependRestore(node);
        }
      }
      applyMountedPageRange(nextRange, { flush: true });
    },
    [
      applyMountedPageRange,
      buildWorkingMountedRangeForScrollTop,
      captureMountedPrependRestore,
      expandRangeToRenderedPageEdges,
    ],
  );

  useLayoutEffect(() => {
    const previousWindow = previousMessageWindowRef.current;
    const nextWindow = {
      ids: messages.map((message) => message.id),
      sessionId,
    };
    previousMessageWindowRef.current = nextWindow;

    if (!isActive) {
      return;
    }

    const prependedCount = resolvePrependedMessageCount(
      previousWindow,
      messages,
      sessionId,
    );
    if (prependedCount === null) {
      return;
    }

    const node = scrollContainerRef.current;
    if (!node) {
      return;
    }

    const prependedPageIndex = Math.floor(prependedCount / VIRTUALIZED_MESSAGES_PER_PAGE);
    const prependedPage = pages[prependedPageIndex];
    if (!prependedPage) {
      return;
    }

    const prependedLocalIndex = prependedCount - prependedPage.startIndex;
    const prependedHeightPx =
      (pageLayout.tops[prependedPageIndex] ?? 0) +
      estimateMessageOffsetWithinPage(
        prependedPage,
        prependedLocalIndex,
        estimateMessageHeight,
      );
    if (!Number.isFinite(prependedHeightPx) || prependedHeightPx <= 0) {
      return;
    }

    const viewportHeightPx =
      node.clientHeight > 0 ? node.clientHeight : viewportHeight;
    const shouldKeepTopBoundaryAfterPrepend =
      pendingPrependedTopBoundaryRef.current && node.scrollTop <= 1;
    pendingPrependedTopBoundaryRef.current = false;
    const pendingBottomGapAfterPrepend = pendingPrependedBottomGapRef.current;
    pendingPrependedBottomGapRef.current = null;
    if (shouldKeepTopBoundaryAfterPrepend) {
      shouldKeepBottomAfterLayoutRef.current = false;
      isDetachedFromBottomRef.current = true;
      pendingMountedPrependRestoreRef.current = null;
      skipNextMountedPrependRestoreRef.current = false;
      pendingPrependedMessageAnchorRef.current = null;
      latestVisibleMessageAnchorRef.current = null;
      clearPendingDeferredLayoutTimer();
      const nextMountedRange = buildWorkingMountedRangeForScrollTop(
        0,
        node.clientHeight,
      );
      if (!rangesEqual(mountedPageRangeRef.current, nextMountedRange)) {
        applyMountedPageRange(nextMountedRange);
      }
      writeScrollTopAndSyncViewport(node, 0);
      lastNativeScrollTopRef.current = 0;
      return;
    }
    const shouldPreserveBottomGapAfterPrepend =
      pendingBottomGapAfterPrepend !== null &&
      pendingBottomGapAfterPrepend >= 0 &&
      pendingBottomGapAfterPrepend <= Math.max(viewportHeightPx * 1.5, 72);
    const hasUnsyncedScrollTop =
      Math.abs(node.scrollTop - lastNativeScrollTopRef.current) > 1;
    const preservedAnchor = hasUnsyncedScrollTop
      ? null
      : latestVisibleMessageAnchorRef.current;
    const preservedAnchorSlot = preservedAnchor
      ? findMountedMessageSlotById(renderedListRef.current, preservedAnchor.messageId)
      : null;
    const estimatedTargetScrollTop = node.scrollTop + prependedHeightPx;
    const targetScrollTop = shouldPreserveBottomGapAfterPrepend
      ? clampVirtualizedViewportScrollTop({
          scrollTop:
            pageLayout.totalHeight -
            viewportHeightPx -
            pendingBottomGapAfterPrepend,
          totalHeight: pageLayout.totalHeight,
          viewportHeight: viewportHeightPx,
        })
      : preservedAnchor && preservedAnchorSlot
        ? Math.max(
            node.scrollTop +
              (preservedAnchorSlot.getBoundingClientRect().top -
                node.getBoundingClientRect().top) -
              preservedAnchor.viewportOffsetPx,
            0,
          )
        : clampVirtualizedViewportScrollTop({
            scrollTop: estimatedTargetScrollTop,
            totalHeight: pageLayout.totalHeight,
            viewportHeight: viewportHeightPx,
          });
    const targetNearBottom =
      pageLayout.totalHeight - (targetScrollTop + viewportHeightPx) < 72;
    const preserveDetachedScroll =
      hasUserScrollInteractionRef.current ||
      isDetachedFromBottomRef.current ||
      !isScrollContainerNearBottom(node);
    shouldKeepBottomAfterLayoutRef.current =
      targetNearBottom && !preserveDetachedScroll;
    isDetachedFromBottomRef.current = preserveDetachedScroll || !targetNearBottom;
    pendingMountedPrependRestoreRef.current = null;
    // Preserve any user-scroll skip intent so the next mounted-range prepend
    // restore can still consume it.
    clearPendingDeferredLayoutTimer();
    pendingPrependedMessageAnchorRef.current =
      preservedAnchor && !shouldPreserveBottomGapAfterPrepend
        ? {
            ...preservedAnchor,
            remainingAttempts: 3,
          }
        : null;

    const preservedAnchorLocation =
      preservedAnchor && !shouldPreserveBottomGapAfterPrepend
        ? messageLocationById.get(preservedAnchor.messageId)
        : undefined;
    const nextMountedRange =
      preservedAnchor &&
      !shouldPreserveBottomGapAfterPrepend &&
      preservedAnchorLocation
        ? {
            startIndex: Math.max(preservedAnchorLocation.pageIndex - 3, 0),
            endIndex: Math.min(preservedAnchorLocation.pageIndex + 5, pages.length),
          }
        : buildWorkingMountedRangeForScrollTop(
            targetScrollTop,
            node.clientHeight,
          );
    const mountedRangeWillChange = !rangesEqual(
      mountedPageRangeRef.current,
      nextMountedRange,
    );
    if (mountedRangeWillChange) {
      applyMountedPageRange(nextMountedRange);
    }

    if (
      preservedAnchor &&
      !shouldPreserveBottomGapAfterPrepend &&
      (mountedRangeWillChange || !preservedAnchorSlot)
    ) {
      return;
    }

    writeScrollTopAndSyncViewport(node, targetScrollTop);
    lastNativeScrollTopRef.current = targetScrollTop;
  }, [
    applyMountedPageRange,
    buildWorkingMountedRangeForScrollTop,
    clearPendingDeferredLayoutTimer,
    estimateMessageHeight,
    isActive,
    messageLocationById,
    messages,
    pageLayout.totalHeight,
    pageLayout.tops,
    pages,
    scrollContainerRef,
    sessionId,
    viewportHeight,
    writeScrollTopAndSyncViewport,
  ]);

  useLayoutEffect(() => {
    const pendingAnchor = pendingPrependedMessageAnchorRef.current;
    if (!pendingAnchor) {
      return;
    }

    if (!isActive) {
      pendingPrependedMessageAnchorRef.current = null;
      return;
    }

    const node = scrollContainerRef.current;
    if (!node) {
      return;
    }

    const anchorSlot = findMountedMessageSlotById(
      renderedListRef.current,
      pendingAnchor.messageId,
    );
    if (!anchorSlot) {
      pendingPrependedMessageAnchorRef.current =
        pendingAnchor.remainingAttempts > 1
          ? {
              ...pendingAnchor,
              remainingAttempts: pendingAnchor.remainingAttempts - 1,
            }
          : null;
      return;
    }

    pendingPrependedMessageAnchorRef.current = null;
    const targetScrollTop = Math.max(
      node.scrollTop +
        (anchorSlot.getBoundingClientRect().top - node.getBoundingClientRect().top) -
        pendingAnchor.viewportOffsetPx,
      0,
    );
    writeScrollTopAndSyncViewport(node, targetScrollTop);
    lastNativeScrollTopRef.current = targetScrollTop;
  }, [
    isActive,
    layoutVersion,
    mountedPageRange,
    scrollContainerRef,
    writeScrollTopAndSyncViewport,
  ]);

  useLayoutEffect(() => {
    if (!isActive || pendingPrependedMessageAnchorRef.current) {
      return;
    }

    const node = scrollContainerRef.current;
    if (!node) {
      latestVisibleMessageAnchorRef.current = null;
      return;
    }

    const nextAnchor = captureFirstVisibleMountedMessageAnchor(
      renderedListRef.current,
      node,
    );
    if (nextAnchor) {
      latestVisibleMessageAnchorRef.current = nextAnchor;
    }
  }, [
    isActive,
    layoutVersion,
    mountedPageRange,
    scrollContainerRef,
    viewportScrollTop,
  ]);

  const resolveScrollTopForMessageLocation = useCallback(
    (
      location: MessageLocation,
      options: VirtualizedConversationJumpOptions = {},
    ) => {
      const page = pages[location.pageIndex];
      if (!page) {
        return null;
      }

      const node = scrollContainerRef.current;
      const mountedSlot = findMountedMessageSlotById(
        renderedListRef.current,
        location.message.id,
      );
      let messageTop: number;
      let messageHeight: number;
      if (node && mountedSlot) {
        const nodeRect = node.getBoundingClientRect();
        const slotRect = mountedSlot.getBoundingClientRect();
        messageTop = node.scrollTop + (slotRect.top - nodeRect.top);
        messageHeight =
          slotRect.height > 0
            ? slotRect.height
            : estimateMessageHeight(location.message);
      } else {
        messageTop =
          (pageLayout.tops[location.pageIndex] ?? 0) +
          estimateMessageOffsetWithinPage(
            page,
            location.pageLocalIndex,
            estimateMessageHeight,
          );
        messageHeight = estimateMessageHeight(location.message);
      }

      const align = options.align ?? "center";
      const rawScrollTop =
        align === "start"
          ? messageTop
          : align === "end"
            ? messageTop - Math.max(viewportHeight - messageHeight, 0)
            : messageTop - Math.max((viewportHeight - messageHeight) / 2, 0);
      return clampVirtualizedViewportScrollTop({
        scrollTop: rawScrollTop,
        viewportHeight,
        totalHeight: pageLayout.totalHeight,
      });
    },
    [
      estimateMessageHeight,
      pageLayout.totalHeight,
      pageLayout.tops,
      pages,
      scrollContainerRef,
      viewportHeight,
    ],
  );

  const jumpToMessageLocation = useCallback(
    (
      location: MessageLocation,
      options: VirtualizedConversationJumpOptions = {},
    ) => {
      if (!isActive) {
        return false;
      }

      const node = scrollContainerRef.current;
      if (!node) {
        return false;
      }

      const nextScrollTop = resolveScrollTopForMessageLocation(location, options);
      if (nextScrollTop === null) {
        return false;
      }

      pendingProgrammaticBottomFollowUntilRef.current = Number.NEGATIVE_INFINITY;
      pendingProgrammaticScrollTopRef.current = nextScrollTop;
      pendingAggressiveIdleCompactionRef.current = true;
      pendingMountedPrependRestoreRef.current = null;
      skipNextMountedPrependRestoreRef.current = false;
      clearPendingDeferredLayoutTimer();
      clearPendingIdleCompactionTimer();
      pendingDeferredLayoutAnchorRef.current = null;
      lastUserScrollKindRef.current = null;
      lastUserScrollInputTimeRef.current = Number.NEGATIVE_INFINITY;
      setHasUserScrollInteraction(false);

      const bottomTarget = Math.max(node.scrollHeight - node.clientHeight, 0);
      const targetNearBottom = bottomTarget - nextScrollTop < 72;
      shouldKeepBottomAfterLayoutRef.current = targetNearBottom;
      isDetachedFromBottomRef.current = !targetNearBottom;

      const nextMountedRange = buildWorkingMountedRangeForScrollTop(
        nextScrollTop,
        node.clientHeight,
      );
      if (!rangesEqual(mountedPageRangeRef.current, nextMountedRange)) {
        applyMountedPageRange(nextMountedRange, { flush: options.flush });
      }
      writeScrollTopAndSyncViewport(node, nextScrollTop);
      return true;
    },
    [
      applyMountedPageRange,
      buildWorkingMountedRangeForScrollTop,
      clearPendingDeferredLayoutTimer,
      clearPendingIdleCompactionTimer,
      isActive,
      resolveScrollTopForMessageLocation,
      scrollContainerRef,
      setHasUserScrollInteraction,
      writeScrollTopAndSyncViewport,
    ],
  );

  const buildLayoutSnapshot = useCallback((): VirtualizedConversationLayoutSnapshot => {
    const snapshotMessages: VirtualizedConversationLayoutMessage[] = [];
    pages.forEach((page) => {
      let offsetWithinPage = 0;
      page.messages.forEach((message, pageLocalIndex) => {
        const estimatedHeight = estimateMessageHeight(message);
        snapshotMessages.push({
          messageId: message.id,
          messageIndex: page.startIndex + pageLocalIndex,
          pageIndex: page.pageIndex,
          type: message.type,
          author: message.author,
          estimatedTopPx:
            (pageLayout.tops[page.pageIndex] ?? 0) + offsetWithinPage,
          estimatedHeightPx: estimatedHeight,
          measuredPageHeightPx: pageHeightsRef.current[page.key] ?? null,
        });
        offsetWithinPage += estimatedHeight + VIRTUALIZED_MESSAGE_GAP_PX;
      });
    });

    return {
      sessionId,
      messageCount: messages.length,
      estimatedTotalHeightPx: pageLayout.totalHeight,
      viewportTopPx: viewportScrollTop,
      viewportHeightPx: viewportHeight,
      viewportWidthPx: viewportWidth,
      isActive,
      visiblePageRange,
      mountedPageRange: renderedMountedPageRange,
      messages: snapshotMessages,
    };
  }, [
    estimateMessageHeight,
    isActive,
    messages.length,
    pageLayout.totalHeight,
    pageLayout.tops,
    pages,
    renderedMountedPageRange,
    sessionId,
    viewportHeight,
    viewportScrollTop,
    viewportWidth,
    visiblePageRange,
  ]);

  const buildViewportSnapshot =
    useCallback((): VirtualizedConversationViewportSnapshot => {
      const node = isActive ? scrollContainerRef.current : null;
      const snapshotViewportHeight =
        node && node.clientHeight > 0 ? node.clientHeight : viewportHeight;
      const snapshotViewportWidth =
        node && node.clientWidth > 0 ? node.clientWidth : viewportWidth;
      const snapshotScrollHeight =
        node && node.scrollHeight > 0 ? node.scrollHeight : pageLayout.totalHeight;
      const snapshotViewportTop = clampVirtualizedViewportScrollTop({
        scrollTop: node ? node.scrollTop : viewportScrollTop,
        totalHeight: snapshotScrollHeight,
        viewportHeight: snapshotViewportHeight,
      });

      return {
        sessionId,
        messageCount: messages.length,
        windowStartMessageId: messages[0]?.id ?? null,
        windowEndMessageId: messages[messages.length - 1]?.id ?? null,
        estimatedTotalHeightPx: snapshotScrollHeight,
        viewportTopPx: snapshotViewportTop,
        viewportHeightPx: snapshotViewportHeight,
        viewportWidthPx: snapshotViewportWidth,
        isActive,
        visiblePageRange,
        mountedPageRange: renderedMountedPageRange,
      };
    }, [
      isActive,
      messages.length,
      pageLayout.totalHeight,
      renderedMountedPageRange,
      scrollContainerRef,
      sessionId,
      viewportHeight,
      viewportScrollTop,
      viewportWidth,
      visiblePageRange,
    ]);

  type VirtualizerHandleState = {
    buildLayoutSnapshot: typeof buildLayoutSnapshot;
    buildViewportSnapshot: typeof buildViewportSnapshot;
    jumpToMessageLocation: typeof jumpToMessageLocation;
    messageLocationById: typeof messageLocationById;
    messagesLength: number;
    pages: typeof pages;
  };
  const virtualizerHandleStateRef = useRef<VirtualizerHandleState | null>(null);
  if (virtualizerHandleStateRef.current === null) {
    virtualizerHandleStateRef.current = {
      buildLayoutSnapshot,
      buildViewportSnapshot,
      jumpToMessageLocation,
      messageLocationById,
      messagesLength: messages.length,
      pages,
    };
  }
  useLayoutEffect(() => {
    virtualizerHandleStateRef.current = {
      buildLayoutSnapshot,
      buildViewportSnapshot,
      jumpToMessageLocation,
      messageLocationById,
      messagesLength: messages.length,
      pages,
    };
  }, [
    buildLayoutSnapshot,
    buildViewportSnapshot,
    jumpToMessageLocation,
    messageLocationById,
    messages.length,
    pages,
  ]);
  const readVirtualizerHandleState = useCallback(() => {
    const state = virtualizerHandleStateRef.current;
    if (state === null) {
      throw new Error("virtualizer handle state is not initialized");
    }
    return state;
  }, []);
  const virtualizerStableHandle = useMemo<VirtualizedConversationMessageListHandle>(
    () => ({
      getLayoutSnapshot: () =>
        readVirtualizerHandleState().buildLayoutSnapshot(),
      getViewportSnapshot: () =>
        readVirtualizerHandleState().buildViewportSnapshot(),
      jumpToMessageId: (messageId, options) => {
        const { jumpToMessageLocation, messageLocationById } =
          readVirtualizerHandleState();
        const location = messageLocationById.get(messageId);
        return location ? jumpToMessageLocation(location, options) : false;
      },
      jumpToMessageIndex: (messageIndex, options) => {
        const { jumpToMessageLocation, messagesLength, pages } =
          readVirtualizerHandleState();
        if (
          !Number.isInteger(messageIndex) ||
          messageIndex < 0 ||
          messageIndex >= messagesLength
        ) {
          return false;
        }

        const pageIndex = Math.floor(messageIndex / VIRTUALIZED_MESSAGES_PER_PAGE);
        const page = pages[pageIndex];
        if (!page || messageIndex < page.startIndex || messageIndex >= page.endIndex) {
          return false;
        }

        const pageLocalIndex = messageIndex - page.startIndex;
        const message = page.messages[pageLocalIndex];
        if (!message) {
          return false;
        }

        return jumpToMessageLocation(
          {
            message,
            messageIndex,
            pageIndex,
            pageLocalIndex,
          },
          options,
        );
      },
    }),
    [readVirtualizerHandleState],
  );

  useLayoutEffect(() => {
    if (!virtualizerHandleRef) {
      return undefined;
    }

    virtualizerHandleRef.current = virtualizerStableHandle;
    return () => {
      if (virtualizerHandleRef.current === virtualizerStableHandle) {
        virtualizerHandleRef.current = null;
      }
    };
  }, [virtualizerHandleRef, virtualizerStableHandle]);

  useEffect(() => {
    return () => {
      clearPendingDeferredLayoutTimer();
      clearPendingDeferredRenderResumeTimer();
      clearPendingIdleCompactionTimer();
      pendingDeferredRenderSuspendedNodeRef.current?.removeAttribute(
        DEFERRED_RENDER_SUSPENDED_ATTRIBUTE,
      );
      pendingDeferredRenderSuspendedNodeRef.current = null;
      pendingProgrammaticViewportSyncRef.current = false;
    };
  }, [
    clearPendingDeferredLayoutTimer,
    clearPendingDeferredRenderResumeTimer,
    clearPendingIdleCompactionTimer,
  ]);

  useEffect(() => {
    pageHeightsRef.current = Object.fromEntries(
      Object.entries(pageHeightsRef.current).filter(([pageKey]) => pageKeys.has(pageKey)),
    );
    estimatedPageHeightsRef.current = Object.fromEntries(
      Object.entries(estimatedPageHeightsRef.current).filter(([pageKey]) =>
        pageKeys.has(pageKey),
      ),
    );
  }, [pageKeys]);

  useLayoutEffect(() => {
    if (!isActive || pendingPrependedMessageAnchorRef.current) {
      return;
    }

    if (pages.length === 0) {
      return;
    }

    const inUserScrollCooldown =
      performance.now() - lastUserScrollInputTimeRef.current < VIRTUALIZED_USER_SCROLL_ADJUSTMENT_COOLDOWN_MS;
    const userScrollKind = lastUserScrollKindRef.current;
    // Only incremental scroll should grow via this layout effect. Seek-style
    // jumps (PageUp/PageDown/Home/End, large scrollbar moves) are owned by the
    // native-scroll reconcile path; letting this effect prepend pages during
    // the same gesture reintroduces a restore that can snap the viewport back.
    if (!inUserScrollCooldown || userScrollKind !== "incremental") {
      return;
    }
    if (!rangeContainsRange(mountedPageRange, visiblePageRange)) {
      return;
    }

    const nextRange = {
      startIndex: Math.min(mountedPageRange.startIndex, workingMountedPageRange.startIndex),
      endIndex: Math.max(mountedPageRange.endIndex, workingMountedPageRange.endIndex),
    };

    if (!rangesEqual(nextRange, mountedPageRange)) {
      const node = scrollContainerRef.current;
      if (
        node &&
        nextRange.startIndex < mountedPageRange.startIndex &&
        !isScrollContainerNearBottom(node)
      ) {
        pendingMountedPrependRestoreRef.current =
          captureMountedPrependRestore(node);
      }
      applyMountedPageRange(nextRange);
    }
  }, [
    applyMountedPageRange,
    captureMountedPrependRestore,
    isActive,
    mountedPageRange,
    pages.length,
    scrollContainerRef,
    visiblePageRange,
    workingMountedPageRange,
  ]);

  useLayoutEffect(() => {
    if (pendingPrependedMessageAnchorRef.current) {
      return;
    }

    if (rangesEqual(mountedPageRange, workingMountedPageRange)) {
      return;
    }

    const inUserScrollCooldown =
      performance.now() - lastUserScrollInputTimeRef.current < VIRTUALIZED_USER_SCROLL_ADJUSTMENT_COOLDOWN_MS;
    const viewportEscapedMountedBand = !rangeContainsRange(
      mountedPageRange,
      visiblePageRange,
    );
    if (inUserScrollCooldown) {
      // Active scroll is grow-only. Keep extra DOM on the opposite side until
      // the gesture settles; trimming during motion is what exposed spacer
      // blanks on the way down and snap-backs on the way up.
      if (lastUserScrollKindRef.current === "seek" && viewportEscapedMountedBand) {
        applyMountedPageRange(workingMountedPageRange);
      }
      return;
    }

    const mountedStillCoversViewport = rangeContainsRange(
      mountedPageRange,
      visiblePageRange,
    );
    const excessAbovePages = Math.max(
      workingMountedPageRange.startIndex - mountedPageRange.startIndex,
      0,
    );
    const excessBelowPages = Math.max(
      mountedPageRange.endIndex - workingMountedPageRange.endIndex,
      0,
    );
    const allowHysteresis = !pendingAggressiveIdleCompactionRef.current;
    if (
      allowHysteresis &&
      mountedStillCoversViewport &&
      excessAbovePages <= IDLE_MOUNTED_COMPACTION_PAGE_HYSTERESIS &&
      excessBelowPages <= IDLE_MOUNTED_COMPACTION_PAGE_HYSTERESIS
    ) {
      return;
    }

    const node = scrollContainerRef.current;
    if (
      isActive &&
      node &&
      !pendingPrependedMessageAnchorRef.current &&
      workingMountedPageRange.startIndex > mountedPageRange.startIndex &&
      !isScrollContainerNearBottom(node)
    ) {
      pendingMountedPrependRestoreRef.current =
        captureMountedPrependRestore(node);
    }

    pendingAggressiveIdleCompactionRef.current = false;
    applyMountedPageRange(workingMountedPageRange);
  }, [
    applyMountedPageRange,
    captureMountedPrependRestore,
    isActive,
    mountedPageRange,
    scrollContainerRef,
    scrollIdleVersion,
    visiblePageRange,
    workingMountedPageRange,
  ]);

  useLayoutEffect(() => {
    if (
      !isActive ||
      searchPinnedMountedPageRange !== null ||
      mountedPageRange.startIndex <= 0
    ) {
      return;
    }

    const isUserScrollCooldown =
      hasUserScrollInteractionRef.current &&
      performance.now() - lastUserScrollInputTimeRef.current <
        VIRTUALIZED_USER_SCROLL_ADJUSTMENT_COOLDOWN_MS;
    const isProgrammaticBottomFollowCooldown =
      pendingProgrammaticBottomFollowUntilRef.current >= performance.now();
    if (!isUserScrollCooldown || isProgrammaticBottomFollowCooldown) {
      return;
    }

    const node = scrollContainerRef.current;
    if (!node || !renderedListRef.current) {
      return;
    }

    const pageNodes = Array.from(
      renderedListRef.current.querySelectorAll<HTMLElement>(
        ".virtualized-message-page[data-page-key]",
      ),
    );
    const renderedCoverageHeight = resolveRenderedPageCoverageHeight(pageNodes);
    const firstPage = pageNodes[0];
    if (!firstPage) {
      return;
    }

    // Symmetric to the bottom-edge coverage guard below: when an upward wheel
    // move outruns React's range update, the viewport can briefly land in the
    // top spacer. Grow from actual DOM bounds so the spacer is replaced by
    // mounted pages before the user sees a blank slab.
    const containerRect = node.getBoundingClientRect();
    let missingAbovePx = firstPage.getBoundingClientRect().top - containerRect.top;
    if (missingAbovePx <= 0) {
      return;
    }

    let nextStartIndex = mountedPageRange.startIndex;
    while (missingAbovePx > 0 && nextStartIndex > 0) {
      nextStartIndex -= 1;
      missingAbovePx -= resolvePageCoverageHeight(
        pageHeights,
        nextStartIndex,
        renderedCoverageHeight,
      );
    }
    if (nextStartIndex >= mountedPageRange.startIndex) {
      return;
    }

    pendingMountedPrependRestoreRef.current =
      captureMountedPrependRestore(node);
    applyMountedPageRange({
      startIndex: nextStartIndex,
      endIndex: mountedPageRange.endIndex,
    });
  }, [
    applyMountedPageRange,
    captureMountedPrependRestore,
    isActive,
    layoutVersion,
    mountedPageRange,
    pageHeights,
    scrollContainerRef,
    searchPinnedMountedPageRange,
    viewportScrollTop,
  ]);

  useLayoutEffect(() => {
    if (
      !isActive ||
      searchPinnedMountedPageRange !== null ||
      mountedPageRange.endIndex >= pages.length
    ) {
      return;
    }

    const isUserScrollCooldown =
      hasUserScrollInteractionRef.current &&
      performance.now() - lastUserScrollInputTimeRef.current <
        VIRTUALIZED_USER_SCROLL_ADJUSTMENT_COOLDOWN_MS;
    if (!isUserScrollCooldown) {
      return;
    }

    const node = scrollContainerRef.current;
    if (!node || !renderedListRef.current) {
      return;
    }

    const pageNodes = Array.from(
      renderedListRef.current.querySelectorAll<HTMLElement>(
        ".virtualized-message-page[data-page-key]",
      ),
    );
    const renderedCoverageHeight = resolveRenderedPageCoverageHeight(pageNodes);
    const lastPage = pageNodes[pageNodes.length - 1];
    if (!lastPage) {
      return;
    }

    // Normal range reconciliation works from estimated page heights. When
    // mounted pages measure shorter than their estimates during a scroll
    // cooldown, the estimated working range can still look valid while the
    // real DOM ends inside the viewport. Grow downward from actual DOM bounds
    // so compact command-heavy transcripts do not expose the bottom spacer as
    // blank pages until the next user scroll. This deliberately covers the
    // visible viewport, not the full below-overscan band, so it cannot fight
    // normal idle compaction.
    const containerRect = node.getBoundingClientRect();
    const renderedMountedBottom =
      node.scrollTop + (lastPage.getBoundingClientRect().bottom - containerRect.top);
    let missingBelowPx = node.scrollTop + node.clientHeight - renderedMountedBottom;
    if (missingBelowPx <= 0) {
      return;
    }

    let nextEndIndex = mountedPageRange.endIndex;
    while (missingBelowPx > 0 && nextEndIndex < pages.length) {
      missingBelowPx -= resolvePageCoverageHeight(
        pageHeights,
        nextEndIndex,
        renderedCoverageHeight,
      );
      nextEndIndex += 1;
    }
    if (nextEndIndex <= mountedPageRange.endIndex) {
      return;
    }

    applyMountedPageRange({
      startIndex: mountedPageRange.startIndex,
      endIndex: nextEndIndex,
    });
  }, [
    applyMountedPageRange,
    isActive,
    layoutVersion,
    mountedPageRange,
    pageHeights,
    pages.length,
    scrollContainerRef,
    searchPinnedMountedPageRange,
    viewportScrollTop,
  ]);

  useLayoutEffect(() => {
    if (
      !isActive ||
      !hasConversationSearch ||
      !activeConversationSearchPositionKey ||
      !activeConversationSearchMessageId ||
      activeConversationSearchScrollTop === null
    ) {
      lastPinnedConversationSearchPositionKeyRef.current = null;
      return;
    }

    // The mount-band pin above uses the full search key so query changes can
    // re-arm mounted pages. This scroll-position guard only keys by session
    // and message so refining the query does not keep re-centering the same
    // active match while the user types.
    if (lastPinnedConversationSearchPositionKeyRef.current === activeConversationSearchPositionKey) {
      return;
    }

    const node = scrollContainerRef.current;
    if (!node) {
      return;
    }

    const mountedTargetSlot = findMountedMessageSlotById(
      renderedListRef.current,
      activeConversationSearchMessageId,
    );
    if (!mountedTargetSlot) {
      const nextMountedRange = buildWorkingMountedRangeForScrollTop(
        activeConversationSearchScrollTop,
        node.clientHeight,
      );
      if (!rangesEqual(mountedPageRangeRef.current, nextMountedRange)) {
        pendingMountedPrependRestoreRef.current = null;
        applyMountedPageRange(nextMountedRange);
      }
      if (Math.abs(node.scrollTop - activeConversationSearchScrollTop) >= 1) {
        shouldKeepBottomAfterLayoutRef.current = false;
      }
      writeScrollTopAndSyncViewport(node, activeConversationSearchScrollTop);
      return;
    }

    const nodeRect = node.getBoundingClientRect();
    const targetRect = mountedTargetSlot.getBoundingClientRect();
    const hasMountedTargetGeometry =
      targetRect.height > 0 || Math.abs(targetRect.top - nodeRect.top) >= 1;
    const nextScrollTop = hasMountedTargetGeometry
      ? Math.max(
          node.scrollTop +
            (targetRect.top - nodeRect.top) -
            Math.max((viewportHeight - targetRect.height) / 2, 0),
          0,
        )
      : activeConversationSearchScrollTop;

    const nextMountedRange = buildWorkingMountedRangeForScrollTop(nextScrollTop, node.clientHeight);
    if (!rangesEqual(mountedPageRangeRef.current, nextMountedRange)) {
      pendingMountedPrependRestoreRef.current = null;
      applyMountedPageRange(nextMountedRange);
    }
    if (Math.abs(node.scrollTop - nextScrollTop) >= 1) {
      shouldKeepBottomAfterLayoutRef.current = false;
    }
    writeScrollTopAndSyncViewport(node, nextScrollTop);
    lastPinnedConversationSearchPositionKeyRef.current = activeConversationSearchPositionKey;
  }, [
    activeConversationSearchMessageId,
    activeConversationSearchPositionKey,
    activeConversationSearchScrollTop,
    applyMountedPageRange,
    buildWorkingMountedRangeForScrollTop,
    hasConversationSearch,
    isActive,
    scrollContainerRef,
    viewportHeight,
    writeScrollTopAndSyncViewport,
  ]);

  useLayoutEffect(() => {
    const pendingRestore = pendingMountedPrependRestoreRef.current;
    if (!pendingRestore) {
      return;
    }
    pendingMountedPrependRestoreRef.current = null;

    const node = scrollContainerRef.current;
    if (!isActive || !node) {
      return;
    }

    const anchorSlot = pendingRestore.anchor
      ? findMountedMessageSlotById(
          renderedListRef.current,
          pendingRestore.anchor.messageId,
        )
      : null;
    const targetScrollTop = anchorSlot
      ? Math.max(
          node.scrollTop +
            (anchorSlot.getBoundingClientRect().top - node.getBoundingClientRect().top) -
            pendingRestore.anchor!.viewportOffsetPx,
          0,
        )
      : pendingRestore.scrollTop + (node.scrollHeight - pendingRestore.scrollHeight);
    writeScrollTopAndSyncViewport(node, targetScrollTop);
    lastNativeScrollTopRef.current = targetScrollTop;
  }, [
    isActive,
    mountedPageRange,
    scrollContainerRef,
    writeScrollTopAndSyncViewport,
  ]);

  useLayoutEffect(() => {
    const pendingAnchor = pendingDeferredLayoutAnchorRef.current;
    if (!pendingAnchor) {
      return;
    }
    pendingDeferredLayoutAnchorRef.current = null;

    const node = scrollContainerRef.current;
    if (!isActive || !node) {
      return;
    }

    const anchorSlot = findMountedMessageSlotById(
      renderedListRef.current,
      pendingAnchor.messageId,
    );
    if (!anchorSlot) {
      return;
    }

    const targetScrollTop = Math.max(
      node.scrollTop +
        (anchorSlot.getBoundingClientRect().top - node.getBoundingClientRect().top) -
        pendingAnchor.viewportOffsetPx,
      0,
    );
    writeScrollTopAndSyncViewport(node, targetScrollTop);
  }, [
    isActive,
    layoutVersion,
    mountedPageRange,
    scrollContainerRef,
    writeScrollTopAndSyncViewport,
  ]);

  useLayoutEffect(() => {
    if (isMeasuringPostActivation && !isDetachedFromBottomRef.current) {
      shouldKeepBottomAfterLayoutRef.current = true;
    }
  }, [isMeasuringPostActivation]);

  useLayoutEffect(() => {
    if (!isMeasuringPostActivation) {
      return;
    }
    if (!isActive) {
      setIsMeasuringPostActivation(false);
      return;
    }

    const visiblePages = pages.slice(visiblePageRange.startIndex, visiblePageRange.endIndex);
    if (visiblePages.length === 0) {
      setIsMeasuringPostActivation(false);
      return;
    }

    const allMeasured = visiblePages.every((page) => pageHeightsRef.current[page.key] !== undefined);
    if (!allMeasured) {
      return;
    }

    if (hasUserScrollInteractionRef.current) {
      shouldKeepBottomAfterLayoutRef.current = false;
      setIsMeasuringPostActivation(false);
      return;
    }

    if (isDetachedFromBottomRef.current) {
      setIsMeasuringPostActivation(false);
      return;
    }

    const node = scrollContainerRef.current;
    if (node) {
      if (pages.length >= POST_ACTIVATION_ESTIMATED_BOTTOM_MIN_PAGES) {
        writeEstimatedScrollTopAndSyncViewport(
          node,
          resolveEstimatedBottomScrollTop(),
        );
      } else {
        const target = Math.max(node.scrollHeight - node.clientHeight, 0);
        writeScrollTopAndSyncViewport(node, target);
      }
    }
    if (pendingBottomBoundaryRevealFrameRef.current !== null) {
      return;
    }
    setIsMeasuringPostActivation(false);
  }, [
    isActive,
    isMeasuringPostActivation,
    pages,
    resolveEstimatedBottomScrollTop,
    scrollContainerRef,
    visiblePageRange.endIndex,
    visiblePageRange.startIndex,
    writeEstimatedScrollTopAndSyncViewport,
    writeScrollTopAndSyncViewport,
  ]);

  useEffect(() => {
    if (!isMeasuringPostActivation) {
      return;
    }

    const timeoutId = window.setTimeout(() => {
      if (hasUserScrollInteractionRef.current) {
        shouldKeepBottomAfterLayoutRef.current = false;
        setIsMeasuringPostActivation(false);
        return;
      }
      if (isDetachedFromBottomRef.current) {
        setIsMeasuringPostActivation(false);
        return;
    }
    const node = scrollContainerRef.current;
    if (node) {
      if (pages.length >= POST_ACTIVATION_ESTIMATED_BOTTOM_MIN_PAGES) {
        writeEstimatedScrollTopAndSyncViewport(
          node,
          resolveEstimatedBottomScrollTop(),
        );
      } else {
        const target = Math.max(node.scrollHeight - node.clientHeight, 0);
        writeScrollTopAndSyncViewport(node, target);
      }
    }
    if (pendingBottomBoundaryRevealFrameRef.current !== null) {
      return;
    }
    shouldKeepBottomAfterLayoutRef.current = true;
    setIsMeasuringPostActivation(false);
    }, 150);

    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [
    isMeasuringPostActivation,
    pages.length,
    resolveEstimatedBottomScrollTop,
    scrollContainerRef,
    writeEstimatedScrollTopAndSyncViewport,
    writeScrollTopAndSyncViewport,
  ]);

  useLayoutEffect(() => {
    if (!isActive || !shouldKeepBottomAfterLayoutRef.current || isDetachedFromBottomRef.current) {
      return;
    }
    // A programmatic `bottom_follow` smooth-scroll is in flight; the browser is
    // animating toward the previous bottom. Hard-writing scrollTop here would
    // cancel that animation mid-flight (visible jank — scrollbar snaps instead
    // of glides). The cooldown ends naturally when the animation lands or when
    // a real user gesture fires `markUserScroll`.
    if (pendingProgrammaticBottomFollowUntilRef.current >= performance.now()) {
      return;
    }

    const timeSinceUserScroll = performance.now() - lastUserScrollInputTimeRef.current;
    if (timeSinceUserScroll < VIRTUALIZED_USER_SCROLL_ADJUSTMENT_COOLDOWN_MS) {
      return;
    }

    const node = scrollContainerRef.current;
    if (!node) {
      return;
    }
    if (hasUserScrollInteractionRef.current && !isScrollContainerNearBottom(node)) {
      shouldKeepBottomAfterLayoutRef.current = false;
      return;
    }

    if (
      isMeasuringPostActivation &&
      pages.length >= POST_ACTIVATION_ESTIMATED_BOTTOM_MIN_PAGES
    ) {
      writeEstimatedScrollTopAndSyncViewport(
        node,
        resolveEstimatedBottomScrollTop(),
      );
      return;
    }

    const target = Math.max(node.scrollHeight - node.clientHeight, 0);
    writeScrollTopAndSyncViewport(node, target);
  }, [
    isActive,
    isMeasuringPostActivation,
    layoutVersion,
    pageLayout.totalHeight,
    pages.length,
    resolveEstimatedBottomScrollTop,
    scrollContainerRef,
    writeEstimatedScrollTopAndSyncViewport,
    writeScrollTopAndSyncViewport,
  ]);

  useLayoutEffect(() => {
    if (!isActive || !pendingBottomBoundarySeekRef.current) {
      return;
    }

    const node = scrollContainerRef.current;
    if (!node) {
      return;
    }

    pendingBottomBoundarySeekRef.current = false;
    shouldKeepBottomAfterLayoutRef.current = true;
    isDetachedFromBottomRef.current = false;
    const target = Math.max(node.scrollHeight - node.clientHeight, 0);
    writeScrollTopAndSyncViewport(node, target);
  }, [
    bottomBoundarySeekVersion,
    isActive,
    mountedPageRange.endIndex,
    mountedPageRange.startIndex,
    scrollContainerRef,
    writeScrollTopAndSyncViewport,
  ]);

  useLayoutEffect(() => {
    if (!isActive) {
      return;
    }

    const node = scrollContainerRef.current;
    if (!node) {
      return;
    }

    const enterBottomFollowMode = () => {
      pendingProgrammaticScrollTopRef.current = null;
      lastNativeScrollTopRef.current = node.scrollTop;
      shouldKeepBottomAfterLayoutRef.current = true;
      isDetachedFromBottomRef.current = false;
      setHasUserScrollInteraction(false);
      lastUserScrollKindRef.current = null;
      lastUserScrollInputTimeRef.current = Number.NEGATIVE_INFINITY;
    };

    const syncViewport = (options: { isNativeScrollEvent?: boolean } = {}) => {
      const isBottomBoundaryRevealScroll =
        pendingBottomBoundaryRevealNodeRef.current === node;
      const isProgrammaticBottomFollowScroll =
        options.isNativeScrollEvent === true &&
        lastUserScrollInputTimeRef.current === Number.NEGATIVE_INFINITY &&
        pendingProgrammaticBottomFollowUntilRef.current >= performance.now() &&
        node.scrollTop >= lastNativeScrollTopRef.current - 1;
      if (options.isNativeScrollEvent) {
        const pendingProgrammaticScrollTop = pendingProgrammaticScrollTopRef.current;
        const isProgrammaticScrollEvent =
          pendingProgrammaticScrollTop !== null &&
          Math.abs(node.scrollTop - pendingProgrammaticScrollTop) < 1;
        if (isProgrammaticScrollEvent || isBottomBoundaryRevealScroll) {
          pendingProgrammaticScrollTopRef.current = null;
          lastNativeScrollTopRef.current = node.scrollTop;
        } else if (isProgrammaticBottomFollowScroll) {
          pendingProgrammaticBottomFollowUntilRef.current =
            performance.now() + MESSAGE_STACK_BOTTOM_FOLLOW_SCROLL_MS;
          enterBottomFollowMode();
        } else {
          const hadUserScrollInteraction = hasUserScrollInteractionRef.current;
          if (isMeasuringPostActivation) {
            cancelPostActivationBottomRestore();
          }
          clearPendingDeferredLayoutTimer();
          pendingDeferredLayoutAnchorRef.current = null;
          const scrollDelta = node.scrollTop - lastNativeScrollTopRef.current;
          lastNativeScrollTopRef.current = node.scrollTop;
          if (Math.abs(scrollDelta) >= 0.5) {
            pendingPrependedBottomGapRef.current = null;
          }
          const isPassiveTailFollowScroll =
            tailFollowIntent &&
            !hadUserScrollInteraction &&
            !isDetachedFromBottomRef.current;
          if (lastUserScrollKindRef.current === null) {
            lastUserScrollKindRef.current = resolveNativeScrollKind(
              lastUserScrollKindRef.current,
              scrollDelta,
              node.clientHeight,
            );
          }
          if (isPassiveTailFollowScroll) {
            shouldKeepBottomAfterLayoutRef.current = true;
          } else {
            releaseConversationSearchPinForUserScroll();
            setHasUserScrollInteraction(true);
            lastUserScrollInputTimeRef.current = performance.now();
            captureLatestVisibleMessageAnchor(node);
            scheduleIdleMountedRangeCompaction(VIRTUALIZED_USER_SCROLL_ADJUSTMENT_COOLDOWN_MS);
          }
          // Scrollbar-thumb drag and touch-inertia scrolls have no preceding
          // wheel/touch/key event, so `prewarmMountedRangeForUpwardWheel`
          // never runs for them. Without flush, `setMountedPageRange` batches
          // until the next React commit — and the browser paints the new
          // scroll position first, exposing the top spacer as a visible void.
          // Flush on upward scrolls (when not already near the bottom) so the
          // mount + the prepend-restore in the consumer layout-effect run
          // synchronously inside the scroll handler, before the next paint.
          // The matching pane-write path already uses the same condition; we
          // keep the conditions symmetric so neither code path can outrun the
          // other.
          const isActiveUpwardNativeScroll =
            !isPassiveTailFollowScroll &&
            scrollDelta < 0 &&
            !isScrollContainerNearBottom(node);
          reconcileMountedRangeForNativeScroll(
            node,
            scrollDelta,
            lastUserScrollKindRef.current,
            { flush: isActiveUpwardNativeScroll },
          );
          if (scrollDelta >= 0 && isScrollContainerNearBottom(node)) {
            // The user just scrolled DOWN to (or stayed at) the
            // bottom. Re-arm the bottom-follow flags so subsequent
            // layout changes can keep the view pinned, and clear the
            // user-interaction flag so the auto-scroll layout effect
            // does not bail when an incoming streamed delta grows
            // the layout past the near-bottom threshold for one
            // frame.
            //
            // We deliberately do NOT call the full `enterBottomFollowMode()`
            // helper here. That helper also resets
            // `lastUserScrollInputTimeRef.current` to NEGATIVE_INFINITY,
            // which bypasses the user-scroll cooldown that
            // `handlePageHeightChange` (the page-measure callback)
            // relies on to suppress its own scroll-write rAF. That
            // handler is a SEPARATE scroll path from the auto-scroll
            // layout effect — it only checks the user-scroll cooldown
            // (`VIRTUALIZED_USER_SCROLL_ADJUSTMENT_COOLDOWN_MS`), not
            // `pendingProgrammaticBottomFollowUntilRef`. Clearing the
            // user-scroll input time here makes a subsequent page
            // remeasure (e.g., a ResizeObserver firing after the user
            // briefly inertial-scrolled past the bottom-follow target)
            // race the bottom-follow cooldown and write scrollTop —
            // which is what the
            // `does not let bottom-follow recapture later inertial
            // native scroll ticks` regression in
            // `panels/AgentSessionPanel.test.tsx` pins.
            isDetachedFromBottomRef.current = false;
            shouldKeepBottomAfterLayoutRef.current = true;
            setHasUserScrollInteraction(false);
            // Do not carry any prior classification into the next native scroll
            // tick. A later scrollbar drag has no wheel/key/touch prelude, so it
            // must be classified from its own delta instead of inheriting the
            // bottom re-entry scroll.
            lastUserScrollKindRef.current = resolveBottomReentryScrollKind();
            clearPendingIdleCompactionTimer();
          }
        }
      }

      syncViewportFromScrollNode(node);

      if (
        shouldKeepBottomAfterLayoutRef.current &&
        !isBottomBoundaryRevealScroll &&
        !isProgrammaticBottomFollowScroll &&
        !(tailFollowIntent && !hasUserScrollInteractionRef.current) &&
        !isScrollContainerNearBottom(node)
      ) {
        shouldKeepBottomAfterLayoutRef.current = false;
      }
    };

    const markUserScroll = (event?: WheelEvent | TouchEvent | KeyboardEvent) => {
      let wheelDeltaY: number | null = null;
      let touchDeltaY: number | null = null;
      if (event?.type === "wheel" && "deltaY" in event) {
        const wheelEvent = event as WheelEvent;
        wheelDeltaY = normalizeWheelDelta(wheelEvent, node);
        if (
          wheelEvent.ctrlKey ||
          Math.abs(wheelDeltaY) < 0.5 ||
          canNestedScrollableConsumeWheel(wheelEvent.target, node, wheelDeltaY)
        ) {
          return;
        }
      } else if (typeof TouchEvent !== "undefined" && event instanceof TouchEvent) {
        const touch = event.touches[0] ?? event.changedTouches[0] ?? null;
        if (touch) {
          const previousTouchClientY = lastTouchClientYRef.current;
          lastTouchClientYRef.current = touch.clientY;
          if (previousTouchClientY !== null) {
            // Finger moves down => scrollTop moves up, matching a negative
            // wheel delta. Feed the same prewarm path before native scroll
            // exposes the top spacer.
            touchDeltaY = previousTouchClientY - touch.clientY;
          }
        }
      }
      const inputScrollDeltaY = wheelDeltaY ?? touchDeltaY;
      const bottomGapBeforeInput = getScrollContainerBottomGap(node);
      const upwardInputDeltaPx =
        inputScrollDeltaY !== null && inputScrollDeltaY < 0
          ? Math.abs(inputScrollDeltaY)
          : null;
      const isLikelyBottomEscape =
        upwardInputDeltaPx !== null
          ? bottomGapBeforeInput <= 72 + upwardInputDeltaPx
          : isScrollContainerNearBottom(node);
      const visibleAnchorBeforeNativeScroll =
        captureFirstVisibleMountedMessageAnchor(renderedListRef.current, node);
      if (visibleAnchorBeforeNativeScroll) {
        // Wheel delta is the browser's intended scroll delta. Touch delta is a
        // finger movement approximation; at scroll boundaries or inside nested
        // scrollers it may not correspond to any scrollTop change, so leave the
        // anchor unshifted until the native scroll handler observes real motion.
        latestVisibleMessageAnchorRef.current =
          wheelDeltaY !== null
            ? {
                ...visibleAnchorBeforeNativeScroll,
                viewportOffsetPx:
                  visibleAnchorBeforeNativeScroll.viewportOffsetPx - wheelDeltaY,
              }
            : visibleAnchorBeforeNativeScroll;
      }
      pendingProgrammaticBottomFollowUntilRef.current =
        Number.NEGATIVE_INFINITY;
      pendingPrependedTopBoundaryRef.current = false;
      if (
        upwardInputDeltaPx === null ||
        !isLikelyBottomEscape
      ) {
        pendingPrependedBottomGapRef.current = null;
      }
      releaseConversationSearchPinForUserScroll();
      if (isMeasuringPostActivation) {
        cancelPostActivationBottomRestore();
      }
      suspendDeferredRenderActivation(node);
      clearPendingDeferredLayoutTimer();
      pendingDeferredLayoutAnchorRef.current = null;
      const isSeekKeyboardNavigation =
        event instanceof KeyboardEvent &&
        (event.key === "Home" || event.key === "End");
      const isPageJumpKeyboardNavigation =
        event instanceof KeyboardEvent &&
        (event.key === "PageUp" || event.key === "PageDown");
      const isExplicitUpwardScrollIntent =
        (wheelDeltaY !== null && wheelDeltaY < 0) ||
        (touchDeltaY !== null && touchDeltaY < 0) ||
        (event instanceof KeyboardEvent &&
          (event.key === "PageUp" ||
            event.key === "ArrowUp" ||
            event.key === "Home"));
      if (isExplicitUpwardScrollIntent) {
        if (isLikelyBottomEscape) {
          // Preserve the browser's first upward escape from the bottom. The
          // mounted-band prepend that follows should expand DOM above without
          // replaying a scrollHeight-delta restore that can undo the page jump.
          skipNextMountedPrependRestoreRef.current = true;
          if (upwardInputDeltaPx !== null) {
            pendingPrependedBottomGapRef.current =
              bottomGapBeforeInput + upwardInputDeltaPx;
          }
        }
        // The first upward gesture from the bottom should always break the
        // "stick to latest" intent immediately. Waiting until the native
        // scroll lands more than 72 px away keeps the bottom-pin armed long
        // enough for a later layout tick to snap the viewport back down once.
        shouldKeepBottomAfterLayoutRef.current = false;
        isDetachedFromBottomRef.current = true;
      }
      lastUserScrollKindRef.current = isSeekKeyboardNavigation
        ? "seek"
        : isPageJumpKeyboardNavigation
          ? "page_jump"
          : "incremental";
      setHasUserScrollInteraction(true);
      lastUserScrollInputTimeRef.current = performance.now();
      scheduleIdleMountedRangeCompaction(VIRTUALIZED_USER_SCROLL_ADJUSTMENT_COOLDOWN_MS);
      if (inputScrollDeltaY !== null && inputScrollDeltaY < 0) {
        prewarmMountedRangeForUpwardWheel(node, inputScrollDeltaY);
      }
    };
    const syncProgrammaticScrollWrite = (event: Event) => {
      const explicitScrollKind =
        event instanceof CustomEvent
          ? ((event.detail as MessageStackScrollWriteDetail | undefined)?.scrollKind ?? null)
          : null;
      const explicitScrollSource =
        event instanceof CustomEvent
          ? ((event.detail as MessageStackScrollWriteDetail | undefined)
              ?.scrollSource ?? "programmatic")
          : "programmatic";

      if (
        explicitScrollKind === "bottom_pin" ||
        explicitScrollKind === "bottom_boundary"
      ) {
        pendingPrependedTopBoundaryRef.current = false;
        pendingPrependedBottomGapRef.current = null;
        pendingProgrammaticBottomFollowUntilRef.current =
          Number.NEGATIVE_INFINITY;
        pendingProgrammaticScrollTopRef.current = node.scrollTop;
        lastNativeScrollTopRef.current = node.scrollTop;
        shouldKeepBottomAfterLayoutRef.current = true;
        isDetachedFromBottomRef.current = false;
        setHasUserScrollInteraction(false);
        lastUserScrollKindRef.current = null;
        lastUserScrollInputTimeRef.current = Number.NEGATIVE_INFINITY;
        pendingAggressiveIdleCompactionRef.current = true;
        pendingMountedPrependRestoreRef.current = null;
        skipNextMountedPrependRestoreRef.current = false;
        clearPendingDeferredLayoutTimer();
        clearPendingIdleCompactionTimer();
        pendingDeferredLayoutAnchorRef.current = null;
        if (explicitScrollKind === "bottom_boundary") {
          pendingBottomBoundarySeekRef.current = true;
          mountBottomBoundary(node);
          scheduleBottomBoundaryReveal(node);
        } else {
          pendingBottomBoundarySeekRef.current = false;
          applyMountedPageRange(buildBottomMountedRange(node.clientHeight));
          scheduleProgrammaticViewportSync(node);
        }
        return;
      }

      if (explicitScrollKind === "bottom_follow") {
        pendingPrependedTopBoundaryRef.current = false;
        pendingPrependedBottomGapRef.current = null;
        pendingProgrammaticBottomFollowUntilRef.current =
          performance.now() + MESSAGE_STACK_BOTTOM_FOLLOW_SCROLL_MS;
        enterBottomFollowMode();
        pendingAggressiveIdleCompactionRef.current = true;
        pendingMountedPrependRestoreRef.current = null;
        skipNextMountedPrependRestoreRef.current = false;
        clearPendingDeferredLayoutTimer();
        clearPendingIdleCompactionTimer();
        pendingDeferredLayoutAnchorRef.current = null;
        applyMountedPageRange(buildBottomMountedRange(node.clientHeight));
        scheduleProgrammaticViewportSync(node);
        return;
      }

      const previousScrollTop = lastNativeScrollTopRef.current;
      const scrollDelta = node.scrollTop - previousScrollTop;
      lastNativeScrollTopRef.current = node.scrollTop;
      pendingProgrammaticScrollTopRef.current = node.scrollTop;
      pendingPrependedTopBoundaryRef.current =
        explicitScrollKind === "seek" && node.scrollTop <= 1;
      const isNearBottomAfterWrite = isScrollContainerNearBottom(node);
      if (pendingPrependedTopBoundaryRef.current || isNearBottomAfterWrite) {
        pendingPrependedBottomGapRef.current = null;
      }
      if (!isNearBottomAfterWrite) {
        suspendDeferredRenderActivation(node);
      }
      if (shouldKeepBottomAfterLayoutRef.current && !isNearBottomAfterWrite) {
        shouldKeepBottomAfterLayoutRef.current = false;
      }
      if (isNearBottomAfterWrite) {
        shouldKeepBottomAfterLayoutRef.current = true;
        isDetachedFromBottomRef.current = false;
        setHasUserScrollInteraction(false);
        lastUserScrollKindRef.current = null;
        lastUserScrollInputTimeRef.current = Number.NEGATIVE_INFINITY;
        clearPendingIdleCompactionTimer();
      }
      clearPendingDeferredLayoutTimer();
      pendingDeferredLayoutAnchorRef.current = null;
      if (Math.abs(scrollDelta) >= 0.5) {
        if (explicitScrollKind === "seek" && isNearBottomAfterWrite) {
          shouldKeepBottomAfterLayoutRef.current = true;
          isDetachedFromBottomRef.current = false;
        }
        const resolvedScrollKind =
          explicitScrollKind ??
          (isNearBottomAfterWrite
            ? "seek"
            : classifyScrollKind(
                scrollDelta,
                node.clientHeight,
              ));
        const scrollWriteTime = performance.now();
        lastUserScrollKindRef.current = isNearBottomAfterWrite
          ? null
          : resolvedScrollKind;
        if (!isNearBottomAfterWrite) {
          if (explicitScrollSource === "user") {
            releaseConversationSearchPinForUserScroll();
          }
          lastUserScrollInputTimeRef.current = scrollWriteTime;
          scheduleIdleMountedRangeCompaction(VIRTUALIZED_USER_SCROLL_ADJUSTMENT_COOLDOWN_MS);
        }
        const isActiveUpwardUserScrollWrite =
          explicitScrollSource === "user" &&
          scrollDelta < 0 &&
          !isNearBottomAfterWrite;
        reconcileMountedRangeForNativeScroll(
          node,
          scrollDelta,
          resolvedScrollKind,
          {
            allowSeekFlush: explicitScrollSource === "user",
            flush: isActiveUpwardUserScrollWrite,
          },
        );
      }
      scheduleProgrammaticViewportSync(node);
    };

    // Scrollbar-thumb mousedown does not produce wheel/touch/keydown events,
    // so a downward scrollbar drag during a `bottom_follow` cooldown otherwise
    // satisfies the `isProgrammaticBottomFollowScroll` discriminator (forward
    // progress + cooldown alive + `lastUserScrollInputTimeRef ===
    // NEGATIVE_INFINITY`) and gets re-classified as continuation of the smooth
    // animation — re-extending the cooldown and fighting the user. Cancelling
    // the cooldown unconditionally on mousedown is correct: a click on message
    // content costs nothing (no native scroll fires), and a click on the
    // scrollbar correctly hands control back to the user.
    const cancelBottomFollowOnMouseDown = (event: MouseEvent) => {
      pendingProgrammaticBottomFollowUntilRef.current = Number.NEGATIVE_INFINITY;
      if (event.target === node) {
        shouldKeepBottomAfterLayoutRef.current = false;
        isDetachedFromBottomRef.current = true;
        setHasUserScrollInteraction(true);
        lastUserScrollInputTimeRef.current = performance.now();
      }
    };
    const recordTouchStart = (event: TouchEvent) => {
      lastTouchClientYRef.current = event.touches[0]?.clientY ?? null;
    };
    const recordTouchEnd = (event: TouchEvent) => {
      lastTouchClientYRef.current = event.touches[0]?.clientY ?? null;
    };

    syncViewport();
    lastNativeScrollTopRef.current = node.scrollTop;
    const onNativeScroll = () => {
      syncViewport({ isNativeScrollEvent: true });
    };
    node.addEventListener("scroll", onNativeScroll, { passive: true });
    node.addEventListener(MESSAGE_STACK_SCROLL_WRITE_EVENT, syncProgrammaticScrollWrite);
    node.addEventListener("wheel", markUserScroll, { passive: true });
    node.addEventListener("touchstart", recordTouchStart, { passive: true });
    node.addEventListener("touchmove", markUserScroll, { passive: true });
    node.addEventListener("touchend", recordTouchEnd, { passive: true });
    node.addEventListener("touchcancel", recordTouchEnd, { passive: true });
    node.addEventListener("keydown", markUserScroll);
    node.addEventListener("mousedown", cancelBottomFollowOnMouseDown);
    const resizeObserver = new ResizeObserver(() => {
      syncViewport();
    });
    resizeObserver.observe(node);

    return () => {
      node.removeEventListener("scroll", onNativeScroll);
      node.removeEventListener(MESSAGE_STACK_SCROLL_WRITE_EVENT, syncProgrammaticScrollWrite);
      node.removeEventListener("wheel", markUserScroll);
      node.removeEventListener("touchstart", recordTouchStart);
      node.removeEventListener("touchmove", markUserScroll);
      node.removeEventListener("touchend", recordTouchEnd);
      node.removeEventListener("touchcancel", recordTouchEnd);
      node.removeEventListener("keydown", markUserScroll);
      node.removeEventListener("mousedown", cancelBottomFollowOnMouseDown);
      resizeObserver.disconnect();
    };
  }, [
    applyMountedPageRange,
    buildBottomMountedRange,
    cancelPostActivationBottomRestore,
    captureLatestVisibleMessageAnchor,
    clearPendingDeferredLayoutTimer,
    clearPendingIdleCompactionTimer,
    isActive,
    isMeasuringPostActivation,
    mountBottomBoundary,
    prewarmMountedRangeForUpwardWheel,
    releaseConversationSearchPinForUserScroll,
    reconcileMountedRangeForNativeScroll,
    scheduleBottomBoundaryReveal,
    scheduleProgrammaticViewportSync,
    scheduleIdleMountedRangeCompaction,
    setHasUserScrollInteraction,
    suspendDeferredRenderActivation,
    scrollContainerRef,
    sessionId,
    syncViewportFromScrollNode,
    tailFollowIntent,
  ]);

  useLayoutEffect(() => {
    if (!isActive) {
      return undefined;
    }

    const node = scrollContainerRef.current;
    if (!node) {
      return undefined;
    }

    syncViewportFromScrollNode(node);

    let frameId = 0;
    let remainingFrames = ACTIVE_VIEWPORT_STARTUP_RESYNC_FRAMES;
    const tick = () => {
      frameId = 0;
      const currentNode = scrollContainerRef.current;
      if (!currentNode) {
        return;
      }

      syncViewportFromScrollNode(currentNode);
      remainingFrames -= 1;
      if (remainingFrames > 0) {
        frameId = window.requestAnimationFrame(tick);
      }
    };

    frameId = window.requestAnimationFrame(tick);
    return () => {
      if (frameId !== 0) {
        window.cancelAnimationFrame(frameId);
      }
    };
  }, [isActive, scrollContainerRef, sessionId, syncViewportFromScrollNode]);

  const handlePageHeightChange = useCallback(
    (pageKey: string, nextHeight: number) => {
      if (!Number.isFinite(nextHeight) || nextHeight <= 0) {
        return;
      }

      const roundedHeight = Math.round(nextHeight);
      const previousHeight = pageHeightsRef.current[pageKey];
      if (previousHeight !== undefined && Math.abs(previousHeight - roundedHeight) < 1) {
        return;
      }

      pageHeightsRef.current[pageKey] = roundedHeight;

      const node = scrollContainerRef.current;
      if (node && hasUserScrollInteractionRef.current && !isScrollContainerNearBottom(node)) {
        shouldKeepBottomAfterLayoutRef.current = false;
      }
      const shouldKeepBottom =
        isActive && node
          ? !isDetachedFromBottomRef.current &&
            (shouldKeepBottomAfterLayoutRef.current || isScrollContainerNearBottom(node))
          : false;
      if (shouldKeepBottom) {
        shouldKeepBottomAfterLayoutRef.current = true;
      }

      const timeSinceUserScroll = performance.now() - lastUserScrollInputTimeRef.current;
      const inUserScrollCooldown = timeSinceUserScroll < VIRTUALIZED_USER_SCROLL_ADJUSTMENT_COOLDOWN_MS;
      if (inUserScrollCooldown) {
        scheduleDeferredLayoutVersion(
          VIRTUALIZED_USER_SCROLL_ADJUSTMENT_COOLDOWN_MS - timeSinceUserScroll,
        );
        return;
      }

      scheduleDeferredLayoutVersion(0);
      if (node && shouldKeepBottom && !hasUserScrollInteractionRef.current) {
        window.requestAnimationFrame(() => {
          if (scrollContainerRef.current !== node) {
            return;
          }
          if (
            hasUserScrollInteractionRef.current ||
            isDetachedFromBottomRef.current ||
            !shouldKeepBottomAfterLayoutRef.current
          ) {
            return;
          }
          const target = Math.max(node.scrollHeight - node.clientHeight, 0);
          writeScrollTopAndSyncViewport(node, target);
        });
      }
    },
    [
      isActive,
      scheduleDeferredLayoutVersion,
      scrollContainerRef,
      writeScrollTopAndSyncViewport,
    ],
  );

  const boundApprovalDecision = useCallback(
    (messageId: string, decision: ApprovalDecision) =>
      onApprovalDecision(sessionId, messageId, decision),
    [onApprovalDecision, sessionId],
  );
  const boundUserInputSubmit = useCallback(
    (messageId: string, answers: Record<string, string[]>) =>
      onUserInputSubmit(sessionId, messageId, answers),
    [onUserInputSubmit, sessionId],
  );
  const boundMcpElicitationSubmit = useCallback(
    (messageId: string, action: McpElicitationAction, content?: JsonValue) =>
      onMcpElicitationSubmit(sessionId, messageId, action, content),
    [onMcpElicitationSubmit, sessionId],
  );
  const boundCodexAppRequestSubmit = useCallback(
    (messageId: string, result: JsonValue) =>
      onCodexAppRequestSubmit(sessionId, messageId, result),
    [onCodexAppRequestSubmit, sessionId],
  );
  if (!isActive) {
    return <div className="virtualized-message-list" style={{ height: pageLayout.totalHeight }} />;
  }

  return (
    <div
      ref={renderedListRef}
      className={`virtualized-message-list${isMeasuringPostActivation ? " is-measuring-post-activation" : ""}${isBottomBoundaryRevealPending ? " is-bottom-boundary-revealing" : ""}`}
    >
      {topSpacerHeight > 0 ? (
        <div className="virtualized-message-spacer" style={{ height: topSpacerHeight }} />
      ) : null}
      {mountedPages.map((page) => (
        <MeasuredPageBand
          key={page.key}
          isActive={isActive}
          page={page}
          preferImmediateHeavyRender={preferImmediateHeavyRender}
          allowDeferredHeavyActivation={allowDeferredHeavyActivation}
          renderMessageCard={renderMessageCard}
          conversationSearchMatchedItemKeys={conversationSearchMatchedItemKeys}
          conversationSearchActiveItemKey={conversationSearchActiveItemKey}
          onSearchItemMount={onConversationSearchItemMount}
          onApprovalDecision={boundApprovalDecision}
          onUserInputSubmit={boundUserInputSubmit}
          onMcpElicitationSubmit={boundMcpElicitationSubmit}
          onCodexAppRequestSubmit={boundCodexAppRequestSubmit}
          onHeightChange={handlePageHeightChange}
        />
      ))}
      {bottomSpacerHeight > 0 ? (
        <div className="virtualized-message-spacer" style={{ height: bottomSpacerHeight }} />
      ) : null}
    </div>
  );
}

const MeasuredPageBand = memo(function MeasuredPageBand({
  isActive,
  page,
  preferImmediateHeavyRender,
  allowDeferredHeavyActivation,
  renderMessageCard,
  conversationSearchMatchedItemKeys,
  conversationSearchActiveItemKey,
  onSearchItemMount,
  onApprovalDecision,
  onUserInputSubmit,
  onMcpElicitationSubmit,
  onCodexAppRequestSubmit,
  onHeightChange,
}: {
  isActive: boolean;
  page: MessagePage;
  preferImmediateHeavyRender: boolean;
  allowDeferredHeavyActivation: boolean;
  renderMessageCard: RenderMessageCard;
  conversationSearchMatchedItemKeys: ReadonlySet<string>;
  conversationSearchActiveItemKey?: string | null;
  onSearchItemMount: (itemKey: string, node: HTMLElement | null) => void;
  onApprovalDecision: (messageId: string, decision: ApprovalDecision) => void;
  onUserInputSubmit: BoundUserInputSubmitHandler;
  onMcpElicitationSubmit: BoundMcpElicitationSubmitHandler;
  onCodexAppRequestSubmit: BoundCodexAppRequestSubmitHandler;
  onHeightChange: (pageKey: string, nextHeight: number) => void;
}) {
  const pageRef = useRef<HTMLDivElement | null>(null);

  useLayoutEffect(() => {
    if (!isActive) {
      return;
    }

    const node = pageRef.current;
    if (!node) {
      return;
    }

    let frameId = 0;
    const measure = () => {
      frameId = 0;
      const slotNodes = Array.from(
        node.querySelectorAll<HTMLElement>(".virtualized-message-slot"),
      );
      let totalHeight = 0;
      let measuredSlotCount = 0;
      slotNodes.forEach((slotNode, index) => {
        const slotHeight = Math.max(slotNode.getBoundingClientRect().height, 0);
        totalHeight += slotHeight;
        if (slotHeight > 0) {
          measuredSlotCount += 1;
        }
        if (index < slotNodes.length - 1) {
          totalHeight += VIRTUALIZED_MESSAGE_GAP_PX;
        }
      });
      if (page.hasTrailingGap) {
        totalHeight += VIRTUALIZED_MESSAGE_GAP_PX;
      }
      // Detached / not-yet-laid-out test environments can report zero-height
      // slots while still giving the page its fixed gap total. Treat that as
      // "not measured yet" rather than replacing realistic estimates with a
      // tiny gap-only page height that collapses the whole virtual layout.
      if (measuredSlotCount === 0) {
        return;
      }
      onHeightChange(page.key, totalHeight);
    };

    measure();
    const resizeObserver = new ResizeObserver(() => {
      if (frameId !== 0) {
        return;
      }
      frameId = window.requestAnimationFrame(measure);
    });
    resizeObserver.observe(node);
    Array.from(node.querySelectorAll(".virtualized-message-slot")).forEach((slotNode) => {
      resizeObserver.observe(slotNode);
    });

    return () => {
      resizeObserver.disconnect();
      if (frameId !== 0) {
        window.cancelAnimationFrame(frameId);
      }
    };
  }, [isActive, onHeightChange, page.hasTrailingGap, page.key]);

  return (
    <div ref={pageRef} className="virtualized-message-page" data-page-key={page.key}>
      <div className="virtualized-message-range">
        {page.messages.map((message) => (
          <div
            key={message.id}
            className="virtualized-message-slot"
            data-message-id={message.id}
          >
            <MessageSlot
              itemKey={isActive ? `message:${message.id}` : undefined}
              isSearchMatch={conversationSearchMatchedItemKeys.has(`message:${message.id}`)}
              isSearchActive={conversationSearchActiveItemKey === `message:${message.id}`}
              onSearchItemMount={onSearchItemMount}
            >
              <DeferredHeavyContentActivationProvider
                allowActivation={allowDeferredHeavyActivation}
              >
                {renderMessageCard(
                  message,
                  preferImmediateHeavyRender,
                  onApprovalDecision,
                  onUserInputSubmit,
                  onMcpElicitationSubmit,
                  onCodexAppRequestSubmit,
                )}
              </DeferredHeavyContentActivationProvider>
            </MessageSlot>
          </div>
        ))}
      </div>
      {page.hasTrailingGap ? (
        <div
          className="virtualized-message-page-gap"
          style={{ height: VIRTUALIZED_MESSAGE_GAP_PX }}
        />
      ) : null}
    </div>
  );
});
