// Virtualized conversation rendering for an agent session.
//
// The model here is intentionally simple:
// - mounted page bands are real DOM and own the live scroll experience
// - only unseen pages above/below the mounted band are virtual space
// - page measurements may refine unseen spacers, but anchor preservation keeps
//   the currently visible DOM band stable while that virtual space catches up

import {
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
  DEFERRED_RENDER_RESUME_EVENT,
  DEFERRED_RENDER_SUSPENDED_ATTRIBUTE,
} from "../deferred-render";
import {
  DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
  clampVirtualizedViewportScrollTop,
  findVirtualizedMessageRange,
  isScrollContainerNearBottom,
  resolveEstimatedConversationMessageHeight,
  type EstimatedMessageHeightEntry,
} from "./conversation-virtualization";
import {
  VIRTUALIZED_MESSAGES_PER_PAGE,
  buildMessagePages,
  buildPageEstimateCacheKey,
  buildPageLayout,
  captureFirstVisibleMountedMessageAnchor,
  estimateMessageOffsetWithinPage,
  estimatePageHeight,
  findMountedMessageSlotById,
  getMountedSlotViewportOffsetPx,
  resolvePageCoverageHeight,
  resolveRenderedPageCoverageHeight,
  type EstimatedPageHeightEntry,
  type MessageLocation,
  type PendingVisibleMessageAnchor,
  type VirtualizedRange,
  type VisibleMessageAnchor,
} from "./virtualized-conversation-measurement";
import { useVirtualizedConversationHandle } from "./virtualized-conversation-handle";
import { useVirtualizedConversationMountedRangeEffects } from "./virtualized-conversation-mounted-range";
import { useVirtualizedConversationPageHeightChange } from "./virtualized-conversation-page-heights";
import { MeasuredPageBand } from "./virtualized-conversation-rendering";
import { useVirtualizedConversationScrollEvents } from "./virtualized-conversation-scroll-events";
import type {
  BoundCodexAppRequestSubmitHandler,
  BoundMcpElicitationSubmitHandler,
  BoundUserInputSubmitHandler,
  CodexAppRequestSubmitHandler,
  McpElicitationSubmitHandler,
  MessageWindowSnapshot,
  RenderMessageCard,
  UserInputSubmitHandler,
  UserScrollKind,
  VirtualizedConversationJumpOptions,
  VirtualizedConversationLayoutMessage,
  VirtualizedConversationLayoutSnapshot,
  VirtualizedConversationMessageListHandleRef,
  VirtualizedConversationViewportSnapshot,
} from "./virtualized-conversation-types";
import type {
  ApprovalDecision,
  JsonValue,
  McpElicitationAction,
  Message,
} from "../types";

export type {
  BoundCodexAppRequestSubmitHandler,
  BoundMcpElicitationSubmitHandler,
  BoundUserInputSubmitHandler,
  CodexAppRequestSubmitHandler,
  McpElicitationSubmitHandler,
  RenderMessageCard,
  UserInputSubmitHandler,
  VirtualizedConversationJumpOptions,
  VirtualizedConversationLayoutMessage,
  VirtualizedConversationLayoutSnapshot,
  VirtualizedConversationMessageListHandle,
  VirtualizedConversationMessageListHandleRef,
  VirtualizedConversationViewportSnapshot,
} from "./virtualized-conversation-types";
export type { VirtualizedRange } from "./virtualized-conversation-measurement";
export {
  resolveBottomReentryScrollKind,
  resolveNativeScrollKind,
} from "./virtualized-conversation-scroll-events";

const ACTIVE_MOUNTED_RESERVE_ABOVE_VIEWPORTS = 3;
const ACTIVE_MOUNTED_RESERVE_BELOW_VIEWPORTS = 3;
const BOUNDARY_SEEK_MOUNTED_RESERVE_ABOVE_VIEWPORTS = 1;
const BOUNDARY_SEEK_MOUNTED_RESERVE_BELOW_VIEWPORTS = 0;
const ACTIVE_MOUNTED_EXTRA_PAGES_BELOW = 2;
const ACTIVE_VIEWPORT_STARTUP_RESYNC_FRAMES = 12;
const BOTTOM_BOUNDARY_REVEAL_SETTLE_FRAMES = 12;
const BOTTOM_BOUNDARY_REVEAL_DELAY_MS = 220;
const POST_ACTIVATION_ESTIMATED_BOTTOM_MIN_PAGES = 20;
const ACTIVE_SCROLL_MOUNTED_RANGE_COLLAPSE_EXTRA_PAGES = 12;
const ACTIVE_SCROLL_MOUNTED_RANGE_COLLAPSE_MULTIPLIER = 2;
export const VIRTUALIZED_USER_SCROLL_ADJUSTMENT_COOLDOWN_MS = 200;
const PREPENDED_MESSAGE_ANCHOR_RESTORE_ATTEMPTS = 3;
// Separate, much shorter cooldown for the deferred-heavy-content (Markdown,
// tool blocks) activation gate. Heavy content paint should resume almost
// immediately after the user stops scrolling, while the broader scroll-state
// machine keeps its 200ms quiet window for range / mount adjustments.
const DEFERRED_HEAVY_ACTIVATION_COOLDOWN_MS = 10;

const EMPTY_MATCHED_ITEM_KEYS = new Set<string>();

function rangesEqual(first: VirtualizedRange, second: VirtualizedRange) {
  return first.startIndex === second.startIndex && first.endIndex === second.endIndex;
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
  const estimatedMessageHeightsRef = useRef<WeakMap<Message, EstimatedMessageHeightEntry>>(
    new WeakMap(),
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
  const visiblePageRangeRef = useRef<VirtualizedRange>({
    startIndex: 0,
    endIndex: 0,
  });

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

  useLayoutEffect(() => {
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
    (message: Message) => {
      const expandedPromptOpen =
        message.type === "text" &&
        message.author === "you" &&
        Boolean(message.expandedText) &&
        isExpandedPromptOpen(message.id);
      return resolveEstimatedConversationMessageHeight(
        estimatedMessageHeightsRef.current,
        message,
        {
          expandedPromptOpen,
          viewportWidth,
        },
      );
    },
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
  visiblePageRangeRef.current = visiblePageRange;

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
              getMountedSlotViewportOffsetPx(node, preservedAnchorSlot) -
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
            remainingAttempts: PREPENDED_MESSAGE_ANCHOR_RESTORE_ATTEMPTS,
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
      if (pendingAnchor.remainingAttempts > 1) {
        pendingPrependedMessageAnchorRef.current = {
          ...pendingAnchor,
          remainingAttempts: pendingAnchor.remainingAttempts - 1,
        };
      } else {
        pendingPrependedMessageAnchorRef.current = null;
        latestVisibleMessageAnchorRef.current = null;
      }
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
  ]);

  useVirtualizedConversationHandle({
    applyMountedPageRange,
    buildWorkingMountedRangeForScrollTop,
    clearPendingDeferredLayoutTimer,
    clearPendingIdleCompactionTimer,
    estimateMessageHeight,
    isActive,
    isDetachedFromBottomRef,
    lastUserScrollInputTimeRef,
    lastUserScrollKindRef,
    messageLocationById,
    messages,
    mountedPageRangeRef,
    pageHeightsRef,
    pageLayout,
    pages,
    pendingAggressiveIdleCompactionRef,
    pendingDeferredLayoutAnchorRef,
    pendingMountedPrependRestoreRef,
    pendingProgrammaticBottomFollowUntilRef,
    pendingProgrammaticScrollTopRef,
    renderedListRef,
    renderedMountedPageRange,
    scrollContainerRef,
    sessionId,
    setHasUserScrollInteraction,
    shouldKeepBottomAfterLayoutRef,
    skipNextMountedPrependRestoreRef,
    viewportHeight,
    viewportScrollTop,
    viewportWidth,
    virtualizerHandleRef,
    visiblePageRange,
    writeScrollTopAndSyncViewport,
  });

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

  useVirtualizedConversationMountedRangeEffects({
    applyMountedPageRange,
    captureMountedPrependRestore,
    hasUserScrollInteractionRef,
    isActive,
    lastUserScrollInputTimeRef,
    lastUserScrollKindRef,
    layoutVersion,
    mountedPageRange,
    pageHeights,
    pagesLength: pages.length,
    pendingAggressiveIdleCompactionRef,
    pendingMountedPrependRestoreRef,
    pendingPrependedMessageAnchorRef,
    pendingProgrammaticBottomFollowUntilRef,
    renderedListRef,
    scrollContainerRef,
    scrollIdleVersion,
    searchPinnedMountedPageRange,
    userScrollAdjustmentCooldownMs: VIRTUALIZED_USER_SCROLL_ADJUSTMENT_COOLDOWN_MS,
    viewportScrollTop,
    visiblePageRange,
    workingMountedPageRange,
  });

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
            getMountedSlotViewportOffsetPx(node, anchorSlot) -
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
        getMountedSlotViewportOffsetPx(node, anchorSlot) -
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

  useVirtualizedConversationScrollEvents({
    applyMountedPageRange,
    buildBottomMountedRange,
    cancelPostActivationBottomRestore,
    captureLatestVisibleMessageAnchor,
    clearPendingDeferredLayoutTimer,
    clearPendingIdleCompactionTimer,
    hasUserScrollInteractionRef,
    isActive,
    isDetachedFromBottomRef,
    isMeasuringPostActivation,
    lastNativeScrollTopRef,
    lastTouchClientYRef,
    lastUserScrollInputTimeRef,
    lastUserScrollKindRef,
    latestVisibleMessageAnchorRef,
    mountBottomBoundary,
    pendingAggressiveIdleCompactionRef,
    pendingBottomBoundaryRevealNodeRef,
    pendingBottomBoundarySeekRef,
    pendingDeferredLayoutAnchorRef,
    pendingMountedPrependRestoreRef,
    pendingPrependedBottomGapRef,
    pendingPrependedTopBoundaryRef,
    pendingProgrammaticBottomFollowUntilRef,
    pendingProgrammaticScrollTopRef,
    prewarmMountedRangeForUpwardWheel,
    reconcileMountedRangeForNativeScroll,
    releaseConversationSearchPinForUserScroll,
    renderedListRef,
    scheduleBottomBoundaryReveal,
    scheduleIdleMountedRangeCompaction,
    scheduleProgrammaticViewportSync,
    scrollContainerRef,
    setHasUserScrollInteraction,
    shouldKeepBottomAfterLayoutRef,
    skipNextMountedPrependRestoreRef,
    suspendDeferredRenderActivation,
    syncViewportFromScrollNode,
    tailFollowIntent,
    userScrollAdjustmentCooldownMs: VIRTUALIZED_USER_SCROLL_ADJUSTMENT_COOLDOWN_MS,
  });

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

  const handlePageHeightChange = useVirtualizedConversationPageHeightChange({
    bumpLayoutVersion,
    clearPendingDeferredLayoutTimer,
    hasUserScrollInteractionRef,
    isActive,
    isDetachedFromBottomRef,
    lastUserScrollInputTimeRef,
    pageHeightsRef,
    scheduleDeferredLayoutVersion,
    scrollContainerRef,
    shouldKeepBottomAfterLayoutRef,
    userScrollAdjustmentCooldownMs: VIRTUALIZED_USER_SCROLL_ADJUSTMENT_COOLDOWN_MS,
    visiblePageRangeRef,
    writeScrollTopAndSyncViewport,
  });

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
