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
  MESSAGE_STACK_SCROLL_WRITE_EVENT,
} from "../message-stack-scroll-sync";
import { MessageSlot } from "./session-message-leaves";
import {
  DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
  VIRTUALIZED_MESSAGE_GAP_PX,
  estimateConversationMessageHeight,
  findVirtualizedMessageRange,
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

const VIRTUALIZED_MESSAGES_PER_PAGE = 8;
const ACTIVE_MOUNTED_RESERVE_ABOVE_VIEWPORTS = 3;
const ACTIVE_MOUNTED_RESERVE_BELOW_VIEWPORTS = 3;
const ACTIVE_MOUNTED_EXTRA_PAGES_BELOW = 1;
const ACTIVE_VIEWPORT_STARTUP_RESYNC_FRAMES = 12;
const USER_SCROLL_ADJUSTMENT_COOLDOWN_MS = 200;

export const EMPTY_MATCHED_ITEM_KEYS = new Set<string>();

type VirtualizedRange = { startIndex: number; endIndex: number };
type UserScrollKind = "incremental" | "seek" | null;
type MessageLocation = {
  message: Message;
  messageIndex: number;
  pageIndex: number;
  pageLocalIndex: number;
};
type MessagePage = {
  key: string;
  pageIndex: number;
  startIndex: number;
  endIndex: number;
  hasTrailingGap: boolean;
  messages: Message[];
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

function rangeContainsRange(container: VirtualizedRange, target: VirtualizedRange) {
  return (
    container.startIndex <= target.startIndex &&
    container.endIndex >= target.endIndex
  );
}

function getMountedMessageSlots(scrollContainerNode: HTMLElement) {
  return Array.from(
    scrollContainerNode.querySelectorAll<HTMLElement>(".virtualized-message-slot[data-message-id]"),
  );
}

function findMountedMessageSlotById(scrollContainerNode: HTMLElement, messageId: string) {
  return (
    getMountedMessageSlots(scrollContainerNode).find(
      (slot) => slot.dataset.messageId === messageId,
    ) ?? null
  );
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

export function VirtualizedConversationMessageList({
  isActive,
  renderMessageCard,
  sessionId,
  messages,
  scrollContainerRef,
  conversationSearchQuery = "",
  conversationSearchMatchedItemKeys = EMPTY_MATCHED_ITEM_KEYS,
  conversationSearchActiveItemKey = null,
  onConversationSearchItemMount = () => {},
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
  conversationSearchQuery?: string;
  conversationSearchMatchedItemKeys?: ReadonlySet<string>;
  conversationSearchActiveItemKey?: string | null;
  onConversationSearchItemMount?: (itemKey: string, node: HTMLElement | null) => void;
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

  const pageHeightsRef = useRef<Record<string, number>>({});
  const shouldKeepBottomAfterLayoutRef = useRef(false);
  const isDetachedFromBottomRef = useRef(false);
  const skipNextMountedPrependRestoreRef = useRef(false);
  const lastPinnedConversationSearchIdRef = useRef<string | null>(null);
  const lastUserScrollInputTimeRef = useRef(Number.NEGATIVE_INFINITY);
  const lastUserScrollKindRef = useRef<UserScrollKind>(null);
  const lastNativeScrollTopRef = useRef(0);
  const pendingProgrammaticScrollTopRef = useRef<number | null>(null);
  const pendingMountedPrependRestoreRef = useRef<{
    scrollHeight: number;
    scrollTop: number;
  } | null>(null);
  const pendingDeferredLayoutTimerRef = useRef<number | null>(null);
  const pendingIdleCompactionTimerRef = useRef<number | null>(null);
  const pendingProgrammaticViewportSyncRef = useRef(false);

  const [viewport, setViewport] = useState({
    height: DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
    scrollTop: 0,
    width: 0,
  });
  const [layoutVersion, setLayoutVersion] = useState(0);
  const [scrollIdleVersion, setScrollIdleVersion] = useState(0);
  const [isMeasuringPostActivation, setIsMeasuringPostActivation] = useState(
    () => isActive && messages.length > 0,
  );
  const [prevIsActive, setPrevIsActive] = useState(isActive);

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

      const armTimer = (remainingDelayMs: number) => {
        pendingDeferredLayoutTimerRef.current = window.setTimeout(() => {
          pendingDeferredLayoutTimerRef.current = null;
          const timeSinceUserScroll =
            performance.now() - lastUserScrollInputTimeRef.current;
          const remainingCooldownMs =
            USER_SCROLL_ADJUSTMENT_COOLDOWN_MS - timeSinceUserScroll;
          if (remainingCooldownMs > 0) {
            armTimer(remainingCooldownMs);
            return;
          }
          bumpLayoutVersion();
        }, Math.max(Math.ceil(remainingDelayMs), 0));
      };

      armTimer(delayMs);
    },
    [bumpLayoutVersion, clearPendingDeferredLayoutTimer, scrollContainerRef],
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
    shouldKeepBottomAfterLayoutRef.current = false;
    setIsMeasuringPostActivation(false);
  }, []);

  if (prevIsActive !== isActive) {
    setPrevIsActive(isActive);
    if (!prevIsActive && isActive && messages.length > 0) {
      setIsMeasuringPostActivation(true);
    }
  }

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
      pages.map(
        (page) => pageHeightsRef.current[page.key] ?? estimatePageHeight(page, estimateMessageHeight),
      ),
    [estimateMessageHeight, layoutVersion, pages],
  );
  const pageLayout = useMemo(() => buildPageLayout(pageHeights), [pageHeights]);
  const effectiveTotalHeight =
    activeViewport !== null
      ? Math.max(pageLayout.totalHeight, activeViewport.scrollHeight)
      : pageLayout.totalHeight;
  const rawViewportScrollTop = activeViewport ? activeViewport.scrollTop : viewport.scrollTop;
  const viewportScrollTop =
    Number.isFinite(rawViewportScrollTop) && rawViewportScrollTop > 0
      ? Math.min(rawViewportScrollTop, Math.max(effectiveTotalHeight - viewportHeight, 0))
      : 0;

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

  const activeMountedBufferAbovePx = Math.max(
    viewportHeight * ACTIVE_MOUNTED_RESERVE_ABOVE_VIEWPORTS,
    DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
  );
  const activeMountedBufferBelowPx = Math.max(
    viewportHeight * ACTIVE_MOUNTED_RESERVE_BELOW_VIEWPORTS,
    DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT,
  );
  // Active scroll keeps a working set around the viewport instead of waiting
  // until the reader is close to a band edge. With 1 viewport above + 1
  // below, the mounted DOM stays at roughly 3x the visible area.
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

    // Keep one whole page mounted below the computed working range. The
    // bottom edge is where minor page-height drift shows up most clearly as a
    // deterministic "messages disappear at this exact offset" gap. Holding one
    // extra page below turns that hard boundary into hysteresis instead of a
    // visible blank slab.
    return {
      startIndex: baseRange.startIndex,
      endIndex: Math.min(baseRange.endIndex + ACTIVE_MOUNTED_EXTRA_PAGES_BELOW, pages.length),
    };
  }, [
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

  const mountedPages = useMemo(
    () => pages.slice(mountedPageRange.startIndex, mountedPageRange.endIndex),
    [mountedPageRange.endIndex, mountedPageRange.startIndex, pages],
  );
  const topSpacerHeight =
    mountedPageRange.startIndex > 0 ? (pageLayout.tops[mountedPageRange.startIndex] ?? 0) : 0;
  const mountedPageEndOffset =
    mountedPageRange.endIndex <= mountedPageRange.startIndex
      ? topSpacerHeight
      : (pageLayout.tops[mountedPageRange.endIndex - 1] ?? topSpacerHeight) +
        (pageHeights[mountedPageRange.endIndex - 1] ?? 0);
  const bottomSpacerHeight = Math.max(pageLayout.totalHeight - mountedPageEndOffset, 0);

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

  const reconcileMountedRangeForNativeScroll = useCallback(
    (node: HTMLElement, scrollDelta: number, scrollKind: UserScrollKind) => {
      const nextWorkingRange = buildWorkingMountedRangeForScrollTop(
        node.scrollTop,
        node.clientHeight,
      );

      const currentRange = mountedPageRangeRef.current;
      let nextRange: VirtualizedRange = currentRange;

      if (scrollKind === "seek") {
        nextRange = nextWorkingRange;
      } else if (scrollDelta > 0) {
        const pageNodes = Array.from(
          node.querySelectorAll<HTMLElement>(".virtualized-message-page[data-page-key]"),
        );
        let nextEndIndex = Math.max(currentRange.endIndex, nextWorkingRange.endIndex);
        if (pageNodes.length > 0) {
          const containerRect = node.getBoundingClientRect();
          const lastPageRect = pageNodes[pageNodes.length - 1]!.getBoundingClientRect();
          const renderedMountedBottom =
            node.scrollTop + (lastPageRect.bottom - containerRect.top);
          const desiredMountedBottom =
            node.scrollTop + node.clientHeight + activeMountedBufferBelowPxRef.current;
          let missingBelowPx = desiredMountedBottom - renderedMountedBottom;
          while (missingBelowPx > 0 && nextEndIndex < pagesLengthRef.current) {
            missingBelowPx -= pageHeightsRefForScroll.current[nextEndIndex] ?? 0;
            nextEndIndex += 1;
          }
        }
        nextRange = {
          startIndex: Math.min(currentRange.startIndex, nextWorkingRange.startIndex),
          endIndex: nextEndIndex,
        };
      } else {
        nextRange = {
          startIndex: Math.min(currentRange.startIndex, nextWorkingRange.startIndex),
          endIndex: Math.max(currentRange.endIndex, nextWorkingRange.endIndex),
        };
      }

      if (rangesEqual(nextRange, currentRange)) {
        return;
      }

      if (
        scrollKind !== "seek" &&
        nextRange.startIndex < currentRange.startIndex &&
        !isScrollContainerNearBottom(node)
      ) {
        if (skipNextMountedPrependRestoreRef.current) {
          skipNextMountedPrependRestoreRef.current = false;
        } else {
          pendingMountedPrependRestoreRef.current = {
            scrollHeight: node.scrollHeight,
            scrollTop: node.scrollTop,
          };
        }
      } else if (scrollKind === "seek") {
        pendingMountedPrependRestoreRef.current = null;
        skipNextMountedPrependRestoreRef.current = false;
      }

      flushSync(() => {
        setMountedPageRange(nextRange);
      });
    },
    [buildWorkingMountedRangeForScrollTop],
  );
  useEffect(() => {
    return () => {
      clearPendingDeferredLayoutTimer();
      clearPendingIdleCompactionTimer();
      pendingProgrammaticViewportSyncRef.current = false;
    };
  }, [clearPendingDeferredLayoutTimer, clearPendingIdleCompactionTimer]);

  useEffect(() => {
    pageHeightsRef.current = Object.fromEntries(
      Object.entries(pageHeightsRef.current).filter(([pageKey]) => pageKeys.has(pageKey)),
    );
  }, [pageKeys]);

  useLayoutEffect(() => {
    if (!isActive) {
      return;
    }

    if (pages.length === 0) {
      return;
    }

    const inUserScrollCooldown =
      performance.now() - lastUserScrollInputTimeRef.current < USER_SCROLL_ADJUSTMENT_COOLDOWN_MS;
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
        pendingMountedPrependRestoreRef.current = {
          scrollHeight: node.scrollHeight,
          scrollTop: node.scrollTop,
        };
      }
      setMountedPageRange(nextRange);
    }
  }, [
    isActive,
    mountedPageRange,
    pages.length,
    scrollContainerRef,
    visiblePageRange,
    workingMountedPageRange,
  ]);

  useLayoutEffect(() => {
    if (rangesEqual(mountedPageRange, workingMountedPageRange)) {
      return;
    }

    const inUserScrollCooldown =
      performance.now() - lastUserScrollInputTimeRef.current < USER_SCROLL_ADJUSTMENT_COOLDOWN_MS;
    const viewportEscapedMountedBand = !rangeContainsRange(
      mountedPageRange,
      visiblePageRange,
    );
    if (inUserScrollCooldown) {
      // Active scroll is grow-only. Keep extra DOM on the opposite side until
      // the gesture settles; trimming during motion is what exposed spacer
      // blanks on the way down and snap-backs on the way up.
      if (lastUserScrollKindRef.current === "seek" && viewportEscapedMountedBand) {
        setMountedPageRange(workingMountedPageRange);
      }
      return;
    }

    setMountedPageRange(workingMountedPageRange);
  }, [
    mountedPageRange,
    scrollIdleVersion,
    visiblePageRange,
    workingMountedPageRange,
  ]);

  useLayoutEffect(() => {
    if (
      !isActive ||
      !hasConversationSearch ||
      !activeConversationSearchMessageId ||
      activeConversationSearchScrollTop === null
    ) {
      lastPinnedConversationSearchIdRef.current = null;
      return;
    }

    if (lastPinnedConversationSearchIdRef.current === activeConversationSearchMessageId) {
      return;
    }

    const node = scrollContainerRef.current;
    if (!node) {
      return;
    }

    const mountedTargetSlot = findMountedMessageSlotById(node, activeConversationSearchMessageId);
    const nextScrollTop = mountedTargetSlot
      ? Math.max(
          node.scrollTop +
            (mountedTargetSlot.getBoundingClientRect().top - node.getBoundingClientRect().top) -
            Math.max(
              (viewportHeight - mountedTargetSlot.getBoundingClientRect().height) / 2,
              0,
            ),
          0,
        )
      : activeConversationSearchScrollTop;

    if (Math.abs(node.scrollTop - nextScrollTop) >= 1) {
      shouldKeepBottomAfterLayoutRef.current = false;
    }
    writeScrollTopAndSyncViewport(node, nextScrollTop);
    lastPinnedConversationSearchIdRef.current = activeConversationSearchMessageId;
  }, [
    activeConversationSearchMessageId,
    activeConversationSearchScrollTop,
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

    const targetScrollTop =
      pendingRestore.scrollTop + (node.scrollHeight - pendingRestore.scrollHeight);
    writeScrollTopAndSyncViewport(node, targetScrollTop);
  }, [
    isActive,
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

    if (isDetachedFromBottomRef.current) {
      setIsMeasuringPostActivation(false);
      return;
    }

    const node = scrollContainerRef.current;
    if (node) {
      const target = Math.max(node.scrollHeight - node.clientHeight, 0);
      writeScrollTopAndSyncViewport(node, target);
    }
    setIsMeasuringPostActivation(false);
  }, [
    isActive,
    isMeasuringPostActivation,
    pages,
    scrollContainerRef,
    visiblePageRange.endIndex,
    visiblePageRange.startIndex,
    writeScrollTopAndSyncViewport,
  ]);

  useEffect(() => {
    if (!isMeasuringPostActivation) {
      return;
    }

    const timeoutId = window.setTimeout(() => {
      if (isDetachedFromBottomRef.current) {
        setIsMeasuringPostActivation(false);
        return;
      }
      const node = scrollContainerRef.current;
      if (node) {
        const target = Math.max(node.scrollHeight - node.clientHeight, 0);
        writeScrollTopAndSyncViewport(node, target);
      }
      shouldKeepBottomAfterLayoutRef.current = true;
      setIsMeasuringPostActivation(false);
    }, 150);

    return () => {
      window.clearTimeout(timeoutId);
    };
  }, [isMeasuringPostActivation, scrollContainerRef, writeScrollTopAndSyncViewport]);

  useLayoutEffect(() => {
    if (!isActive || !shouldKeepBottomAfterLayoutRef.current || isDetachedFromBottomRef.current) {
      return;
    }

    const timeSinceUserScroll = performance.now() - lastUserScrollInputTimeRef.current;
    if (timeSinceUserScroll < USER_SCROLL_ADJUSTMENT_COOLDOWN_MS) {
      return;
    }

    const node = scrollContainerRef.current;
    if (!node) {
      return;
    }

    const target = Math.max(node.scrollHeight - node.clientHeight, 0);
    writeScrollTopAndSyncViewport(node, target);
  }, [isActive, layoutVersion, pageLayout.totalHeight, scrollContainerRef, writeScrollTopAndSyncViewport]);

  useLayoutEffect(() => {
    if (!isActive) {
      return;
    }

    const node = scrollContainerRef.current;
    if (!node) {
      return;
    }

    const syncViewport = (options: { isNativeScrollEvent?: boolean } = {}) => {
      if (options.isNativeScrollEvent) {
        const pendingProgrammaticScrollTop = pendingProgrammaticScrollTopRef.current;
        const isProgrammaticScrollEvent =
          pendingProgrammaticScrollTop !== null &&
          Math.abs(node.scrollTop - pendingProgrammaticScrollTop) < 1;
        if (isProgrammaticScrollEvent) {
          pendingProgrammaticScrollTopRef.current = null;
        } else {
          if (isMeasuringPostActivation) {
            cancelPostActivationBottomRestore();
          }
          const scrollDelta = node.scrollTop - lastNativeScrollTopRef.current;
          lastNativeScrollTopRef.current = node.scrollTop;
          if (lastUserScrollKindRef.current === null) {
            lastUserScrollKindRef.current =
              Math.abs(scrollDelta) >= Math.max(node.clientHeight * 1.5, DEFAULT_VIRTUALIZED_VIEWPORT_HEIGHT)
                ? "seek"
                : "incremental";
          }
          lastUserScrollInputTimeRef.current = performance.now();
          scheduleIdleMountedRangeCompaction(USER_SCROLL_ADJUSTMENT_COOLDOWN_MS);
          reconcileMountedRangeForNativeScroll(
            node,
            scrollDelta,
            lastUserScrollKindRef.current,
          );
          if (
            isDetachedFromBottomRef.current &&
            scrollDelta >= 0 &&
            isScrollContainerNearBottom(node)
          ) {
            isDetachedFromBottomRef.current = false;
          }
        }
      }

      syncViewportFromScrollNode(node);

      if (shouldKeepBottomAfterLayoutRef.current && !isScrollContainerNearBottom(node)) {
        shouldKeepBottomAfterLayoutRef.current = false;
      }
    };

    const markUserScroll = (event?: WheelEvent | TouchEvent | KeyboardEvent) => {
      if (isMeasuringPostActivation) {
        cancelPostActivationBottomRestore();
      }
      const isSeekKeyboardNavigation =
        event instanceof KeyboardEvent &&
        (event.key === "PageUp" ||
          event.key === "PageDown" ||
          event.key === "Home" ||
          event.key === "End");
      const isExplicitUpwardScrollIntent =
        (event instanceof WheelEvent && event.deltaY < 0) ||
        (event instanceof KeyboardEvent &&
          (event.key === "PageUp" ||
            event.key === "ArrowUp" ||
            event.key === "Home"));
      if (isExplicitUpwardScrollIntent) {
        if (isScrollContainerNearBottom(node)) {
          // Preserve the browser's first upward escape from the bottom. The
          // mounted-band prepend that follows should expand DOM above without
          // replaying a scrollHeight-delta restore that can undo the page jump.
          skipNextMountedPrependRestoreRef.current = true;
        }
        // The first upward gesture from the bottom should always break the
        // "stick to latest" intent immediately. Waiting until the native
        // scroll lands more than 72 px away keeps the bottom-pin armed long
        // enough for a later layout tick to snap the viewport back down once.
        shouldKeepBottomAfterLayoutRef.current = false;
        isDetachedFromBottomRef.current = true;
      }
      lastUserScrollKindRef.current = isSeekKeyboardNavigation ? "seek" : "incremental";
      lastUserScrollInputTimeRef.current = performance.now();
      scheduleIdleMountedRangeCompaction(USER_SCROLL_ADJUSTMENT_COOLDOWN_MS);
    };
    const syncProgrammaticScrollWrite = () => {
      if (shouldKeepBottomAfterLayoutRef.current && !isScrollContainerNearBottom(node)) {
        shouldKeepBottomAfterLayoutRef.current = false;
      }
      if (isScrollContainerNearBottom(node)) {
        isDetachedFromBottomRef.current = false;
      }
      clearPendingDeferredLayoutTimer();
      clearPendingIdleCompactionTimer();
      lastUserScrollKindRef.current = null;
      scheduleProgrammaticViewportSync(node);
    };

    syncViewport();
    lastNativeScrollTopRef.current = node.scrollTop;
    const onNativeScroll = () => {
      syncViewport({ isNativeScrollEvent: true });
    };
    node.addEventListener("scroll", onNativeScroll, { passive: true });
    node.addEventListener(MESSAGE_STACK_SCROLL_WRITE_EVENT, syncProgrammaticScrollWrite);
    node.addEventListener("wheel", markUserScroll, { passive: true });
    node.addEventListener("touchmove", markUserScroll, { passive: true });
    node.addEventListener("keydown", markUserScroll);
    const resizeObserver = new ResizeObserver(() => {
      syncViewport();
    });
    resizeObserver.observe(node);

    return () => {
      node.removeEventListener("scroll", onNativeScroll);
      node.removeEventListener(MESSAGE_STACK_SCROLL_WRITE_EVENT, syncProgrammaticScrollWrite);
      node.removeEventListener("wheel", markUserScroll);
      node.removeEventListener("touchmove", markUserScroll);
      node.removeEventListener("keydown", markUserScroll);
      resizeObserver.disconnect();
    };
  }, [
    cancelPostActivationBottomRestore,
    clearPendingDeferredLayoutTimer,
    clearPendingIdleCompactionTimer,
    isActive,
    isMeasuringPostActivation,
    reconcileMountedRangeForNativeScroll,
    scheduleProgrammaticViewportSync,
    scheduleIdleMountedRangeCompaction,
    scrollContainerRef,
    sessionId,
    syncViewportFromScrollNode,
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
      const shouldKeepBottom =
        isActive && node
          ? !isDetachedFromBottomRef.current &&
            (shouldKeepBottomAfterLayoutRef.current || isScrollContainerNearBottom(node))
          : false;
      if (shouldKeepBottom) {
        shouldKeepBottomAfterLayoutRef.current = true;
      }

      const timeSinceUserScroll = performance.now() - lastUserScrollInputTimeRef.current;
      const inUserScrollCooldown = timeSinceUserScroll < USER_SCROLL_ADJUSTMENT_COOLDOWN_MS;
      if (inUserScrollCooldown) {
        scheduleDeferredLayoutVersion(
          USER_SCROLL_ADJUSTMENT_COOLDOWN_MS - timeSinceUserScroll,
        );
        return;
      }

      bumpLayoutVersion();
      if (node && shouldKeepBottom) {
        const target = Math.max(node.scrollHeight - node.clientHeight, 0);
        writeScrollTopAndSyncViewport(node, target);
      }
    },
    [
      bumpLayoutVersion,
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
      className={`virtualized-message-list${isMeasuringPostActivation ? " is-measuring-post-activation" : ""}`}
    >
      {topSpacerHeight > 0 ? (
        <div className="virtualized-message-spacer" style={{ height: topSpacerHeight }} />
      ) : null}
      {mountedPages.map((page) => (
        <MeasuredPageBand
          key={page.key}
          isActive={isActive}
          page={page}
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
      slotNodes.forEach((slotNode, index) => {
        totalHeight += Math.max(slotNode.getBoundingClientRect().height, 0);
        if (index < slotNodes.length - 1) {
          totalHeight += VIRTUALIZED_MESSAGE_GAP_PX;
        }
      });
      if (page.hasTrailingGap) {
        totalHeight += VIRTUALIZED_MESSAGE_GAP_PX;
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
  }, [isActive, onHeightChange, page.hasTrailingGap, page.key, page.messages]);

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
              {renderMessageCard(
                message,
                true,
                onApprovalDecision,
                onUserInputSubmit,
                onMcpElicitationSubmit,
                onCodexAppRequestSubmit,
              )}
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
